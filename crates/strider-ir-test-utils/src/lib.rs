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

use strider_ir::{Function, FunctionBuilder, ReadOnlyMemory, Result, Value};

/// Sentinel asm-fingerprint address used by every helper in this
/// module.  Distinct from any real machine address so debug output
/// (graph dumps, IR snapshots) is obvious when a sentinel-stamped
/// node leaks into a production code path.
pub const SENTINEL_LIFT_ADDR: u64 = 0xDEAD_BEEF_0000_0001;

/// Fluent builder for the 7-positional-arg `FunctionBuilder::new_raw`
/// signature used by mock-IR tests across the workspace.
///
/// The builder defers to `FunctionBuilder::new_raw` and then stamps
/// [`SENTINEL_LIFT_ADDR`] as the active lift address so every node
/// the test subsequently creates carries a non-empty asm-fingerprint
/// (Layer-C contract).  Test sites no longer repeat the
/// `new_raw + set_lift_addr` dance.
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
}

impl RegisterSet {
    /// Construct an empty register set.  All vectors start empty and
    /// `sp` / `ret_stack_pop` default to `None` / `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `vn` to the tracked-variables list.  Equivalent to the
    /// first positional argument of `FunctionBuilder::new_raw`.
    #[must_use]
    pub fn tracked(mut self, vn: rsleigh::Vn) -> Self {
        self.tracked.push(vn);
        self
    }

