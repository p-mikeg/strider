//! A reference interpreter over the IR, for running a raw and an optimised
//! graph of one function against each other.
//!
//! A CONSISTENCY oracle: both graphs run here, so a misconception it shares
//! with a pass is invisible. It is written apart from `strider-opt`'s constant
//! folder, and from `strider_ir::eval` which that folder calls, so the two do
//! not share one: every integer operation below is computed here, over
//! `strider_ir::eval`'s width helpers alone.
//!
//! It runs integer arithmetic, compares, casts, phis, RAM and REGISTER-space
//! loads and stores, and `If` / `Switch` / `Region` control including loops; a
//! run that outlasts [`Env::step_budget`] is [`Outcome::Diverge`]. Floats,
//! `CallOther`, `IndirectBranch` and other spaces are [`Unsupported`].
//! Integers run at the declared width, the two past the `u128` carrier
//! included.
//!
//! A `Call` is an opaque callee. Call `k` of a run (from 1) puts
//! `hash(seed, k, target, varnode)` in each register it clobbers and leaves
//! every byte outside the ROM reading `hash(seed, k, address)`, unless its
//! convention preserves memory. No pass may forward a load across a call
//! without an assumption, so a forward shows up as a divergence. Each call's
//! target, arguments and the bytes written before it are recorded.
//!
//! The REGISTER space is a byte array of its own: the ROM does not back it,
//! an unwritten byte is noise, and a call that clobbers memory clobbers it
//! too. Only RAM bytes are reported in `writes`.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::rc::Rc;

use cranelift_entity::SecondaryMap;
use strider_ir::eval;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    ExtendOp, Function, IRViewer, IRWalker, IntBinaryOp, IntCmpOp, IntUnaryOp, ReadOnlyMemory,
    ValueType,
};
use strider_ir::{IBig, UBig};
use strider_target::Endianness;

/// An entry register, for [`Env::entry`] to pick its value.
#[derive(Clone, Copy, Debug)]
pub struct EntryVar {
    /// Its `InitialVnId` index.
    pub index: usize,
    pub vn: rsleigh::Vn,
    pub ty: ValueType,
}

/// What a run starts from.
pub struct Env<'a> {
    /// Each entry register's value; the interpreter masks it to the width.
    pub entry: &'a dyn Fn(EntryVar) -> UBig,
    /// What an unwritten byte reads, before and after any call: a callee
    /// cannot write read-only memory.
    pub rom: Option<&'a dyn ReadOnlyMemory>,
    /// Varies the noise unwritten bytes and callee results are drawn from.
    pub seed: u64,
    /// Control nodes a run may pass before it is [`Outcome::Diverge`].
    pub step_budget: usize,
    /// Addresses left out of every memory snapshot: the function's own frame,
    /// whose stores may die with it.
    pub private: Option<RangeInclusive<u64>>,
}

/// Why a run cannot go on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported(pub String);

/// A returned value or a call argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    Known(UBig),
    /// A division by zero, `INT_MIN / -1`, or a value computed from one. Any
    /// value refines it.
    Undefined,
    /// A value the interpreter cannot produce.
    Unsupported(String),
}

impl Slot {
    fn of(r: Eval<Option<UBig>>) -> Self {
        match r {
            Ok(Some(x)) => Self::Known(x),
            Ok(None) => Self::Undefined,
            Err(Unsupported(why)) => Self::Unsupported(why),
        }
    }

    /// Whether `opt`, from the optimised run, may stand for this raw slot;
    /// `None` when this slot is `Unsupported` and so checks nothing.
    #[must_use]
    pub fn admits(&self, opt: &Self) -> Option<bool> {
        match self {
            Self::Known(x) => Some(matches!(opt, Self::Known(y) if y == x)),
            Self::Undefined => Some(true),
            Self::Unsupported(_) => None,
        }
    }
}

/// A call as its callee saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    /// The callee's address, the low 128 bits of the target value.
    pub target: u128,
    pub args: Vec<Slot>,
    /// Bytes written since the previous call that clobbers memory, outside
    /// [`Env::private`].
    pub writes: BTreeMap<u64, u8>,
}

/// How a run ended. `writes` holds the bytes written since the last call that
/// clobbers memory, outside [`Env::private`]; earlier ones are in that call's
/// record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Return {
        /// One per `Return` slot.
        values: Vec<Slot>,
        writes: BTreeMap<u64, u8>,
        calls: Vec<CallRecord>,
    },
    Unreachable {
        writes: BTreeMap<u64, u8>,
        calls: Vec<CallRecord>,
    },
    Diverge(Divergence),
}

/// Why a run is [`Outcome::Diverge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Divergence {
    /// The run outlasted [`Env::step_budget`].
    StepBudget,
    /// An `If` or `Switch` tested an undefined value.
    UndefinedBranch,
}

type Eval<T> = Result<T, Unsupported>;

fn unsupported<T>(why: impl Into<String>) -> Eval<T> {
    Err(Unsupported(why.into()))
}

/// Splitmix64.
#[must_use]
pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn noise(seed: u64, a: u64, b: u64) -> u64 {
    mix(seed ^ mix(a ^ mix(b)))
}

/// A node kind's name without its payload.
fn label(kind: &NodeKind) -> String {
    let text = format!("{kind:?}");
    text.split(['(', ' ']).next().unwrap_or_default().to_owned()
}

/// Runs `f` from its entry.
///
/// # Errors
///
/// [`Unsupported`] when control or memory reaches something the interpreter
/// does not model.
pub fn run(f: &Function, env: &Env<'_>) -> Result<Outcome, Unsupported> {
    run_observed(f, env, &mut |_, _, _| {})
}

