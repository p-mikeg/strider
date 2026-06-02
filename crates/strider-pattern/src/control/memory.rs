//! `Load` / `Store` builders with first-class memory-token vertices.
//!
//! Both are thin slot-convention wrappers over the shared [`NodePat`]
//! core. Slot conventions (matching the IR `expected_signature`):
//!
//! * `Load`  inputs `[mem(0), addr(1)]`; output: the loaded value (slot 0).
//! * `Store` inputs `[mem(0), addr(1), data(2)]`; output: the new memory
//!   token (slot 0).
//!
//! `Load` is value-producing (a value root that nests as a value
//! operand); `Store` is a memory-token root (exposes its produced memory
//! token via [`MemPat`]). Both model their memory predecessor (wired via
//! `mem_in`).
//!
//! `space` is enforced at kind-match time via [`KindSpec::VariantWith`];
//! `bit_width` is a declarative output-vertex width constraint (on the
//! `Load`'s value output / the `Store`'s data-input producer output,
//! checked by the matcher's `output_ok`); `stack_only` / `stack_offset`
//! are genuine `Function::stack_offset` side-table lookups routed through
//! a [`NodePat`] node-limit.

use strider_ir::node::{NodeId, NodeKind};

use crate::builder::{MatcherBuilder, PatOutRef};
use crate::capture::Capture;
use crate::match_pat::MatchPat;
use crate::pattern::{KindSpec, Pattern};

use super::MemPat;
use super::node_pat::{KindCheck, NodePat, variant_kind};

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
    let discriminant = std::mem::discriminant(&exemplar);
    let check = space.map(|s| {
        let check: KindCheck = Box::new(move |k| {
            matches!((exemplar_is_load(&exemplar), k),
                (true, NodeKind::Load(actual)) | (false, NodeKind::Store(actual))
                    if *actual == s)
        });
        check
    });
    variant_kind(discriminant, check)
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
    addr: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
    mem_in: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
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
        self.addr = Some(Box::new(move |n: NodePat| n.input(1, p)));
        self
    }

    /// Constrain the load's memory predecessor (`inputs[0]`) to a
    /// memory-producing sub-pattern (a `store` / `mem_phi` / `call`).
    /// Wires the producer's memory-token output into the load's memory
    /// input slot, so the IR memory chain is walked the same way as the
    /// value chain.
    #[must_use]
    pub fn mem_in<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.mem_in = Some(Box::new(move |n: NodePat| n.input_mem(0, p)));
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

    /// Translate the accumulated filters into a configured [`NodePat`]
    /// (a `Load`-kind value root at slot 0).
    fn configured(self) -> NodePat {
        let LoadPat {
            space,
            addr,
            mem_in,
            bit_width,
            stack,
            capture,
        } = self;
        let exemplar = NodeKind::Load(rsleigh::VnSpace::RAM);
        // The loaded value lives at output slot 0.
        let mut n = NodePat::value(load_store_kind(exemplar, space), 0);
        if let Some(m) = mem_in {
            n = m(n);
        }
        if let Some(a) = addr {
            n = a(n);
        }
        if let Some(c) = capture {
            n = n.capture(c);
        }
        if let Some(w) = bit_width {
            // The loaded value is the Load's value output, so pin the
            // anchor output vertex's width declaratively.
            n = n.with_output_width(w);
        }
        if stack.active() {
            // The SP-relative stack filter is an irreducible
            // `Function::stack_offset` side-table lookup.
            n = n.with_node_limit(move || {
                Box::new(move |matcher, node, _ty| stack.check(matcher.function(), node))
            });
        }
        n
    }

    /// Seal the builder into a finished [`Pattern`].
    #[must_use]
    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MatchPat for LoadPat {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        self.configured().compile_value(b)
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
    addr: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
    data: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
    mem_in: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
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
        self.addr = Some(Box::new(move |n: NodePat| n.input(1, p)));
        self
    }

    /// Constrain the value being stored (`inputs[2]`).
    #[must_use]
    pub fn data<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.data = Some(Box::new(move |n: NodePat| n.input(2, p)));
        self
    }

    /// Constrain the store's memory predecessor (`inputs[0]`) to a
    /// memory-producing sub-pattern (a `store` / `mem_phi` / `call`).
    #[must_use]
    pub fn mem_in<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.mem_in = Some(Box::new(move |n: NodePat| n.input_mem(0, p)));
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

    /// Translate the accumulated filters into a configured [`NodePat`]
    /// (a `Store`-kind node root with a memory token at output slot 0).
    fn configured(self) -> NodePat {
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
        // The new memory token lives at output slot 0.
        let mut n = NodePat::node(load_store_kind(exemplar, space)).with_mem_out(0);
        if let Some(m) = mem_in {
            n = m(n);
        }
        if let Some(a) = addr {
            n = a(n);
        }
        if let Some(d) = data {
            n = d(n);
        } else if bit_width.is_some() {
            // No explicit data sub-pattern, but a width constraint needs a
            // wired producer output at the data slot to pin the width on.
            n = n.input(2, crate::typed::wildcards::any());
        }
        if let Some(c) = capture {
            n = n.capture(c);
        }
        if let Some(w) = bit_width {
            // The stored value is the Store's data input (`inputs[2]`), so
            // pin that input's producer-output width declaratively.
            n = n.with_input_width(2, w);
        }
        if stack.active() {
            // The SP-relative stack filter is an irreducible
            // `Function::stack_offset` side-table lookup.
            n = n.with_node_limit(move || {
                Box::new(move |matcher, node, _ty| stack.check(matcher.function(), node))
            });
        }
        n
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the
    /// `Store` node (a memory-token root, no value output).
    #[must_use]
    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MemPat for StorePat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatOutRef {
        self.configured().lower(b).mem_out()
    }
}

/// Construct a fresh [`StorePat`].
#[must_use]
pub fn store() -> StorePat {
    StorePat::default()
}