    /// Append `vn` to the arg-passing list (second positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn arg(mut self, vn: rsleigh::Vn) -> Self {
        self.arg_passing.push(vn);
        self
    }

    /// Append `vn` to the callee-saved list (third positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn callee_saved(mut self, vn: rsleigh::Vn) -> Self {
        self.callee_saved.push(vn);
        self
    }

    /// Append `vn` to the ret-val list (fourth positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn ret(mut self, vn: rsleigh::Vn) -> Self {
        self.ret_val.push(vn);
        self
    }

    /// Set the stack-pointer varnode (fifth positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn stack_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.sp = Some(vn);
        self
    }

    /// Set the `ret_stack_pop` value (sixth positional arg of
    /// `FunctionBuilder::new_raw`).
    #[must_use]
    pub fn ret_stack_pop(mut self, n: i64) -> Self {
        self.ret_stack_pop = n;
        self
    }

    /// Construct a `FunctionBuilder` with this register set and stamp
    /// [`SENTINEL_LIFT_ADDR`] as the active lift address.  No region
    /// is created — callers that need multiple regions can drive
    /// `create_region` / `set_entry_region` / `set_region` themselves.
    ///
    /// # Errors
    ///
    /// Propagates any error from `FunctionBuilder::new_raw`.
    pub fn build_fn(self) -> Result<FunctionBuilder> {
        let mut b = FunctionBuilder::new_raw(
            self.tracked,
            &self.arg_passing,
            &self.callee_saved,
            &self.ret_val,
            self.sp,
            self.ret_stack_pop,
        )?;
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
    /// Propagates any error from `FunctionBuilder::new_raw`,
    /// `create_region`, or `set_entry_region`.
    pub fn build_fn_single_region(self) -> Result<FunctionBuilder> {
        let mut b = self.build_fn()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
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
    /// Propagates any error from `FunctionBuilder::new_raw`, region /
    /// IR construction, the closure, or `FunctionBuilder::build`.
    pub fn build_if_then_else_returns<F, T>(self, cond_builder: F) -> Result<(Function, strider_ir::node::NodeId, T)>
    where
        F: FnOnce(&mut FunctionBuilder) -> Result<(strider_ir::Value, T)>,
    {
        use strider_ir::node::{NodeKind, ValueType};

        let mut b = self.build_fn()?;
        let entry = b.create_region()?;
        let t = b.create_region()?;
        let f = b.create_region()?;

        b.set_entry_region(entry)?;
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
    let mut b = FunctionBuilder::empty()?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
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
pub fn make_fn_with_var<F>(
    vn: rsleigh::Vn,
    f: F,
) -> Result<(Function, Value)>
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

/// Fabricates a register varnode of the given size at offset `off`.
#[must_use]
pub fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

/// Test `ReadOnlyMemory` helper covering the three mock-rom shapes
/// that appear across the opt-pass test suite.  Replaces the bespoke
/// `TableRom` / `PartialRom` / `TestRom` / `Limited` / `AlwaysAnswer` /
/// `OneEntryRom` impls that previously lived inline in each host test
/// file.
///
/// Construct one with [`MockRom::strided`], [`MockRom::fixed_table`],
/// [`MockRom::always_answer`], or [`MockRom::limited`].  Builder-style
/// modifier [`MockRom::with_cutoff`] covers the additional behaviour
/// real tests need.
///
/// `RecordingRom` deliberately stays separate — it records reads to a
/// side log and is not shape-compatible with this helper.
pub struct MockRom {
    shape: MockRomShape,
}

enum MockRomShape {
    /// Strided table: returns `entries[i]` at `base + i * stride`.
    /// `size_filter` restricts which read sizes match (None = any).
    /// `cutoff` caps how many entries can be served (None = all).
    Strided {
        base: u64,
        stride: u64,
        entries: Vec<u64>,
        size_filter: Option<usize>,
        cutoff: Option<usize>,
    },
    /// Lookup table keyed by exact address; matches any read size.
    FixedTable { entries: BTreeMap<u64, u64> },
    /// Single (addr, size) → value mapping; everything else returns
    /// `None`.  Equivalent to `FixedTable` of length 1 with a size
    /// filter, kept as a distinct shape for call-site clarity.
    Limited {
        addr: u64,
        size: usize,
        value: u64,
    },
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
    #[must_use]
    pub fn strided(base: u64, stride: u64, entries: Vec<u64>, size: usize) -> Self {
        Self {
            shape: MockRomShape::Strided {
                base,
                stride,
                entries,
                size_filter: Some(size),
                cutoff: None,
            },
        }
    }

    /// Cap a [`MockRom::strided`] to serve only the first `n` entries.
    /// Reads at valid table addresses past the cutoff return `None`.
    /// Replaces the bespoke `PartialRom` shape.
    ///
    /// # Panics
    ///
    /// Panics if `self` was not constructed via [`MockRom::strided`].
    #[must_use]
    pub fn with_cutoff(mut self, n: usize) -> Self {
        match &mut self.shape {
            MockRomShape::Strided { cutoff, .. } => *cutoff = Some(n),
            _ => panic!("with_cutoff only supported on MockRom::strided"),
        }
        self
    }

    /// Fixed `(addr, value)` lookup table; size is not constrained.
    /// Replaces the bespoke `TestRom` shape.
    #[must_use]
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
    #[must_use]
    pub fn limited(addr: u64, size: usize, value: u64) -> Self {
        Self {
            shape: MockRomShape::Limited { addr, size, value },
        }
    }

    /// Returns the same value for every `(addr, size)`.  Replaces the
    /// bespoke `AlwaysAnswer` shape.
    #[must_use]
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
    /// `MockRom` therefore use a little-endian `OptCtx`, which is the
    /// default.
    fn resolve(&self, addr: u64, size: usize) -> Option<u64> {
        match &self.shape {
            MockRomShape::Strided {
                base,
                stride,
                entries,
                size_filter,
                cutoff,
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
                if let Some(c) = cutoff
                    && idx >= *c
                {
                    return None;
                }
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
        // Encode the low `size` bytes little-endian (matches the default
        // little-endian decode in `OptCtx`).
        buf.copy_from_slice(&value.to_le_bytes()[..size]);
        Ok(())
    }
}

/// Stack-pointer varnode at REGISTER:0x20 with x86 ESP width (4 bytes).
#[must_use]
pub fn stack_vn_x86() -> rsleigh::Vn {
    reg_vn(0x20, 4)
}

/// Stack-pointer varnode at REGISTER:0x20 with x86_64 RSP width (8 bytes).
#[must_use]
pub fn stack_vn_x86_64() -> rsleigh::Vn {
    reg_vn(0x20, 8)
}

/// Stack-pointer varnode at REGISTER:0x40 with AArch64 / ARM64 SP width
/// (8 bytes).  Same offset used by the AArch64 Sleigh spec and by the
/// `sp64_vn` / `sp64` helpers that appear in several opt-pass test modules.
#[must_use]
pub fn stack_vn_aarch64() -> rsleigh::Vn {
    reg_vn(0x40, 8)
}

/// Builds a single-region function with `sp_vn` tracked as a stack-pointer
/// variable.  The closure receives the builder and the read-back SP value
/// (`InitialVar(sp_vn)`) and is responsible for emitting the function body
/// — including the `Return`.  This matches `FunctionBuilder::new_raw(vec![sp],
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
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&stack_vn)?;
    f(&mut b, sp_val)?;
    b.set_lift_addr(None);
    b.build()
}
