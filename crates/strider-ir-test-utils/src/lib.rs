//! Shared mock-IR helpers used by tests across the workspace.
//!
//! Every helper here sets a **sentinel lift address** on the
//! `FunctionBuilder` for the duration of the closure so every node
//! created through the `build_*` API inherits a non-empty
//! asm-fingerprint.  This makes mock-graph tests satisfy the always-on
//! Layer-C asm-fingerprint check without needing to stamp each node by
//! hand.  The sentinel value is the magic constant [`SENTINEL_LIFT_ADDR`]
//! (`0xDEAD_BEEF_0000_0001`) so debugging is unambiguous when a sentinel
//! leaks into production output.
//!
//! This is a dedicated test-utility crate so consumers can dev-depend on
//! it without forcing strider-ir to carry a feature flag.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{
    Function, FunctionBuilder, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntUnaryOp,
    ReadOnlyMemory, Result, Value, ValueType,
};

/// Test-only extension over [`IRWalker`] supplying node-kind counting /
/// existence assertions.  These are assertion vocabulary used only by tests, so
/// they live here rather than on the production [`IRWalker`] trait.  Blanket-
/// implemented for every `IRWalker`, so a test brings them into scope with
/// `use strider_ir_test_utils::IrWalkerEx;`.
pub trait IrWalkerEx: IRWalker {
    /// Counts entry-reachable nodes whose [`NodeKind`] satisfies `pred`.
    fn count_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> usize {
        self.walk().filter(|&n| pred(self.node_kind(n))).count()
    }

    /// Returns `true` when at least one entry-reachable node satisfies `pred`.
    /// Short-circuits at the first match.
    fn has_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> bool {
        self.walk().any(|n| pred(self.node_kind(n)))
    }
}

impl<T: IRWalker + ?Sized> IrWalkerEx for T {}

/// Test-only extension over [`IRBuilderExt`] supplying builder shorthands that
/// aren't primitive in the IR.  Blanket-implemented for every `IRBuilderExt`,
/// so a test brings them into scope with
/// `use strider_ir_test_utils::IrBuilderEx;`.
pub trait IrBuilderEx: IRBuilderExt {
    /// Emits the canonical lowered shape for `lhs - rhs`:
    /// `Add(lhs, IntUnaryOp::Neg(rhs))`.  `IntBinaryOp::Sub` is not a primitive
    /// in this IR (pcode-lift lowers `IntSub` at lift time); this reproduces the
    /// same shape from the builder API.
    ///
    /// # Errors
    ///
    /// Returns an error if either operand is not a value edge.
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

/// Sentinel asm-fingerprint address used by every helper in this
/// module.  Distinct from any real machine address so debug output
/// (graph dumps, IR snapshots) is obvious when a sentinel-stamped
/// node leaks into a production code path.
pub const SENTINEL_LIFT_ADDR: u64 = 0xDEAD_BEEF_0000_0001;

/// Synthetic stack-pointer varnode [`RegisterSet::build_fn`] tracks when a
/// fixture configures no explicit `stack_vn`.  An 8-byte REGISTER at a high
/// offset that no common test register collides with, so a `Call`-building
/// fixture always has a tracked SP to read (the builder no longer mints
/// one).
const DEFAULT_TEST_SP: rsleigh::Vn = rsleigh::Vn {
    addr_off: 0x7000,
    addr_space: rsleigh::VnSpace::REGISTER,
    size: 8,
};

/// Fluent description of a mock function's register convention used by
/// mock-IR tests across the workspace.
///
/// [`RegisterSet::build_fn`] synthesises a
/// [`strider_target::BuiltCallingConvention`] from the declared register
/// lists and hands it to the single `FunctionBuilder::new` constructor,
/// then stamps [`SENTINEL_LIFT_ADDR`] as the active lift address so every
/// node the test subsequently creates carries a non-empty asm-fingerprint
/// (Layer-C contract).
///
/// The constructed `FunctionBuilder` has the sentinel lift_addr set
/// but no region created yet — callers that want a single entry
/// region can use [`RegisterSet::build_fn_single_region`] instead.
#[derive(Default, Clone)]
pub struct RegisterSet {
    tracked: Vec<rsleigh::Vn>,
    arg_passing: Vec<rsleigh::Vn>,
    callee_saved: Vec<rsleigh::Vn>,
    ret_val: Vec<rsleigh::Vn>,
    sp: Option<rsleigh::Vn>,
    ret_stack_pop: i64,
    /// Positional stack-argument layout baked into the built function's
    /// `default_cc` — the SSoT the SP-aware arg passes read.  `None` unless a
    /// stack-argument-detection fixture sets it.
    stack_args: Option<strider_target::StackArgs>,
    /// `None` defaults to little-endian in [`RegisterSet::build_fn`].
    endianness: Option<strider_target::Endianness>,
    /// Synthesised convention's `link_register_vn`.  `None` (the default)
    /// builds a convention with no architectural link register (the
    /// x86 / x86_64 case); set it for link-register ISAs (ARM/AArch64/…).
    link_register: Option<rsleigh::Vn>,
}

impl RegisterSet {
    /// Construct an empty register set.  All vectors start empty and
    /// `sp` / `ret_stack_pop` default to `None` / `0`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `vn` to the tracked-varnode set passed to
    /// `FunctionBuilder::new`.
    pub fn tracked(mut self, vn: rsleigh::Vn) -> Self {
        self.tracked.push(vn);
        self
    }