/// [`run`], handing `observe` each operand of a compare an `If` tests, the
/// condition itself, and each returned value, with the control node it is
/// seen at.
///
/// # Errors
///
/// As [`run`].
pub fn run_observed(
    f: &Function,
    env: &Env<'_>,
    observe: &mut dyn FnMut(NodeId, ValueId, &UBig),
) -> Result<Outcome, Unsupported> {
    let mut m = Machine::new(f, env);
    let mut ctrl = f.node_outputs(f.entry())[0];
    for _ in 0..env.step_budget {
        let (node, slot) = m.sole_use(ctrl)?;
        match *f.node_kind(node) {
            NodeKind::Region => {
                m.enter(node, slot);
                ctrl = f.node_outputs(node)[NodeKind::CTRL_OUT_SLOT];
            }
            NodeKind::If => {
                let cond = f.if_cond(node);
                let test = f.producer(cond);
                if matches!(f.node_kind(test), NodeKind::IntCmpOp(_)) {
                    for operand in f.node_inputs(test) {
                        if let Some(v) = m.int(operand)? {
                            observe(node, operand, &v);
                        }
                    }
                }
                let Some(c) = m.int(cond)? else {
                    return Ok(Outcome::Diverge(Divergence::UndefinedBranch));
                };
                observe(node, cond, &c);
                // Output 0 is the taken arm.
                ctrl = f.node_outputs(node)[usize::from(c == UBig::ZERO)];
            }
            NodeKind::Switch(_) => {
                let Some(selector) = m.int(f.node_inputs(node)[NodeKind::SWITCH_SELECTOR_SLOT])?
                else {
                    return Ok(Outcome::Diverge(Divergence::UndefinedBranch));
                };
                ctrl = f.node_outputs(node)[m.switch_arm(node, &selector)?];
            }
            NodeKind::Call { .. } => {
                m.call(node)?;
                ctrl = f.node_outputs(node)[NodeKind::CTRL_OUT_SLOT];
            }
            NodeKind::Return => return m.ret(node, observe),
            NodeKind::Unreachable => return m.trap(node),
            other => return unsupported(format!("{} on the control path", label(&other))),
        }
    }
    Ok(Outcome::Diverge(Divergence::StepBudget))
}

/// Memory at one point of a run: the bytes written since the last call that
/// clobbers memory, over what that call, or the entry state, left.
#[derive(Clone, Debug, Default)]
struct Mem {
    written: BTreeMap<u64, u8>,
    /// The REGISTER space's bytes, likewise.
    register: BTreeMap<u64, u8>,
    /// Calls before this point; 0 is the entry state.
    calls: u64,
}

struct Machine<'a, 'e> {
    f: &'a Function,
    env: &'a Env<'e>,
    live: strider_ir::walk::NodeIdSet,
    /// Values that change as control moves: phis, memory phis, call outputs.
    now: SecondaryMap<ValueId, Option<Eval<Option<UBig>>>>,
    mem_now: SecondaryMap<ValueId, Option<Eval<Rc<Mem>>>>,
    /// Every other value, over the current `now` and `mem_now`.
    ints: SecondaryMap<ValueId, Option<Option<UBig>>>,
    mems: SecondaryMap<ValueId, Option<Rc<Mem>>>,
    calls: Vec<CallRecord>,
}

impl<'a, 'e> Machine<'a, 'e> {
    fn new(f: &'a Function, env: &'a Env<'e>) -> Self {
        Self {
            f,
            env,
            live: f.walk().collect(),
            now: SecondaryMap::new(),
            mem_now: SecondaryMap::new(),
            ints: SecondaryMap::new(),
            mems: SecondaryMap::new(),
            calls: Vec::new(),
        }
    }

    /// The one live node `ctrl` flows into, and the input slot it lands in.
    fn sole_use(&self, ctrl: ValueId) -> Eval<(NodeId, usize)> {
        let mut uses = self
            .f
            .value_uses(ctrl)
            .filter(|&(node, _)| self.live.contains(node));
        match (uses.next(), uses.next()) {
            (Some((node, slot)), None) => Ok((node, slot as usize)),
            (None, _) => unsupported("a control value with no live consumer"),
            (Some(_), Some(_)) => unsupported("a control value with several live consumers"),
        }
    }

    /// Enters `region` through predecessor `slot`: every phi takes its input
    /// for that edge, all read before any is written.
    fn enter(&mut self, region: NodeId, slot: usize) {
        let f = self.f;
        let token = f.node_outputs(region)[NodeKind::REGION_PHI_OUT_SLOT];
        let phis: Vec<NodeId> = f
            .value_uses(token)
            .map(|(node, _)| node)
            .filter(|&node| self.live.contains(node))
            .collect();
        let mut ints = Vec::new();
        let mut mems = Vec::new();
        for phi in phis {
            let input = f.node_inputs(phi)[f.node_kind(phi).input_head_len() + slot];
            let out = f.node_outputs(phi)[0];
            match f.node_kind(phi) {
                NodeKind::Phi => ints.push((out, self.int(input))),
                NodeKind::MemPhi => mems.push((out, self.mem(input))),
                other => panic!("{other:?} hangs off a region's phi token"),
            }
        }
        self.settle(ints, mems);
    }

    /// Installs new values for control-dependent outputs. Anything computed
    /// over a value that is replaced is stale.
    fn settle(
        &mut self,
        ints: Vec<(ValueId, Eval<Option<UBig>>)>,
        mems: Vec<(ValueId, Eval<Rc<Mem>>)>,
    ) {
        let mut replaced = false;
        for (v, r) in ints {
            replaced |= self.now[v].replace(r).is_some();
        }
        for (v, r) in mems {
            replaced |= self.mem_now[v].replace(r).is_some();
        }
        if replaced {
            self.ints.clear();
            self.mems.clear();
        }
    }

