//! `Load` / `Store` builders with first-class memory-token vertices.
//!
//! Slot conventions (matching the IR `expected_signature`):
//!
//! * `Load`  inputs `[mem(0), addr(1)]`; output: the loaded value (slot 0).
//! * `Store` inputs `[mem(0), addr(1), data(2)]`; output: the new memory
//!   token (slot 0).
//!
//! `Load` is value-producing (sealed via
//! [`finish`](MatcherBuilder::finish)); `Store` is a memory-token root
//! (sealed via [`finish_node`](MatcherBuilder::finish_node)). Both model
//! their memory predecessor (wired via `mem_in`) and `Store` exposes its
//! produced memory token (via [`MatcherBuilder::memory_output`]) so a
//! downstream `load` / `store` can chain off it.
//!
//! `space` is enforced at kind-match time via
//! [`KindSpec::VariantWith`](crate::pattern::KindSpec::VariantWith);
//! `bit_width` / `stack_only` / `stack_offset` are node-only predicates
//! routed through [`MatcherBuilder::set_node_limit`] so they short-circuit
//! before child recursion.

use strider_ir::node::{NodeId, NodeKind};

use crate::builder::{MatcherBuilder, PatNodeRef, PatOutRef};
use crate::capture::Capture;
use crate::match_pat::MatchPat;
use crate::pattern::{KindSpec, Pattern};

use super::{MemPat, SubCompiler};

// ── Stack-access filter (shared by LoadPat / StorePat) ───────────────────────

/// Filter applied at match time by looking up `Function::stack_offset`
/// on the matched node (O(1) — no re-decomposition of the address).
#[derive(Clone)]
enum StackOffsetFilter {
    /// Match exactly one concrete offset.
    Exact(i64),
    /// Match any offset in the provided set.
    Set(Vec<i64>),
}

impl StackOffsetFilter {
    fn matches(&self, offset: i64) -> bool {
        match self {
            Self::Exact(k) => offset == *k,
            Self::Set(ks) => ks.contains(&offset),
        }
    }
}

/// SP-relative match state shared by `LoadPat` and `StorePat`.
#[derive(Clone, Default)]
struct StackAccessSpec {
    stack_offset_filter: Option<StackOffsetFilter>,
    /// When `true`, rejects matches where `Function::stack_offset` is `None`.
    stack_only: bool,
}

impl StackAccessSpec {
    fn active(&self) -> bool {
        self.stack_offset_filter.is_some() || self.stack_only
    }

    fn check(&self, function: &strider_ir::Function, node: NodeId) -> bool {
        if !self.active() {
            return true;
        }
        let Some((_base, offset)) = function.stack_offset(node) else {
            return false;
        };
        if let Some(f) = &self.stack_offset_filter
            && !f.matches(offset)
        {
            return false;
        }
        true
    }
}

/// Build the `space`-aware kind spec for a `Load` / `Store`. Without a
/// space constraint the spec is variant-agnostic; with one it pins the
/// exact `VnSpace` via a `VariantWith` predicate so the check fires at
/// kind-match time.
fn load_store_kind(exemplar: NodeKind, space: Option<rsleigh::VnSpace>) -> KindSpec {
    match space {
        None => KindSpec::Variant(std::mem::discriminant(&exemplar)),
        Some(s) => KindSpec::VariantWith {
            discriminant: std::mem::discriminant(&exemplar),
            check: Box::new(move |k| {
                matches!((exemplar_is_load(&exemplar), k),
                    (true, NodeKind::Load(actual)) | (false, NodeKind::Store(actual))
                        if *actual == s)
            }),
        },
    }
}

fn exemplar_is_load(k: &NodeKind) -> bool {
    matches!(k, NodeKind::Load(_))
}

// ── LoadPat ───────────────────────────────────────────────────────────────────

/// Builder for `Load` node patterns. Created by [`load`].
///
/// `Load` inputs are `[mem(0), addr(1)]`; its single output is the
/// loaded value.
#[derive(Default)]
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<SubCompiler>,
    mem_in: Option<SubCompiler>,
    bit_width: Option<u32>,
    stack: StackAccessSpec,
    capture: Option<Capture>,
}