    /// Append `vn` to the synthesised convention's `arg_passing_regs`
    /// (see [`RegisterSet::build_fn`]).
    pub fn arg(mut self, vn: rsleigh::Vn) -> Self {
        self.arg_passing.push(vn);
        self
    }

    /// Append `vn` to the synthesised convention's `callee_saved_regs`.
    pub fn callee_saved(mut self, vn: rsleigh::Vn) -> Self {
        self.callee_saved.push(vn);
        self
    }

    /// Append `vn` to the synthesised convention's `ret_val_regs`.
    pub fn ret(mut self, vn: rsleigh::Vn) -> Self {
        self.ret_val.push(vn);
        self
    }

    /// Set the synthesised convention's `stack_vn`.
    pub fn stack_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.sp = Some(vn);
        self
    }

    /// Set the `ret_stack_pop` value.
    pub fn ret_stack_pop(mut self, n: i64) -> Self {
        self.ret_stack_pop = n;
        self
    }

    /// Set the synthesised convention's positional stack-argument layout
    /// directly, so the built function carries it in its `default_cc` — the
    /// SSoT the SP-aware arg passes read.
    pub fn stack_args(mut self, stack_args: Option<strider_target::StackArgs>) -> Self {
        self.stack_args = stack_args;
        self
    }

    /// Set the target endianness (defaults to little-endian).
    pub fn endianness(mut self, e: strider_target::Endianness) -> Self {
        self.endianness = Some(e);
        self
    }

    /// Set the synthesised convention's `link_register_vn` — the
    /// architectural link register for return-via-LR ISAs.  Leave unset
    /// for x86 / x86_64 (no link register).
    pub fn link_register(mut self, vn: rsleigh::Vn) -> Self {
        self.link_register = Some(vn);
        self
    }