    /// `None` is an undefined value: a division by zero or `INT_MIN / -1`, or
    /// anything computed from one.
    fn int(&mut self, v: ValueId) -> Eval<Option<UBig>> {
        if let Some(r) = &self.now[v] {
            return r.clone();
        }
        if let Some(r) = &self.ints[v] {
            return Ok(r.clone());
        }
        let f = self.f;
        let node = f.producer(v);
        let ty = int_type(f, v)?;
        let ins: Vec<ValueId> = f.node_inputs(node).into_iter().collect();
        let r = match *f.node_kind(node) {
            NodeKind::IntConst(_) => Some(f.int_const(v).expect("an interned constant").clone()),
            NodeKind::InitialVar(id) => {
                let var = EntryVar {
                    index: id.index(),
                    vn: f.initial_vn(id),
                    ty,
                };
                Some(eval::mask(&(self.env.entry)(var), ty))
            }
            NodeKind::IntBinaryOp(op) => {
                let (a, b) = (self.int(ins[0])?, self.int(ins[1])?);
                a.zip(b).and_then(|(a, b)| binary(op, &a, &b, ty))
            }
            NodeKind::IntUnaryOp(IntUnaryOp::Neg) => {
                self.int(ins[0])?.map(|a| unsigned(-IBig::from(a), ty))
            }
            NodeKind::IntCmpOp(op) => {
                let t = int_type(f, ins[0])?;
                if int_type(f, ins[1])? != t {
                    return unsupported("a compare of mismatched widths");
                }
                let (a, b) = (self.int(ins[0])?, self.int(ins[1])?);
                a.zip(b)
                    .map(|(a, b)| UBig::from(u8::from(compare(op, &a, &b, t))))
            }
            NodeKind::Truncate => self.int(ins[0])?.map(|a| eval::mask(&a, ty)),
            NodeKind::Extend(ExtendOp::ZeroExtend) => self.int(ins[0])?,
            NodeKind::Extend(ExtendOp::SignExtend) => {
                let t = int_type(f, ins[0])?;
                self.int(ins[0])?.map(|a| unsigned(signed(&a, t), ty))
            }
            NodeKind::Popcount => self
                .int(ins[0])?
                .map(|a| eval::mask(&UBig::from(a.count_ones()), ty)),
            NodeKind::Lzcount => {
                // Over the input's bytes, as Sleigh counts: an `I1` counts 8.
                let bits = int_type(f, ins[0])?.byte_size() * 8;
                self.int(ins[0])?
                    .map(|a| eval::mask(&UBig::from(bits - significant_bits(&a)), ty))
            }
            NodeKind::Load(space) => {
                if !modelled(space) {
                    return unsupported(format!("a load from {space:?}"));
                }
                let mem = self.mem(ins[NodeKind::ACCESS_MEM_SLOT])?;
                let Some(at) = self.int(ins[NodeKind::ACCESS_ADDR_SLOT])? else {
                    return unsupported("a load from an undefined address");
                };
                Some(self.load(&mem, space, low_u64(&at), ty))
            }
            // A phi or call output not in `now` has not been produced yet.
            other => return unsupported(format!("{} as a value", label(&other))),
        };
        self.ints[v] = Some(r.clone());
        Ok(r)
    }

    fn mem(&mut self, v: ValueId) -> Eval<Rc<Mem>> {
        if let Some(r) = &self.mem_now[v] {
            return r.clone();
        }
        if let Some(m) = &self.mems[v] {
            return Ok(Rc::clone(m));
        }
        let f = self.f;
        let node = f.producer(v);
        let mem = match *f.node_kind(node) {
            NodeKind::InitialMemory => Rc::default(),
            NodeKind::Store(space) => {
                if !modelled(space) {
                    return unsupported(format!("a store to {space:?}"));
                }
                let ins: Vec<ValueId> = f.node_inputs(node).into_iter().collect();
                let prev_in = ins[NodeKind::ACCESS_MEM_SLOT];
                let prev = self.mem(prev_in)?;
                let Some(at) = self.int(ins[NodeKind::ACCESS_ADDR_SLOT])? else {
                    return unsupported("a store to an undefined address");
                };
                let data_in = ins[NodeKind::STORE_DATA_SLOT];
                let Some(data) = self.int(data_in)? else {
                    return unsupported("a store of an undefined value");
                };
                let data_ty = int_type(f, data_in)?;
                let size = data_ty.bit_width().div_ceil(8);
                // A state only this store reads leaves the cache and is
                // updated in place; one with other readers is copied.
                if f.value_has_one_use(prev_in) && self.mem_now[prev_in].is_none() {
                    self.mems[prev_in] = None;
                }
                let mut mem = Rc::unwrap_or_clone(prev);
                // Little-endian value bytes, then into address order.
                let mut bytes = eval::le_bytes(&data, data_ty)[..size].to_vec();
                if f.byte_order().of(space) == Endianness::Big {
                    bytes.reverse();
                }
                let space_bytes = if space == rsleigh::VnSpace::REGISTER {
                    &mut mem.register
                } else {
                    &mut mem.written
                };
                let at = low_u64(&at);
                for (i, byte) in bytes.into_iter().enumerate() {
                    space_bytes.insert(at.wrapping_add(i as u64), byte);
                }
                Rc::new(mem)
            }
            other => return unsupported(format!("{} as a memory state", label(&other))),
        };
        self.mems[v] = Some(Rc::clone(&mem));
        Ok(mem)
    }

    fn load(&self, mem: &Mem, space: rsleigh::VnSpace, at: u64, ty: ValueType) -> UBig {
        let size = ty.bit_width().div_ceil(8);
        let mut bytes: Vec<u8> = (0..size as u64)
            .map(|i| {
                let addr = at.wrapping_add(i);
                if space == rsleigh::VnSpace::REGISTER {
                    self.register_byte(mem, addr)
                } else {
                    self.byte(mem, addr)
                }
            })
            .collect();
        if self.f.byte_order().of(space) == Endianness::Big {
            bytes.reverse();
        }
        eval::mask(&UBig::from_le_bytes(&bytes), ty)
    }

    fn byte(&self, mem: &Mem, addr: u64) -> u8 {
        if let Some(&byte) = mem.written.get(&addr) {
            return byte;
        }
        if let Some(rom) = self.env.rom {
            let mut buf = [0u8];
            if rom.read(addr, &mut buf).is_ok() {
                return buf[0];
            }
        }
        noise(self.env.seed, addr, mem.calls) as u8
    }