impl LoadPat {
    /// Restrict the match to loads in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }

    /// Constrain the load's address operand (`inputs[1]`).
    #[must_use]
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.addr = Some(Box::new(move |b| p.compile(b)));
        self
    }

    /// Constrain the load's memory predecessor (`inputs[0]`) to a
    /// memory-producing sub-pattern (a `store` / `mem_phi` / `call`).
    /// Wires the producer's memory-token output into the load's memory
    /// input slot, so the IR memory chain is walked the same way as the
    /// value chain.
    #[must_use]
    pub fn mem_in<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.mem_in = Some(Box::new(move |b| p.compile_mem(b)));
        self
    }

    /// Restrict the match to loads whose value output is `n` bits wide.
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }

    /// Restrict the match to loads whose address decomposes to exactly
    /// `sp + k` (reads `Function::stack_offset` in O(1)).
    #[must_use]
    pub fn stack_offset(mut self, k: i64) -> Self {
        self.stack.stack_offset_filter = Some(StackOffsetFilter::Exact(k));
        self
    }

    /// Restrict the match to loads whose address decomposes to `sp + k`
    /// for some `k` in `ks`.
    #[must_use]
    pub fn stack_offset_any(mut self, ks: impl Into<Vec<i64>>) -> Self {
        self.stack.stack_offset_filter = Some(StackOffsetFilter::Set(ks.into()));
        self
    }

    /// Reject matches where `Function::stack_offset(node)` is `None`.
    #[must_use]
    pub fn stack_only(mut self) -> Self {
        self.stack.stack_only = true;
        self
    }

    /// Bind the resulting `Load`'s value output to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Seal the builder into a finished [`Pattern`].
    #[must_use]
    pub fn build(self) -> Pattern {
        let LoadPat {
            space,
            addr,
            mem_in,
            bit_width,
            stack,
            capture,
        } = self;
        let mut b = MatcherBuilder::new();
        let exemplar = NodeKind::Load(rsleigh::VnSpace::RAM);
        let node = b.node(load_store_kind(exemplar, space));
        // The loaded value lives at output slot 0.
        let value_out = b.value_output(node, 0);

        wire_mem_in(&mut b, node, 0, mem_in);
        if let Some(addr) = addr {
            let a = addr(&mut b);
            b.input(node, 1, a);
        }
        if let Some(c) = capture {
            b.capture_node(value_out, c);
        }
        install_load_node_filter(&mut b, value_out, bit_width, stack);
        b.finish(value_out)
    }
}

/// Construct a fresh [`LoadPat`].
#[must_use]
pub fn load() -> LoadPat {
    LoadPat::default()
}

// ── StorePat ──────────────────────────────────────────────────────────────────

/// Builder for `Store` node patterns. Created by [`store`].
///
/// `Store` inputs are `[mem(0), addr(1), data(2)]`; its single output is
/// the new memory token (slot 0).
#[derive(Default)]
pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<SubCompiler>,
    data: Option<SubCompiler>,
    mem_in: Option<SubCompiler>,
    bit_width: Option<u32>,
    stack: StackAccessSpec,
    capture: Option<Capture>,
}

impl StorePat {
    /// Restrict the match to stores in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }

    /// Constrain the store's address operand (`inputs[1]`).
    #[must_use]
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.addr = Some(Box::new(move |b| p.compile(b)));
        self
    }

    /// Constrain the value being stored (`inputs[2]`).
    #[must_use]
    pub fn data<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.data = Some(Box::new(move |b| p.compile(b)));
        self
    }

    /// Constrain the store's memory predecessor (`inputs[0]`) to a
    /// memory-producing sub-pattern (a `store` / `mem_phi` / `call`).
    #[must_use]
    pub fn mem_in<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.mem_in = Some(Box::new(move |b| p.compile_mem(b)));
        self
    }

    /// Restrict the match to stores whose data input (`inputs[2]`) is
    /// `n` bits wide.
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }

    /// Restrict the match to stores whose address decomposes to exactly
    /// `sp + k`.
    #[must_use]
    pub fn stack_offset(mut self, k: i64) -> Self {
        self.stack.stack_offset_filter = Some(StackOffsetFilter::Exact(k));
        self
    }

    /// Restrict the match to stores whose address decomposes to `sp + k`
    /// for some `k` in `ks`.
    #[must_use]
    pub fn stack_offset_any(mut self, ks: impl Into<Vec<i64>>) -> Self {
        self.stack.stack_offset_filter = Some(StackOffsetFilter::Set(ks.into()));
        self
    }

    /// Reject matches where `Function::stack_offset(node)` is `None`.
    #[must_use]
    pub fn stack_only(mut self) -> Self {
        self.stack.stack_only = true;
        self
    }

    /// Bind the resulting `Store` node to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Lower the store onto `b`, returning its memory-token output and
    /// node handle. Shared by [`build`](Self::build) (which seals on the
    /// node) and [`compile_mem`](MemPat::compile_mem) (which returns the
    /// memory output for chaining).
    fn lower(self, b: &mut MatcherBuilder) -> (PatNodeRef, PatOutRef) {
        let StorePat {
            space,
            addr,
            data,
            mem_in,
            bit_width,
            stack,
            capture,
        } = self;
        let exemplar = NodeKind::Store(rsleigh::VnSpace::RAM);
        let node = b.node(load_store_kind(exemplar, space));
        // The new memory token lives at output slot 0.
        let mem_out = b.memory_output(node, 0);

        wire_mem_in(b, node, 0, mem_in);
        if let Some(addr) = addr {
            let a = addr(b);
            b.input(node, 1, a);
        }
        if let Some(data) = data {
            let d = data(b);
            b.input(node, 2, d);
        }
        if let Some(c) = capture {
            b.capture_node(mem_out, c);
        }
        install_store_node_filter(b, mem_out, bit_width, stack);
        (node, mem_out)
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the
    /// `Store` node (a memory-token root, no value output).
    #[must_use]
    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let (node, _mem_out) = self.lower(&mut b);
        b.finish_node(node)
    }
}

impl MemPat for StorePat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatOutRef {
        let (_node, mem_out) = self.lower(b);
        mem_out
    }
}

/// Construct a fresh [`StorePat`].
#[must_use]
pub fn store() -> StorePat {
    StorePat::default()
}

// ── Shared wiring helpers ────────────────────────────────────────────────────

/// Wire a memory-producing `mem_in` sub-pattern into `node`'s memory
/// input `slot` (the producer's memory-token output feeds the slot).
fn wire_mem_in(
    b: &mut MatcherBuilder,
    node: PatNodeRef,
    slot: usize,
    mem_in: Option<SubCompiler>,
) {
    if let Some(mem_in) = mem_in {
        let m = mem_in(b);
        b.input(node, slot, m);
    }
}

/// Install the `bit_width` (value-output width) + stack-access node
/// limit on a `Load` (the width reads the matched node's value output).
fn install_load_node_filter(
    b: &mut MatcherBuilder,
    out: PatOutRef,
    bit_width: Option<u32>,
    stack: StackAccessSpec,
) {
    if bit_width.is_none() && !stack.active() {
        return;
    }
    b.set_node_limit(
        out,
        Box::new(move |matcher, node, ty| {
            if let Some(w) = bit_width
                && ty.bit_width() != w as usize
            {
                return false;
            }
            stack.check(matcher.function(), node)
        }),
    );
}

/// Install the `bit_width` (data-input width) + stack-access node limit
/// on a `Store`. The store's own output is the memory token, so the
/// width is read from the data input (`inputs[2]`).
fn install_store_node_filter(
    b: &mut MatcherBuilder,
    out: PatOutRef,
    bit_width: Option<u32>,
    stack: StackAccessSpec,
) {
    if bit_width.is_none() && !stack.active() {
        return;
    }
    b.set_node_limit(
        out,
        Box::new(move |matcher, node, _ty| {
            let f = matcher.function();
            if let Some(w) = bit_width {
                let Ok(data_in) = f.node_input_id_at(node, 2) else {
                    return false;
                };
                let data_out = f.input_output_id(data_in);
                let Some(data_ty) = f.output_kind(data_out).as_value() else {
                    return false;
                };
                if data_ty.bit_width() != w as usize {
                    return false;
                }
            }
            stack.check(f, node)
        }),
    );
}
