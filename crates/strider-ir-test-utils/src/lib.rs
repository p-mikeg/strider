//! Shared mock-IR helpers used by tests across the workspace.
//!
//! Every helper sets [`SENTINEL_LIFT_ADDR`] as the builder's lift address for
//! the duration of its closure, so nodes built through the `build_*` API
//! inherit a non-empty asm-fingerprint and mock graphs satisfy the always-on
//! fingerprint check without per-node stamping.
//!
//! Its own crate so consumers can dev-depend on it without strider-ir carrying
//! a feature flag.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{
    Function, FunctionBuilder, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntUnaryOp,
    ReadOnlyMemory, Result, Value, ValueType,
};

/// Node-kind assertion vocabulary over entry-reachable nodes. Test-only, so it
/// lives here rather than on the production [`IRWalker`].
pub trait IrWalkerEx: IRWalker {
    fn count_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> usize {
        self.walk_kind(pred).count()
    }

    fn has_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> bool {
        self.walk_kind(pred).next().is_some()
    }
}

impl<T: IRWalker + ?Sized> IrWalkerEx for T {}

/// Builder shorthands for shapes that aren't primitive in the IR. Test-only.
pub trait IrBuilderEx: IRBuilderExt {
    /// `Add(lhs, Neg(rhs))`. There is no `IntBinaryOp::Sub`; the lifter lowers
    /// `IntSub` to this, and so does this helper.
    ///
    /// # Errors
    ///
    /// If either operand is not a value edge.
    fn build_sub_as_add_neg(
        &mut self,
        lhs_id: ValueId,
        rhs_id: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let neg_rhs = self.build_int_unary_operation(rhs_id, IntUnaryOp::Neg, output_type)?;
        self.build_int_binary_operation(lhs_id, neg_rhs, IntBinaryOp::Add, output_type)
    }
}

impl<T: IRBuilderExt + ?Sized> IrBuilderEx for T {}

/// Deliberately unlike any real machine address, so a sentinel-stamped node
/// leaking into a graph dump or IR snapshot is unmistakable.
pub const SENTINEL_LIFT_ADDR: u64 = 0xDEAD_BEEF_0000_0001;

/// Fallback SP when a fixture declares no `stack_vn`. `build_call` reads the
/// SP from the variable table and errors if it is absent, so a `Call`-building
/// fixture always needs one; the high offset dodges common test registers.
const DEFAULT_TEST_SP: rsleigh::Vn = rsleigh::Vn {
    addr_off: 0x7000,
    addr_space: rsleigh::VnSpace::REGISTER,
    size: 8,
};

/// Fluent description of a mock function's register convention.
///
/// [`build_fn`](RegisterSet::build_fn) synthesises a
/// [`strider_target::BuiltCallingConvention`] from the declared lists and
/// stamps the sentinel lift address, but creates NO region; use
/// [`build_fn_single_region`](RegisterSet::build_fn_single_region) for that.
#[derive(Default, Clone)]
pub struct RegisterSet {
    tracked: Vec<rsleigh::Vn>,
    arg_passing: Vec<rsleigh::Vn>,
    callee_saved: Vec<rsleigh::Vn>,
    ret_val: Vec<rsleigh::Vn>,
    sp: Option<rsleigh::Vn>,
    ret_stack_pop: i64,
    /// Baked into the built function's `default_cc`, which is what the
    /// SP-aware arg passes read.
    stack_args: Option<strider_target::StackArgs>,
    /// `None` defaults to little-endian.
    endianness: Option<strider_target::Endianness>,
    /// `None` (the default) means no architectural link register, i.e. the
    /// x86 / x86_64 case. Set it for link-register ISAs (ARM, AArch64, ...).
    link_register: Option<rsleigh::Vn>,
}