    fn register_byte(&self, mem: &Mem, addr: u64) -> u8 {
        if let Some(&byte) = mem.register.get(&addr) {
            return byte;
        }
        noise(!self.env.seed, addr, mem.calls) as u8
    }

    fn snapshot(&self, mem: &Mem) -> BTreeMap<u64, u8> {
        mem.written
            .iter()
            .filter(|(addr, _)| {
                !self
                    .env
                    .private
                    .as_ref()
                    .is_some_and(|frame| frame.contains(*addr))
            })
            .map(|(&addr, &byte)| (addr, byte))
            .collect()
    }

    fn switch_arm(&self, node: NodeId, selector: &UBig) -> Eval<usize> {
        // An interworking dispatch carries its ISA mode in the selector's low
        // bit.
        let interworking = self.f.switch_isa_mode(node).is_some();
        // `UBig` has no `Not`: clearing the low bit is a round trip through 1.
        let even = (selector.clone() >> 1usize) << 1usize;
        self.f
            .switch_targets(node)
            .iter()
            .position(|&t| UBig::from(t) == *selector || (interworking && UBig::from(t) == even))
            .map_or_else(|| unsupported("a switch selector naming no arm"), Ok)
    }

    /// Runs the opaque callee: records what it sees, then replaces memory and
    /// every register it clobbers with noise keyed by the call's number.
    fn call(&mut self, node: NodeId) -> Eval<()> {
        let f = self.f;
        // Inputs are [ctrl, mem, target, sp, args..].
        let ins: Vec<ValueId> = f.node_inputs(node).into_iter().collect();
        let Some(target) = self.int(ins[NodeKind::CALL_TARGET_SLOT])? else {
            return unsupported("a call to an undefined target");
        };
        let target = low_u128(&target);
        // The callee reads none of them, so an unsupported one does not stop
        // the run.
        let args = ins[NodeKind::Call { cc: None }.input_head_len()..]
            .iter()
            .map(|&arg| Slot::of(self.int(arg)))
            .collect();
        let before = self.mem(ins[NodeKind::CTRL_MEM_SLOT])?;
        let writes = self.snapshot(&before);
        self.calls.push(CallRecord {
            target,
            args,
            writes,
        });
        let k = self.calls.len() as u64;
        let outs = f.node_outputs(node);
        let after = if f.get_cc(node).preserves_memory {
            before
        } else {
            Rc::new(Mem {
                written: BTreeMap::new(),
                register: BTreeMap::new(),
                calls: k,
            })
        };
        let seed = self.env.seed;
        let ints = outs[f.node_kind(node).output_head_len()..]
            .iter()
            .enumerate()
            .map(|(i, &out)| {
                // Keyed by varnode, so dropping an unused output does not shift
                // the others.
                let key = f
                    .get_vn_for_value(out)
                    .map_or(i as u64, |vn| vn.addr_off ^ (u64::from(vn.size) << 48));
                let value = int_type(f, out).map(|ty| {
                    let high = u128::from(noise(seed, k, key));
                    let low = u128::from(noise(seed, k ^ (target as u64), key));
                    Some(eval::mask(&UBig::from((high << 64) | low), ty))
                });
                (out, value)
            })
            .collect();
        self.settle(ints, vec![(outs[NodeKind::CTRL_MEM_OUT_SLOT], Ok(after))]);
        Ok(())
    }

    fn ret(
        &mut self,
        node: NodeId,
        observe: &mut dyn FnMut(NodeId, ValueId, &UBig),
    ) -> Eval<Outcome> {
        let f = self.f;
        let ins: Vec<ValueId> = f.node_inputs(node).into_iter().collect();
        let head = NodeKind::Return.input_head_len();
        let mut values = Vec::with_capacity(ins.len() - head);
        for &v in &ins[head..] {
            let value = Slot::of(self.int(v));
            if let Slot::Known(x) = &value {
                observe(node, v, x);
            }
            values.push(value);
        }
        let mem = self.mem(ins[NodeKind::CTRL_MEM_SLOT])?;
        Ok(Outcome::Return {
            values,
            writes: self.snapshot(&mem),
            calls: std::mem::take(&mut self.calls),
        })
    }

    fn trap(&mut self, node: NodeId) -> Eval<Outcome> {
        // The memory input is optional.
        let writes = match self
            .f
            .node_inputs(node)
            .get(NodeKind::CTRL_MEM_SLOT)
            .copied()
        {
            Some(input) => {
                let mem = self.mem(input)?;
                self.snapshot(&mem)
            }
            None => BTreeMap::new(),
        };
        Ok(Outcome::Unreachable {
            writes,
            calls: std::mem::take(&mut self.calls),
        })
    }
}

fn modelled(space: rsleigh::VnSpace) -> bool {
    space == rsleigh::VnSpace::RAM || space == rsleigh::VnSpace::REGISTER
}

fn int_type(f: &Function, v: ValueId) -> Eval<ValueType> {
    match f.value_type_opt(v) {
        Some(ty) if ty.is_integer() => Ok(ty),
        Some(ty) => unsupported(format!("a {ty} value")),
        None => unsupported("a control, memory or token edge as a value"),
    }
}

/// An address, which is 64 bits whatever the value it was computed in.
fn low_u64(v: &UBig) -> u64 {
    v.as_words().first().copied().unwrap_or(0)
}

/// A call target: an address, kept in the carrier the record declares.
fn low_u128(v: &UBig) -> u128 {
    let w = v.as_words();
    u128::from(w.first().copied().unwrap_or(0)) | (u128::from(w.get(1).copied().unwrap_or(0)) << 64)
}

/// Bits up to and including `v`'s highest set one, 0 for zero.
fn significant_bits(v: &UBig) -> usize {
    let bytes = v.to_le_bytes();
    bytes
        .iter()
        .rposition(|&b| b != 0)
        .map_or(0, |i| i * 8 + (8 - bytes[i].leading_zeros() as usize))
}