    /// Construct a `FunctionBuilder` with this register set and stamp
    /// [`SENTINEL_LIFT_ADDR`] as the active lift address.  No region
    /// is created — callers that need multiple regions can drive
    /// `create_region` / `set_entry_region` / `set_region` themselves.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new`.
    pub fn build_fn(self) -> Result<FunctionBuilder> {
        // When no stack pointer is configured, default it to a synthetic SP
        // register.  `build_call` reads the stack pointer through the
        // variable table and errors when it is absent (it no longer mints an
        // SP anchor), so a fixture that builds a `Call` needs a tracked SP.
        // The synthetic SP sits at a high offset no common test register uses.
        // `FunctionBuilder::new` no longer seeds the stack vn (the lifter owns
        // that in production), so add it to the tracked set here — mirroring
        // the lifter — alongside the declared arg/ret regs it still seeds.
        let stack_vn = self.sp.unwrap_or(DEFAULT_TEST_SP);
        let mut tracked = self.tracked;
        if !tracked.contains(&stack_vn) {
            tracked.push(stack_vn);
        }
        // Synthesise a convention from the declared register lists and hand
        // it to the single `FunctionBuilder::new` constructor.  Struct-literal
        // construction (not `try_new`) skips the ABI-disjointness validation
        // so synthetic fixtures can declare overlapping/degenerate sets.
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
        };
        let endianness = self
            .endianness
            .unwrap_or(strider_target::Endianness::Little);
        let mut b = FunctionBuilder::new(tracked, cc, endianness)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        Ok(b)
    }

    /// Construct a `FunctionBuilder` with this register set and a
    /// single entry region.  Equivalent to `build_fn` followed by
    /// `create_region` + `set_entry_region` + `set_region`.
    /// [`SENTINEL_LIFT_ADDR`] is stamped as the active lift address.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new`,
    /// `create_region`, or `set_entry_region`.
    pub fn build_fn_single_region(self) -> Result<FunctionBuilder> {
        let mut b = self.build_fn()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        // Mirror the lifter: record register-arg carriers after entry setup so
        // arg-query tests see the same `arg_index_to_values` a lifted function
        // would (the IR no longer records these inside `set_entry_region`).
        b.record_register_arg_carriers();
        b.set_region(region);
        Ok(b)
    }

    /// Build the canonical `if (cond) { return 1 } else { return 2 }`
    /// scaffold used by the `If`-rewrite passes' test suites.
    ///
    /// Creates entry / true / false regions, calls `cond_builder` in
    /// the entry region to produce the boolean condition, then emits
    /// `build_if(cond, t, f)`, `return 1` from `t`, and `return 2`
    /// from `f`.  Returns the built graph, the unique `If` node id,
    /// and whatever auxiliary value the closure threads back through
    /// `T` (e.g. the operands the caller will assert against after
    /// the rewrite).
    ///
    /// Use `()` for `T` when the closure has no auxiliary outputs.
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
        use strider_ir::node::{NodeKind, ValueType};

        let mut b = self.build_fn()?;
        let entry = b.create_region()?;
        let t = b.create_region()?;
        let f = b.create_region()?;

        b.set_entry_region(entry)?;
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
        // The scaffold has exactly one `If` node — the validator's
        // shape contract would already reject anything else.
        let if_node = fg
            .graph()
            .all_node_ids()
            .find(|&nid| matches!(fg.node_kind(nid), NodeKind::If))
            .expect("scaffold must contain exactly one If node");
        Ok((fg, if_node, aux))
    }
}

/// Builds a single-region function whose return value is what `f` produces.
///
/// Sets [`SENTINEL_LIFT_ADDR`] as the active lift address for the
/// duration of `f` and the trailing `build_return` so every emitted
/// node carries a non-empty asm-fingerprint (Layer-C contract).
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