impl RegisterSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tracked(mut self, vn: rsleigh::Vn) -> Self {
        self.tracked.push(vn);
        self
    }

    pub fn arg(mut self, vn: rsleigh::Vn) -> Self {
        self.arg_passing.push(vn);
        self
    }

    pub fn callee_saved(mut self, vn: rsleigh::Vn) -> Self {
        self.callee_saved.push(vn);
        self
    }

    pub fn ret(mut self, vn: rsleigh::Vn) -> Self {
        self.ret_val.push(vn);
        self
    }

    pub fn stack_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.sp = Some(vn);
        self
    }

    pub fn ret_stack_pop(mut self, n: i64) -> Self {
        self.ret_stack_pop = n;
        self
    }

    pub fn stack_args(mut self, stack_args: Option<strider_target::StackArgs>) -> Self {
        self.stack_args = stack_args;
        self
    }

    pub fn endianness(mut self, e: strider_target::Endianness) -> Self {
        self.endianness = Some(e);
        self
    }

    pub fn link_register(mut self, vn: rsleigh::Vn) -> Self {
        self.link_register = Some(vn);
        self
    }

    /// Stamps [`SENTINEL_LIFT_ADDR`] but creates no region; drive
    /// `create_region` / `set_entry_region` / `set_region` yourself.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new`.
    pub fn build_fn(self) -> Result<FunctionBuilder> {
        // `FunctionBuilder::new` seeds the declared arg/ret regs but not the
        // stack vn (in production the lifter owns that), so track it here.
        let stack_vn = self.sp.unwrap_or(DEFAULT_TEST_SP);
        let mut tracked = self.tracked;
        if !tracked.contains(&stack_vn) {
            tracked.push(stack_vn);
        }
        // Struct-literal construction rather than `try_new` deliberately skips
        // ABI-disjointness validation, so fixtures may declare overlapping or
        // otherwise degenerate register sets.
        let cc = strider_target::BuiltCallingConvention {
            arg_passing_regs: self.arg_passing,
            callee_saved_regs: self.callee_saved,
            ret_val_regs: self.ret_val,
            ret_val_regs_float: Vec::new(),
            stack_vn,
            stack_args: self.stack_args,
            ret_stack_pop: self.ret_stack_pop,
            link_register_vn: self.link_register,
            preserves_memory: false,
            no_return: false,
        };
        let endianness = self
            .endianness
            .unwrap_or(strider_target::Endianness::Little);
        let mut b = FunctionBuilder::new(tracked, cc, endianness)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        Ok(b)
    }

    /// `build_fn` plus `create_region` + `set_entry_region` + `set_region`.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new`,
    /// `create_region`, or `set_entry_region`.
    pub fn build_fn_single_region(self) -> Result<FunctionBuilder> {
        let mut b = self.build_fn()?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        // Mirrors the lifter: recording arg carriers after entry setup is what
        // gives arg-query tests the same `arg_index_to_values` a lifted
        // function would have.
        b.record_register_arg_carriers();
        b.set_region(region);
        Ok(b)
    }

    /// The canonical `if (cond) { return 1 } else { return 2 }` scaffold the
    /// `If`-rewrite passes test against. `cond_builder` runs in the entry
    /// region and yields the condition plus an auxiliary `T` threaded back to
    /// the caller (typically operands to assert on after the rewrite); use
    /// `()` when there is none.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new`, region /
    /// IR construction, the closure, or `FunctionBuilder::build`.
    pub fn build_if_then_else_returns<F, T>(
        self,
        cond_builder: F,
    ) -> Result<(Function, strider_ir::node::NodeId, T)>
    where
        F: FnOnce(&mut FunctionBuilder) -> Result<(strider_ir::Value, T)>,
    {
        let mut b = self.build_fn()?;
        let entry = b.create_region_all()?;
        let t = b.create_region_all()?;
        let f = b.create_region_all()?;

        b.set_entry_region_all(entry)?;
        b.record_register_arg_carriers();
        b.set_region(entry);
        let (cond, aux) = cond_builder(&mut b)?;
        b.build_if(cond, t, f)?;

        b.set_region(t);
        let one = b.build_int_const(1u64, ValueType::I64)?;
        b.build_return(Some(one), &[])?;

        b.set_region(f);
        let two = b.build_int_const(2u64, ValueType::I64)?;
        b.build_return(Some(two), &[])?;
        b.set_lift_addr(None);

        let fg = b.build()?;
        let if_node = fg
            .graph()
            .all_node_ids()
            .find(|&nid| matches!(fg.node_kind(nid), NodeKind::If))
            .expect("scaffold must contain exactly one If node");
        Ok((fg, if_node, aux))
    }
}