/// `v` read as two's complement at `ty`'s width.
fn signed(v: &UBig, ty: ValueType) -> IBig {
    let m = eval::mask(v, ty);
    let half = UBig::ONE << (ty.bit_width() - 1);
    if m >= half {
        IBig::from(m) - (IBig::from(half) << 1usize)
    } else {
        IBig::from(m)
    }
}

/// `v` wrapped into `ty`'s two's-complement ring.
fn unsigned(v: IBig, ty: ValueType) -> UBig {
    v & eval::all_ones(ty)
}

fn signed_min(ty: ValueType) -> IBig {
    -(IBig::ONE << (ty.bit_width() - 1))
}

fn signed_max(ty: ValueType) -> IBig {
    (IBig::ONE << (ty.bit_width() - 1)) - IBig::ONE
}

fn binary(op: IntBinaryOp, a: &UBig, b: &UBig, ty: ValueType) -> Option<UBig> {
    let w = ty.bit_width();
    // The count keeps its own width; p-code clears the result once it reaches
    // the output's. A count no `usize` holds is past every width.
    let count = usize::try_from(b).ok().filter(|&s| s < w);
    let raw = match op {
        IntBinaryOp::Add => a + b,
        IntBinaryOp::Mul => a * b,
        IntBinaryOp::And => a & b,
        IntBinaryOp::Or => a | b,
        IntBinaryOp::Xor => a ^ b,
        IntBinaryOp::ShiftLeft => match count {
            Some(s) => a.clone() << s,
            None => UBig::ZERO,
        },
        IntBinaryOp::ShiftRight => match count {
            Some(s) => a.clone() >> s,
            None => UBig::ZERO,
        },
        IntBinaryOp::SShiftRight => {
            let sa = signed(a, ty);
            match count {
                Some(s) => unsigned(sa >> s, ty),
                None => {
                    if sa < IBig::ZERO {
                        eval::all_ones(ty)
                    } else {
                        UBig::ZERO
                    }
                }
            }
        }
        IntBinaryOp::Div => {
            if *b == UBig::ZERO {
                return None;
            }
            a / b
        }
        IntBinaryOp::Rem => {
            if *b == UBig::ZERO {
                return None;
            }
            a % b
        }
        IntBinaryOp::Sdiv | IntBinaryOp::Srem => {
            let (sa, sb) = (signed(a, ty), signed(b, ty));
            if sb == IBig::ZERO || (sa == signed_min(ty) && sb == IBig::NEG_ONE) {
                return None;
            }
            let q = if op == IntBinaryOp::Sdiv {
                sa / sb
            } else {
                sa % sb
            };
            unsigned(q, ty)
        }
    };
    Some(eval::mask(&raw, ty))
}