/// Like [`make_empty_fn`] but with an explicit target endianness baked into
/// the function — for fixtures that exercise endianness-dependent behaviour
/// (the function is the single source of truth its passes read).
///
/// # Errors
///
/// Propagates any error from the builder or from `FunctionBuilder::build`.
pub fn make_empty_fn_endian<F>(endianness: strider_target::Endianness, f: F) -> Result<Function>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<Value>,
{
    // No declared registers → the trivial convention (only the synthetic
    // SP gets tracked, as an unreferenced `InitialVar`).
    let mut b = RegisterSet::new().endianness(endianness).build_fn()?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.record_register_arg_carriers();
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let val = f(&mut b)?;
    // Re-stamp the sentinel after the closure so that the trailing
    // `build_return` is attributed even if `f` cleared the lift_addr
    // (e.g. asm-fingerprint-propagation tests that set their own
    // per-insn addresses and reset to `None` before returning).
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// Builds a single-region function with a tracked variable `vn`.  The closure
/// receives the read-back value (a `VarPhi` over `InitialVar(vn)`) and
/// returns the value to wire into the function's `Return`.  Returns the built
/// graph and the read-back `Value` so the caller can refer to it later.
///
/// Sets [`SENTINEL_LIFT_ADDR`] for the duration of `f` and the trailing
/// `build_return` so every emitted node carries a non-empty
/// asm-fingerprint (Layer-C contract).
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

/// Constructs a [`FunctionBuilder`] from unpacked convention parts —
/// the low-level mock-construction entry point for tests that don't use
/// the fluent [`RegisterSet`].  Synthesises a
/// [`strider_target::BuiltCallingConvention`] from the slices and hands it
/// to the single [`FunctionBuilder::new`] constructor (the builder seeds
/// the convention's arg / ret / SP registers into the tracked set).
///
/// `stack_vn = None` defaults to the synthetic test SP so a `Call`-building
/// fixture always has a tracked stack pointer.  No region is created.
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
    RegisterSet {
        tracked,
        arg_passing: arg_passing.to_vec(),
        callee_saved: callee_saved.to_vec(),
        ret_val: ret_val.to_vec(),
        sp: stack_vn,
        ret_stack_pop,
        stack_args: None,
        endianness: Some(endianness),
        link_register: None,
    }
    .build_fn()
}

/// Constructs an "empty" [`FunctionBuilder`]: no declared registers, the
/// trivial convention, little-endian.  No region is created.  The lift
/// sentinel is set (via [`RegisterSet::build_fn`]).
///
/// # Errors
///
/// Propagates any error from [`FunctionBuilder::new`].
pub fn empty_builder() -> Result<FunctionBuilder> {
    RegisterSet::new().build_fn()
}

mod tb;
pub use tb::Tb;

/// Fabricates a register varnode of the given size at offset `off`.
pub fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// Creates a node directly on `function`'s graph and stamps the
/// [`SENTINEL_LIFT_ADDR`] asm-fingerprint on it, so fixtures that build mock
/// graphs after `build` satisfy the always-on fingerprint check without the
/// repetitive two-line create-then-stamp dance.
pub fn sentinel_node(
    function: &mut strider_ir::Function,
    kind: strider_ir::node::NodeKind,
    inputs: impl IntoIterator<Item = strider_ir::node::ValueId>,
    outputs: impl IntoIterator<Item = strider_ir::node::ValueKind>,
) -> strider_ir::node::NodeId {
    let n = function.graph_mut().create_node(kind, inputs, outputs);
    function.side_tables_mut().extend_asm_fingerprint(n, &[SENTINEL_LIFT_ADDR]);
    n
}

/// Test `ReadOnlyMemory` helper covering the three mock-rom shapes
/// that appear across the opt-pass test suite.  Replaces the bespoke
/// `TableRom` / `PartialRom` / `TestRom` / `Limited` / `AlwaysAnswer` /
/// `OneEntryRom` impls that previously lived inline in each host test
/// file.
///
/// Construct one with [`MockRom::strided`], [`MockRom::fixed_table`],
/// [`MockRom::always_answer`], or [`MockRom::limited`].
///
/// `RecordingRom` deliberately stays separate — it records reads to a
/// side log and is not shape-compatible with this helper.
pub struct MockRom {
    shape: MockRomShape,
}

enum MockRomShape {
    /// Strided table: returns `entries[i]` at `base + i * stride`.
    /// `size_filter` restricts which read sizes match (None = any).
    Strided {
        base: u64,
        stride: u64,
        entries: Vec<u64>,
        size_filter: Option<usize>,
    },
    /// Lookup table keyed by exact address; matches any read size.
    FixedTable { entries: BTreeMap<u64, u64> },
    /// Single (addr, size) → value mapping; everything else returns
    /// `None`.  Equivalent to `FixedTable` of length 1 with a size
    /// filter, kept as a distinct shape for call-site clarity.
    Limited { addr: u64, size: usize, value: u64 },
    /// Returns the same value for every (addr, size).
    AlwaysAnswer { value: u64 },
}