/// Single-region function returning whatever `f` produces.
///
/// # Errors
///
/// Propagates any error from the builder closure or from `FunctionBuilder::build`.
pub fn make_empty_fn<F>(f: F) -> Result<Function>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<Value>,
{
    make_empty_fn_endian(strider_target::Endianness::Little, f)
}

/// [`make_empty_fn`] with an explicit endianness baked into the function,
/// which is the source of truth its passes read.
///
/// # Errors
///
/// Propagates any error from the builder or from `FunctionBuilder::build`.
pub fn make_empty_fn_endian<F>(endianness: strider_target::Endianness, f: F) -> Result<Function>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<Value>,
{
    // No declared registers, so the trivial convention: only the synthetic SP
    // is tracked, as an unreferenced `InitialVar`.
    let mut b = RegisterSet::new().endianness(endianness).build_fn()?;
    let region = b.create_region_all()?;
    b.set_entry_region_all(region)?;
    b.record_register_arg_carriers();
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let val = f(&mut b)?;
    // Re-stamp so the trailing `build_return` is attributed even when `f`
    // cleared the lift address, as fingerprint-propagation tests do when they
    // set their own per-insn addresses and reset to `None`.
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// Single-region function with `vn` tracked. The closure receives the
/// read-back value (a `VarPhi` over `InitialVar(vn)`) and returns the value to
/// wire into the `Return`; that read-back value comes back to the caller too.
///
/// # Errors
///
/// Propagates any error from the builder closure or from `FunctionBuilder::build`.
pub fn make_fn_with_var<F>(vn: rsleigh::Vn, f: F) -> Result<(Function, Value)>
where
    F: FnOnce(&mut FunctionBuilder, Value) -> Result<Value>,
{
    let mut b = RegisterSet::new()
        .tracked(vn)
        .arg(vn)
        .build_fn_single_region()?;
    let x = b.read_variable(&vn)?;
    let val = f(&mut b, x)?;
    // Re-stamp the sentinel after the closure (see `make_empty_fn`).
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    Ok((b.build()?, x))
}

/// Unpacked-slices entry point for tests that don't use the fluent
/// [`RegisterSet`]. `stack_vn = None` falls back to the synthetic test SP.
/// No region is created.
///
/// # Errors
///
/// Propagates any error from [`FunctionBuilder::new`].
pub fn builder(
    tracked: Vec<rsleigh::Vn>,
    arg_passing: &[rsleigh::Vn],
    callee_saved: &[rsleigh::Vn],
    ret_val: &[rsleigh::Vn],
    stack_vn: Option<rsleigh::Vn>,
    ret_stack_pop: i64,
    endianness: strider_target::Endianness,
) -> Result<FunctionBuilder> {
    tb::build_rs(
        tracked,
        arg_passing,
        callee_saved,
        ret_val,
        stack_vn,
        ret_stack_pop,
    )
    .endianness(endianness)
    .build_fn()
}

/// No declared registers, trivial convention, little-endian, no region.
///
/// # Errors
///
/// Propagates any error from [`FunctionBuilder::new`].
pub fn empty_builder() -> Result<FunctionBuilder> {
    RegisterSet::new().build_fn()
}

mod tb;
pub use tb::Tb;

pub fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// Create-and-stamp in one step, for fixtures that add nodes to a graph after
/// `build` and still have to satisfy the always-on fingerprint check.
pub fn sentinel_node(
    function: &mut strider_ir::Function,
    kind: strider_ir::node::NodeKind,
    inputs: impl IntoIterator<Item = strider_ir::node::ValueId>,
    outputs: impl IntoIterator<Item = strider_ir::node::ValueKind>,
) -> strider_ir::node::NodeId {
    let n = function.graph_mut().create_node(kind, inputs, outputs);
    function
        .side_tables_mut()
        .extend_asm_fingerprint(n, &[SENTINEL_LIFT_ADDR]);
    n
}

/// Test `ReadOnlyMemory` covering the mock-rom shapes the opt-pass suite
/// needs. `RecordingRom` stays separate: it logs reads to the side and is not
/// shape-compatible with this.
pub struct MockRom {
    shape: MockRomShape,
}

enum MockRomShape {
    /// `entries[i]` at `base + i * stride`; `size_filter` of `None` matches
    /// any read size.
    Strided {
        base: u64,
        stride: u64,
        entries: Vec<u64>,
        size_filter: Option<usize>,
    },
    /// Keyed by exact address; matches any read size.
    FixedTable { entries: BTreeMap<u64, u64> },
    /// A one-entry `FixedTable` with a size filter, kept distinct because it
    /// reads better at the call site.
    Limited { addr: u64, size: usize, value: u64 },
    /// Serves every `(addr, size)`.
    AlwaysAnswer { value: u64 },
}

impl MockRom {
    /// `entries[i]` at `base + i * stride`. Any other read size, and any
    /// stride of `0`, resolves to `None`.
    pub fn strided(base: u64, stride: u64, entries: Vec<u64>, size: usize) -> Self {
        Self {
            shape: MockRomShape::Strided {
                base,
                stride,
                entries,
                size_filter: Some(size),
            },
        }
    }

    /// Size is not constrained.
    pub fn fixed_table(entries: &[(u64, u64)]) -> Self {
        Self {
            shape: MockRomShape::FixedTable {
                entries: entries.iter().copied().collect(),
            },
        }
    }

    /// One `(addr, size)` mapping; every other read resolves to `None`.
    pub fn limited(addr: u64, size: usize, value: u64) -> Self {
        Self {
            shape: MockRomShape::Limited { addr, size, value },
        }
    }

    pub fn always_answer(value: u64) -> Self {
        Self {
            shape: MockRomShape::AlwaysAnswer { value },
        }
    }
}

impl MockRom {
    /// `None` when the shape doesn't serve that address/size.
    ///
    /// `read` encodes the resolved value LITTLE-ENDIAN, and `LoadReadOnly`
    /// decodes with the function's own endianness, so a fixture driving
    /// `MockRom` must build its function little-endian (the default).
    fn resolve(&self, addr: u64, size: usize) -> Option<u64> {
        match &self.shape {
            MockRomShape::Strided {
                base,
                stride,
                entries,
                size_filter,
            } => {
                if size_filter.is_some_and(|sz| size != sz) {
                    return None;
                }
                if addr < *base {
                    return None;
                }
                let offset = addr - *base;
                if *stride == 0 {
                    return None;
                }
                if !offset.is_multiple_of(*stride) {
                    return None;
                }
                let idx = (offset / *stride) as usize;
                entries.get(idx).copied()
            }
            MockRomShape::FixedTable { entries } => entries.get(&addr).copied(),
            MockRomShape::Limited {
                addr: a,
                size: s,
                value,
            } => (addr == *a && size == *s).then_some(*value),
            MockRomShape::AlwaysAnswer { value } => Some(*value),
        }
    }
}

impl ReadOnlyMemory for MockRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        let size = buf.len();
        // Every shape resolves to a `u64`, so a wider read is unserviceable.
        if size > 8 {
            anyhow::bail!("MockRom: read size {size} > 8 unsupported");
        }
        let value = self
            .resolve(addr, size)
            .ok_or_else(|| anyhow::anyhow!("MockRom: no value at {addr:#x} for size {size}"))?;
        buf.copy_from_slice(&value.to_le_bytes()[..size]);
        Ok(())
    }
}

pub fn stack_vn_x86() -> rsleigh::Vn {
    reg_vn(0x20, 4)
}

pub fn stack_vn_x86_64() -> rsleigh::Vn {
    reg_vn(0x20, 8)
}

/// Offset 0x40 matches the AArch64 Sleigh spec.
pub fn stack_vn_aarch64() -> rsleigh::Vn {
    reg_vn(0x40, 8)
}

/// Single-region function with `stack_vn` tracked as the stack pointer. The
/// closure gets the read-back SP (`InitialVar(stack_vn)`) and owns the whole
/// body, the `Return` included.
///
/// # Errors
///
/// Propagates any error from the builder closure or from `FunctionBuilder::build`.
pub fn make_sp_fn<F>(stack_vn: rsleigh::Vn, f: F) -> Result<Function>
where
    F: FnOnce(&mut FunctionBuilder, Value) -> Result<()>,
{
    let mut b = RegisterSet::new()
        .tracked(stack_vn)
        .callee_saved(stack_vn)
        .stack_vn(stack_vn)
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&stack_vn)?;
    f(&mut b, sp_val)?;
    b.set_lift_addr(None);
    b.build()
}