fn compare(op: IntCmpOp, a: &UBig, b: &UBig, ty: ValueType) -> bool {
    match op {
        IntCmpOp::Equal => a == b,
        IntCmpOp::Less => a < b,
        IntCmpOp::Sless => signed(a, ty) < signed(b, ty),
        IntCmpOp::Carry => a + b > eval::all_ones(ty),
        IntCmpOp::Scarry => {
            let sum = signed(a, ty) + signed(b, ty);
            sum < signed_min(ty) || sum > signed_max(ty)
        }
        IntCmpOp::Sborrow => {
            let diff = signed(a, ty) - signed(b, ty);
            diff < signed_min(ty) || diff > signed_max(ty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MockRom, RegisterSet, make_empty_fn, make_empty_fn_endian, reg_vn};
    use strider_ir::node::ValueKind;
    use strider_ir::{IRBuilder as _, IRBuilderExt as _};

    const SEED: u64 = 7;

    fn known(v: u64) -> Slot {
        Slot::Known(UBig::from(v))
    }

    fn env<'a>(
        entry: &'a dyn Fn(EntryVar) -> UBig,
        rom: Option<&'a dyn ReadOnlyMemory>,
    ) -> Env<'a> {
        Env {
            entry,
            rom,
            seed: SEED,
            step_budget: 200,
            private: None,
        }
    }

    fn returned(f: &Function, env: &Env<'_>) -> Vec<Slot> {
        match run(f, env).expect("the function runs") {
            Outcome::Return { values, .. } => values,
            other => panic!("ended {other:?}"),
        }
    }

    /// P-code clears a shift whose count reaches the width, and the count keeps
    /// its own width: an `I16` count of 256 on an `I8` shift is 256, not its low
    /// byte.
    #[test]
    fn a_shift_count_wider_than_its_output_is_not_truncated() {
        let f = make_empty_fn(|b| {
            let one = b.build_int_const(1u8, ValueType::int(8))?;
            let count = b.build_int_const(0x100u16, ValueType::int(16))?;
            // `build_int_binary_operation` pins both operands to the output width.
            let node = b.create_node(
                NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
                [one, count],
                [ValueKind::Typed(ValueType::int(8))],
            );
            Ok(b.function().node_outputs(node)[0])
        })
        .expect("a shift with a wider count is valid IR");
        assert_eq!(returned(&f, &env(&|_| UBig::ZERO, None)), vec![known(0)]);
    }

    /// Sleigh counts over the input's bytes, so an `I1` counts over 8 bits, as
    /// `strider_ir::eval::lzcount` does.
    #[test]
    fn lzcount_of_an_i1_counts_over_a_byte() {
        for (bit, zeros) in [(1u8, 7), (0, 8)] {
            let f = make_empty_fn(|b| {
                let v = b.build_int_const(bit, ValueType::I1)?;
                b.build_lzcount(v, ValueType::int(8))
            })
            .unwrap();
            assert_eq!(
                returned(&f, &env(&|_| UBig::ZERO, None)),
                vec![known(zeros)]
            );
        }
    }

    /// Integers run at the declared width: a value the `u128` carrier cannot
    /// hold is computed, stored, reloaded and counted, not refused.
    #[test]
    fn an_i256_runs_at_its_declared_width() {
        let i256 = ValueType::int(256);
        let x = reg_vn(0x1000, 32);
        let mut b = RegisterSet::new()
            .tracked(x)
            .build_fn_single_region()
            .unwrap();
        let v = b.read_variable(&x).unwrap();
        let bit200 = UBig::ONE << 200usize;
        let c = b.build_int_const(bit200.clone(), i256).unwrap();
        let sum = b
            .build_int_binary_operation(v, c, IntBinaryOp::Add, i256)
            .unwrap();
        let eight = b.build_int_const(8u32, i256).unwrap();
        let arith = b
            .build_int_binary_operation(v, eight, IntBinaryOp::SShiftRight, i256)
            .unwrap();
        let lz = b.build_lzcount(sum, ValueType::int(32)).unwrap();
        let at = b.build_int_const(0x5000u64, ValueType::int(64)).unwrap();
        b.build_store(at, sum, rsleigh::VnSpace::RAM).unwrap();
        let back = b.build_load(at, rsleigh::VnSpace::RAM, i256).unwrap();
        b.build_return(None, &[sum, arith, back, lz]).unwrap();
        let f = b.build().unwrap();
        // The top bit alone, which no `u128` holds.
        let top = UBig::ONE << 255usize;
        let want = top.clone() + bit200;
        // -2^255 >> 8 is -2^247: every bit from 247 up.
        let shifted = strider_ir::eval::complement(&((UBig::ONE << 247usize) - UBig::ONE), i256);
        assert_eq!(
            returned(&f, &env(&|_| top.clone(), None)),
            vec![
                Slot::Known(want.clone()),
                Slot::Known(shifted),
                Slot::Known(want),
                known(0),
            ]
        );
    }

    /// `i = start; do i += step while i != stop; return i`.
    fn counting_loop(start: u32, step: u32, stop: u32) -> Function {
        let i = reg_vn(0x1000, 4);
        let mut b = RegisterSet::new().tracked(i).build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let head = b.create_region_all().unwrap();
        let exit = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        let first = b.build_int_const(start, ValueType::int(32)).unwrap();
        b.write_variable(&i, first).unwrap();
        b.build_branch(head).unwrap();
        b.set_region(head);
        let cur = b.read_variable(&i).unwrap();
        let step = b.build_int_const(step, ValueType::int(32)).unwrap();
        let next = b
            .build_int_binary_operation(cur, step, IntBinaryOp::Add, ValueType::int(32))
            .unwrap();
        b.write_variable(&i, next).unwrap();
        let stop = b.build_int_const(stop, ValueType::int(32)).unwrap();
        let done = b
            .build_int_cmp_operation(next, stop, IntCmpOp::Equal, ValueType::int(32))
            .unwrap();
        b.build_if(done, exit, head).unwrap();
        b.set_region(exit);
        let out = b.read_variable(&i).unwrap();
        b.build_return(None, &[out]).unwrap();
        b.build().unwrap()
    }

    #[test]
    fn a_loop_runs_until_it_exits() {
        assert_eq!(
            returned(&counting_loop(0, 1, 5), &env(&|_| UBig::ZERO, None)),
            vec![known(5)]
        );
    }

    /// Odd values never meet zero inside the budget.
    #[test]
    fn a_loop_past_the_step_budget_diverges() {
        assert_eq!(
            run(&counting_loop(1, 2, 0), &env(&|_| UBig::ZERO, None)),
            Ok(Outcome::Diverge(Divergence::StepBudget))
        );
    }

    #[test]
    fn memory_follows_the_function_endianness() {
        for (endianness, first) in [(Endianness::Little, 0x44), (Endianness::Big, 0x11)] {
            let f = make_empty_fn_endian(endianness, |b| {
                let at = b.build_int_const(0x5000u64, ValueType::int(64))?;
                let word = b.build_int_const(0x1122_3344u32, ValueType::int(32))?;
                b.build_store(at, word, rsleigh::VnSpace::RAM)?;
                b.build_load(at, rsleigh::VnSpace::RAM, ValueType::int(8))
            })
            .unwrap();
            assert_eq!(
                returned(&f, &env(&|_| UBig::ZERO, None)),
                vec![known(first)],
                "{endianness:?}"
            );
        }
    }

    /// A RAM store at a register-space address leaves the register space alone.
    #[test]
    fn the_register_space_is_a_byte_array_of_its_own() {
        let f = make_empty_fn_endian(Endianness::Little, |b| {
            let at = b.build_int_const(0x300u64, ValueType::int(64))?;
            let word = b.build_int_const(0x1122_3344u32, ValueType::int(32))?;
            let other = b.build_int_const(0x5566_7788u32, ValueType::int(32))?;
            b.build_store(at, word, rsleigh::VnSpace::REGISTER)?;
            b.build_store(at, other, rsleigh::VnSpace::RAM)?;
            b.build_load(at, rsleigh::VnSpace::REGISTER, ValueType::int(16))
        })
        .unwrap();
        assert_eq!(
            returned(&f, &env(&|_| UBig::ZERO, None)),
            vec![known(0x3344)]
        );
    }

    #[test]
    fn an_unwritten_byte_reads_the_rom() {
        let rom = MockRom::raw_bytes(0x1000, vec![1, 2, 3, 4]);
        let f = make_empty_fn(|b| {
            let at = b.build_int_const(0x1000u64, ValueType::int(64))?;
            b.build_load(at, rsleigh::VnSpace::RAM, ValueType::int(32))
        })
        .unwrap();
        assert_eq!(
            returned(&f, &env(&|_| UBig::ZERO, Some(&rom))),
            vec![known(0x0403_0201)]
        );
    }

    /// The callee sees the byte stored before it and replaces it with noise;
    /// the ROM byte survives.
    #[test]
    fn a_call_clobbers_memory_but_not_the_rom() {
        let rom = MockRom::raw_bytes(0x1000, vec![0x5a]);
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        let heap = b.build_int_const(0x5000u64, ValueType::int(64)).unwrap();
        let byte = b.build_int_const(0xabu8, ValueType::int(8)).unwrap();
        b.build_store(heap, byte, rsleigh::VnSpace::RAM).unwrap();
        let target = b.build_int_const(0x9000u64, ValueType::int(64)).unwrap();
        b.build_call_cc(target, None).unwrap();
        let rom_at = b.build_int_const(0x1000u64, ValueType::int(64)).unwrap();
        let kept = b
            .build_load(rom_at, rsleigh::VnSpace::RAM, ValueType::int(8))
            .unwrap();
        let lost = b
            .build_load(heap, rsleigh::VnSpace::RAM, ValueType::int(8))
            .unwrap();
        b.build_return(None, &[kept, lost]).unwrap();
        let f = b.build().unwrap();
        match run(&f, &env(&|_| UBig::ZERO, Some(&rom))).expect("the function runs") {
            Outcome::Return { values, calls, .. } => {
                let noise_byte = u64::from(noise(SEED, 0x5000, 1) as u8);
                assert_eq!(values, vec![known(0x5a), known(noise_byte)]);
                assert_eq!(
                    calls,
                    vec![CallRecord {
                        target: 0x9000,
                        args: Vec::new(),
                        writes: BTreeMap::from([(0x5000, 0xab)]),
                    }]
                );
            }
            other => panic!("ended {other:?}"),
        }
    }

    fn two_way_switch() -> Function {
        let sel = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new().tracked(sel).build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let low = b.create_region_all().unwrap();
        let high = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        let s = b.read_variable(&sel).unwrap();
        b.build_switch_with_mode(s, None, &[(low, 0x100), (high, 0x200)])
            .unwrap();
        for (region, value) in [(low, 1u64), (high, 2)] {
            b.set_region(region);
            let v = b.build_int_const(value, ValueType::int(64)).unwrap();
            b.build_return(None, &[v]).unwrap();
        }
        b.build().unwrap()
    }

    #[test]
    fn a_switch_takes_the_arm_its_selector_names() {
        assert_eq!(
            returned(&two_way_switch(), &env(&|_| UBig::from(0x200u64), None)),
            vec![known(2)]
        );
        assert_eq!(
            run(&two_way_switch(), &env(&|_| UBig::from(0x300u64), None)),
            Err(Unsupported("a switch selector naming no arm".into()))
        );
    }

    #[test]
    fn a_raw_slot_admits_only_what_refines_it() {
        assert_eq!(known(1).admits(&known(1)), Some(true));
        assert_eq!(known(1).admits(&Slot::Undefined), Some(false));
        assert_eq!(Slot::Undefined.admits(&known(7)), Some(true));
        assert_eq!(Slot::Unsupported(String::new()).admits(&known(7)), None);
    }

    #[test]
    fn a_return_slot_tells_undefined_from_unsupported() {
        let x = reg_vn(0x1000, 4);
        let mut b = RegisterSet::new()
            .tracked(x)
            .build_fn_single_region()
            .unwrap();
        let v = b.read_variable(&x).unwrap();
        let zero = b.build_int_const(0u32, ValueType::int(32)).unwrap();
        let undefined = b
            .build_int_binary_operation(v, zero, IntBinaryOp::Div, ValueType::int(32))
            .unwrap();
        let float = b.build_float_const(0, ValueType::float(32));
        b.build_return(None, &[undefined, float]).unwrap();
        let values = returned(&b.build().unwrap(), &env(&|_| UBig::from(5u64), None));
        assert_eq!(values[0], Slot::Undefined);
        assert!(matches!(values[1], Slot::Unsupported(_)), "{values:?}");
    }

    #[test]
    fn private_bytes_are_left_out_of_the_writes() {
        let f = make_empty_fn(|b| {
            for at in [0x5000u64, 0x7000] {
                let at = b.build_int_const(at, ValueType::int(64))?;
                let byte = b.build_int_const(0xabu8, ValueType::int(8))?;
                b.build_store(at, byte, rsleigh::VnSpace::RAM)?;
            }
            b.build_int_const(0u8, ValueType::int(8))
        })
        .unwrap();
        let env = Env {
            private: Some(0x7000..=0x7fff),
            ..env(&|_| UBig::ZERO, None)
        };
        match run(&f, &env).expect("the function runs") {
            Outcome::Return { writes, .. } => {
                assert_eq!(writes, BTreeMap::from([(0x5000, 0xab)]));
            }
            other => panic!("ended {other:?}"),
        }
    }

    #[test]
    fn a_memory_preserving_call_keeps_memory() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        let heap = b.build_int_const(0x5000u64, ValueType::int(64)).unwrap();
        let byte = b.build_int_const(0xabu8, ValueType::int(8)).unwrap();
        b.build_store(heap, byte, rsleigh::VnSpace::RAM).unwrap();
        let cc = strider_target::BuiltCallingConvention {
            preserves_memory: true,
            ..b.function().default_cc().clone()
        };
        let target = b.build_int_const(0x9000u64, ValueType::int(64)).unwrap();
        b.build_call_cc(target, Some(&cc)).unwrap();
        let kept = b
            .build_load(heap, rsleigh::VnSpace::RAM, ValueType::int(8))
            .unwrap();
        b.build_return(None, &[kept]).unwrap();
        let f = b.build().unwrap();
        let written = BTreeMap::from([(0x5000, 0xab)]);
        assert_eq!(
            run(&f, &env(&|_| UBig::ZERO, None)),
            Ok(Outcome::Return {
                values: vec![known(0xab)],
                writes: written.clone(),
                calls: vec![CallRecord {
                    target: 0x9000,
                    args: Vec::new(),
                    writes: written,
                }],
            })
        );
    }

    /// `a, b = b, a` three times: each phi reads its input before any is
    /// written.
    #[test]
    fn phis_on_one_edge_take_their_inputs_together() {
        let (a, b_vn, i) = (reg_vn(0x1000, 4), reg_vn(0x1010, 4), reg_vn(0x1020, 4));
        let mut b = RegisterSet::new()
            .tracked(a)
            .tracked(b_vn)
            .tracked(i)
            .build_fn()
            .unwrap();
        let entry = b.create_region_all().unwrap();
        let head = b.create_region_all().unwrap();
        let exit = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        for (vn, value) in [(a, 1u32), (b_vn, 2), (i, 0)] {
            let c = b.build_int_const(value, ValueType::int(32)).unwrap();
            b.write_variable(&vn, c).unwrap();
        }
        b.build_branch(head).unwrap();
        b.set_region(head);
        let (cur_a, cur_b) = (
            b.read_variable(&a).unwrap(),
            b.read_variable(&b_vn).unwrap(),
        );
        b.write_variable(&a, cur_b).unwrap();
        b.write_variable(&b_vn, cur_a).unwrap();
        let cur_i = b.read_variable(&i).unwrap();
        let one = b.build_int_const(1u32, ValueType::int(32)).unwrap();
        let next = b
            .build_int_binary_operation(cur_i, one, IntBinaryOp::Add, ValueType::int(32))
            .unwrap();
        b.write_variable(&i, next).unwrap();
        let three = b.build_int_const(3u32, ValueType::int(32)).unwrap();
        let done = b
            .build_int_cmp_operation(next, three, IntCmpOp::Equal, ValueType::int(32))
            .unwrap();
        b.build_if(done, exit, head).unwrap();
        b.set_region(exit);
        let out = [
            b.read_variable(&a).unwrap(),
            b.read_variable(&b_vn).unwrap(),
        ];
        b.build_return(None, &out).unwrap();
        let f = b.build().unwrap();
        assert_eq!(
            returned(&f, &env(&|_| UBig::ZERO, None)),
            vec![known(2), known(1)]
        );
    }

    /// A trap reports what its optional memory input wrote after the last
    /// call, and that call.
    #[test]
    fn a_trap_reports_its_writes_and_calls() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        let heap = b.build_int_const(0x5000u64, ValueType::int(64)).unwrap();
        let byte = b.build_int_const(0xabu8, ValueType::int(8)).unwrap();
        b.build_store(heap, byte, rsleigh::VnSpace::RAM).unwrap();
        let target = b.build_int_const(0x9000u64, ValueType::int(64)).unwrap();
        b.build_call_cc(target, None).unwrap();
        b.build_unreachable().unwrap();
        let mut f = b.build().unwrap();
        let only = |f: &Function, kind: fn(&NodeKind) -> bool| f.walk_kind(kind).next().unwrap();
        let trap = only(&f, |k| matches!(k, NodeKind::Unreachable));
        let call = only(&f, |k| matches!(k, NodeKind::Call { .. }));
        // The builder's trap takes no memory; give it a store after the call.
        let mut edit = strider_ir::EditFunction::new(&mut f);
        let after_call = edit.function().node_outputs(call)[1];
        let at = edit.build_int_const(0x5001u64, ValueType::int(64)).unwrap();
        let byte = edit.build_int_const(0xcdu8, ValueType::int(8)).unwrap();
        let store = edit.create_node(
            NodeKind::Store(rsleigh::VnSpace::RAM),
            [after_call, at, byte],
            [ValueKind::Memory],
        );
        let stored = edit.function().node_outputs(store)[0];
        edit.add_node_input(trap, stored);
        assert_eq!(
            run(&f, &env(&|_| UBig::ZERO, None)),
            Ok(Outcome::Unreachable {
                writes: BTreeMap::from([(0x5001, 0xcd)]),
                calls: vec![CallRecord {
                    target: 0x9000,
                    args: Vec::new(),
                    writes: BTreeMap::from([(0x5000, 0xab)]),
                }],
            })
        );
    }

    #[test]
    fn observe_sees_tested_operands_the_condition_and_returned_values() {
        let x = reg_vn(0x1000, 4);
        // if x < 10 { return x } else { return 0 }
        let mut b = RegisterSet::new().tracked(x).build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let then = b.create_region_all().unwrap();
        let other = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        let v = b.read_variable(&x).unwrap();
        let ten = b.build_int_const(10u32, ValueType::int(32)).unwrap();
        let cond = b
            .build_int_cmp_operation(v, ten, IntCmpOp::Less, ValueType::int(32))
            .unwrap();
        b.build_if(cond, then, other).unwrap();
        b.set_region(then);
        b.build_return(None, &[v]).unwrap();
        b.set_region(other);
        let zero = b.build_int_const(0u32, ValueType::int(32)).unwrap();
        b.build_return(None, &[zero]).unwrap();
        let f = b.build().unwrap();
        let mut seen = Vec::new();
        run_observed(
            &f,
            &env(&|_| UBig::from(3u64), None),
            &mut |at, _, value| {
                seen.push((label(f.node_kind(at)), value.clone()));
            },
        )
        .expect("the function runs");
        let expected = [("If", 3u64), ("If", 10), ("If", 1), ("Return", 3)];
        assert_eq!(
            seen,
            expected.map(|(kind, value)| (kind.to_owned(), UBig::from(value)))
        );
    }

    #[test]
    fn an_undefined_branch_condition_diverges() {
        let x = reg_vn(0x1000, 4);
        let mut b = RegisterSet::new().tracked(x).build_fn().unwrap();
        let entry = b.create_region_all().unwrap();
        let then = b.create_region_all().unwrap();
        let other = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        let v = b.read_variable(&x).unwrap();
        let zero = b.build_int_const(0u32, ValueType::int(32)).unwrap();
        let undefined = b
            .build_int_binary_operation(v, zero, IntBinaryOp::Div, ValueType::int(32))
            .unwrap();
        let cond = b
            .build_int_cmp_operation(undefined, zero, IntCmpOp::Equal, ValueType::int(32))
            .unwrap();
        b.build_if(cond, then, other).unwrap();
        for region in [then, other] {
            b.set_region(region);
            b.build_return(None, &[zero]).unwrap();
        }
        let f = b.build().unwrap();
        assert_eq!(
            run(&f, &env(&|_| UBig::from(5u64), None)),
            Ok(Outcome::Diverge(Divergence::UndefinedBranch))
        );
    }
}