impl MockRom {
    /// Strided lookup: returns `entries[i]` at addresses `base`,
    /// `base + stride`, `base + 2*stride`, …  Read sizes other than
    /// `size` return `None`.  Stride `0` always returns `None`.
    ///
    /// Replaces the bespoke `TableRom` shape used by the jump-table
    /// classifier tests.
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

    /// Fixed `(addr, value)` lookup table; size is not constrained.
    /// Replaces the bespoke `TestRom` shape.
    pub fn fixed_table(entries: &[(u64, u64)]) -> Self {
        Self {
            shape: MockRomShape::FixedTable {
                entries: entries.iter().copied().collect(),
            },
        }
    }

    /// Single `(addr, size) → value` mapping; every other read
    /// returns `None`.  Replaces the bespoke `Limited` and
    /// `OneEntryRom` shapes.
    pub fn limited(addr: u64, size: usize, value: u64) -> Self {
        Self {
            shape: MockRomShape::Limited { addr, size, value },
        }
    }

    /// Returns the same value for every `(addr, size)`.  Replaces the
    /// bespoke `AlwaysAnswer` shape.
    pub fn always_answer(value: u64) -> Self {
        Self {
            shape: MockRomShape::AlwaysAnswer { value },
        }
    }
}

impl MockRom {
    /// Resolves the configured value for `(addr, size)`, or `None` when
    /// the shape doesn't serve that address/size.  The `read` impl
    /// encodes this value LITTLE-ENDIAN into the caller buffer (the
    /// reader no longer decodes — the optimizer does).  Tests that drive
    /// `MockRom` therefore back it with a little-endian function (the
    /// default for `make_empty_fn` / `RegisterSet`); `LoadReadOnly` decodes
    /// using the function's own `Function::endianness`.
    fn resolve(&self, addr: u64, size: usize) -> Option<u64> {
        match &self.shape {
            MockRomShape::Strided {
                base,
                stride,
                entries,
                size_filter,
            } => {
                if let Some(sz) = size_filter
                    && size != *sz
                {
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
            } => {
                if addr == *a && size == *s {
                    Some(*value)
                } else {
                    None
                }
            }
            MockRomShape::AlwaysAnswer { value } => Some(*value),
        }
    }
}

impl ReadOnlyMemory for MockRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<()> {
        let size = buf.len();
        // `size > 8` was historically rejected by the trait's `u64`
        // contract; the value fits a u64, so the same bound holds.
        if size > 8 {
            anyhow::bail!("MockRom: read size {size} > 8 unsupported");
        }
        let value = self
            .resolve(addr, size)
            .ok_or_else(|| anyhow::anyhow!("MockRom: no value at {addr:#x} for size {size}"))?;
        // Encode the low `size` bytes little-endian; `LoadReadOnly` decodes
        // them with the function's own `Function::endianness`, so a fixture
        // using `MockRom` must be built little-endian (the default).
        buf.copy_from_slice(&value.to_le_bytes()[..size]);
        Ok(())
    }
}

/// Stack-pointer varnode at REGISTER:0x20 with x86 ESP width (4 bytes).
pub fn stack_vn_x86() -> rsleigh::Vn {
    reg_vn(0x20, 4)
}

/// Stack-pointer varnode at REGISTER:0x20 with x86_64 RSP width (8 bytes).
pub fn stack_vn_x86_64() -> rsleigh::Vn {
    reg_vn(0x20, 8)
}

/// Stack-pointer varnode at REGISTER:0x40 with AArch64 / ARM64 SP width
/// (8 bytes).  Same offset used by the AArch64 Sleigh spec and by the
/// `sp64_vn` / `sp64` helpers that appear in several opt-pass test modules.
pub fn stack_vn_aarch64() -> rsleigh::Vn {
    reg_vn(0x40, 8)
}

/// Builds a single-region function with `sp_vn` tracked as a stack-pointer
/// variable.  The closure receives the builder and the read-back SP value
/// (`InitialVar(sp_vn)`) and is responsible for emitting the function body
/// — including the `Return`.  This matches `strider_ir_test_utils::builder(vec![sp],
/// &[], &[sp], &[], None, 0)?` + region setup, which appears verbatim in
/// dozens of opt tests.
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
