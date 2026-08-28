//! Slot conventions, per the IR `expected_signature`:
//!
//! * `Load` inputs `[mem(0), addr(1)]`, output the loaded value at slot 0.
//! * `Store` inputs `[mem(0), addr(1), data(2)]`, output the new memory token
//!   at slot 0.
//!
//! `Load` is value-producing and nests as a value operand; `Store` is a
//! memory-token root exposing its token via [`MemPat`].

use crate::node_builders::delegate_with_output;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef, Pattern};

use super::MemPat;
use super::flow::OutputPat;
use super::node_pat::{KindCheck, NodePat, variant_kind};

/// The address-region filter of a `Load`/`Store` pattern: a mutually-exclusive
/// choice, so the state can hold only one at a time and the last builder call
/// wins. Every arm but `Any` demands a decomposition verdict, so on a function
/// whose memory side-table is not populated they all see `Unknown` and match
/// nothing.
#[derive(Default)]
enum RegionFilter {
    #[default]
    Any,
    /// Address decomposes to a stack base; `Some(k)` also requires exactly
    /// `sp + k`.
    Stack { offset: Option<i128> },
    /// Address decomposes to a heap base or is proven not memory-rooted.
    NonStack,
    /// Address decomposes to a heap base.
    Heap,
}

impl RegionFilter {
    fn active(&self) -> bool {
        !matches!(self, RegionFilter::Any)
    }

    fn check(&self, function: &strider_ir::Function, node: NodeId) -> bool {
        use strider_ir::MemDecomp;
        let Some(addr) = access_address(function, node) else {
            return matches!(self, RegionFilter::Any);
        };
        let (class, slot) = function.side_tables().memory_decomp(addr);
        match self {
            RegionFilter::Any => true,
            RegionFilter::Heap => matches!(class, MemDecomp::Heap(_)),
            RegionFilter::NonStack => {
                matches!(class, MemDecomp::Heap(_) | MemDecomp::NotMemory)
            }
            RegionFilter::Stack { offset } => {
                matches!(class, MemDecomp::Stack(_))
                    && offset.is_none_or(|k| slot.is_some_and(|(_base, o)| o == k))
            }
        }
    }

    fn set_stack_offset(&mut self, k: i128) {
        *self = RegionFilter::Stack { offset: Some(k) };
    }

    /// Keeps an offset already pinned by `stack_offset`, so
    /// `.stack_offset(k).stack_only()` still requires `k`.
    fn set_stack_only(&mut self) {
        if !matches!(self, RegionFilter::Stack { .. }) {
            *self = RegionFilter::Stack { offset: None };
        }
    }

    fn set_heap_only(&mut self) {
        *self = RegionFilter::Heap;
    }

    fn set_non_stack(&mut self) {
        *self = RegionFilter::NonStack;
    }

    /// A no-op when inactive.
    fn apply(self, n: NodePat) -> NodePat {
        if !self.active() {
            return n;
        }
        n.with_node_predicate(move || {
            Box::new(move |matcher, node| self.check(matcher.function(), node))
        })
    }
}

/// The address of a `Load` / `Store`, per the slot conventions above.
fn access_address(function: &strider_ir::Function, node: NodeId) -> Option<ValueId> {
    if !matches!(
        function.node_kind(node),
        NodeKind::Load(_) | NodeKind::Store(_)
    ) {
        return None;
    }
    function.node_inputs(node).get(1).copied()
}

/// Variant-agnostic without a space constraint; with one, pins the exact
/// `VnSpace`.
fn load_store_kind(exemplar: NodeKind, space: Option<rsleigh::VnSpace>) -> KindSpec {
    let discriminant = std::mem::discriminant(&exemplar);
    let is_load = matches!(exemplar, NodeKind::Load(_));
    let check = space.map(|s| {
        let check: KindCheck = Box::new(move |k| {
            matches!((is_load, k),
                (true, NodeKind::Load(actual)) | (false, NodeKind::Store(actual))
                    if *actual == s)
        });
        check
    });
    variant_kind(discriminant, check)
}

/// Inputs `[mem(0), addr(1)]`, single output the loaded value.
///
/// Holds its `NodePat` eagerly. `space` re-narrows the kind in place rather
/// than deferring every setter behind a boxed closure to be replayed once the
/// space is known.
pub struct LoadPat {
    inner: NodePat,
    bit_width: Option<u32>,
    region: RegionFilter,
}

impl LoadPat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.inner = self.inner.with_kind(load_store_kind(
            NodeKind::Load(rsleigh::VnSpace::RAM),
            Some(s),
        ));
        self
    }

    /// `inputs[1]`.
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(1, p);
        self
    }

    /// `inputs[0]`, taking a `store` / `mem_phi` / `call`.
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(0, p);
        self
    }

    /// Candidates are mem and addr. A typed value sub binds only addr;
    /// `var` / `anything` also reaches the memory edge. Repeatable.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_any(p);
        self
    }

    /// Pins the value output's width.
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }

    /// Requires the address to decompose to exactly `sp + k`. Replaces any
    /// region filter set before it.
    pub fn stack_offset(mut self, k: i128) -> Self {
        self.region.set_stack_offset(k);
        self
    }

    /// Keeps only accesses whose address decomposes to a stack base, keeping an
    /// offset a preceding [`stack_offset`](Self::stack_offset) pinned. The one
    /// region filter that does not discard what came before.
    pub fn stack_only(mut self) -> Self {
        self.region.set_stack_only();
        self
    }

    /// Keeps only accesses proven to be heap-rooted or not memory-rooted.
    /// An address with no decomposition verdict is rejected. Replaces any
    /// region filter set before it.
    pub fn non_stack(mut self) -> Self {
        self.region.set_non_stack();
        self
    }

    /// Keeps only accesses whose address decomposes to a heap base (a pure
    /// allocator's return pointer). Replaces any region filter set before it.
    pub fn heap_only(mut self) -> Self {
        self.region.set_heap_only();
        self
    }

    /// Raw input slot `slot`. Slot numbering is per node kind, laid out by the
    /// IR's `expected_signature`; the named accessors above are the intended
    /// surface and this is the escape hatch beneath them.
    pub fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inner = self.inner.input(slot, p);
        self
    }

    /// The one output, at slot 0. Returns a terminal taking one of
    /// `.capture(c)`, `.of_width(w)`, `.of_type(ty)`.
    pub fn output(self, slot: usize) -> OutputPat<Self> {
        OutputPat::at(self, Some(slot))
    }

    /// Some output rather than a fixed slot; otherwise
    /// [`output`](Self::output).
    pub fn any_output(self) -> OutputPat<Self> {
        OutputPat::at(self, None)
    }

    /// Binds the value output.
    pub fn capture(mut self, c: Capture) -> Self {
        self.inner = self.inner.capture(c);
        self
    }

    fn configured(self) -> NodePat {
        let LoadPat {
            mut inner,
            bit_width,
            region,
        } = self;
        if let Some(w) = bit_width {
            // The loaded value IS the anchor output, so pin it there.
            inner = inner.with_output_width(w);
        }
        region.apply(inner)
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MatchPat for LoadPat {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_anchored(b)
    }
}

pub fn load() -> LoadPat {
    LoadPat {
        inner: NodePat::value(
            load_store_kind(NodeKind::Load(rsleigh::VnSpace::RAM), None),
            0,
        ),
        bit_width: None,
        region: RegionFilter::default(),
    }
}

delegate_with_output!(LoadPat, inner);

/// Inputs `[mem(0), addr(1), data(2)]`, single output the new memory token.
pub struct StorePat {
    inner: NodePat,
    /// A width needs SOME wired producer at the data slot to pin on, so
    /// `configured` synthesises one when `data` was never called.
    has_data: bool,
    bit_width: Option<u32>,
    region: RegionFilter,
}

impl StorePat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.inner = self.inner.with_kind(load_store_kind(
            NodeKind::Store(rsleigh::VnSpace::RAM),
            Some(s),
        ));
        self
    }

    /// `inputs[1]`.
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(1, p);
        self
    }

    /// The stored value, `inputs[2]`.
    pub fn data<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(2, p);
        self.has_data = true;
        self
    }

    /// `inputs[0]`, taking a `store` / `mem_phi` / `call`.
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(0, p);
        self
    }

    /// Candidates are mem, addr and data. A typed value sub binds only addr
    /// or data; `var` / `anything` also reaches the memory edge. Repeatable.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_any(p);
        self
    }

    /// Pins the width of the data input, `inputs[2]`.
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }

    /// Requires the address to decompose to exactly `sp + k`. Replaces any
    /// region filter set before it.
    pub fn stack_offset(mut self, k: i128) -> Self {
        self.region.set_stack_offset(k);
        self
    }

    /// Keeps only accesses whose address decomposes to a stack base, keeping an
    /// offset a preceding [`stack_offset`](Self::stack_offset) pinned. The one
    /// region filter that does not discard what came before.
    pub fn stack_only(mut self) -> Self {
        self.region.set_stack_only();
        self
    }

    /// Keeps only accesses proven to be heap-rooted or not memory-rooted.
    /// An address with no decomposition verdict is rejected. Replaces any
    /// region filter set before it.
    pub fn non_stack(mut self) -> Self {
        self.region.set_non_stack();
        self
    }

    /// Keeps only accesses whose address decomposes to a heap base (a pure
    /// allocator's return pointer). Replaces any region filter set before it.
    pub fn heap_only(mut self) -> Self {
        self.region.set_heap_only();
        self
    }

    /// Raw input slot `slot`. Slot numbering is per node kind, laid out by the
    /// IR's `expected_signature`; the named accessors above are the intended
    /// surface and this is the escape hatch beneath them.
    pub fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inner = self.inner.input(slot, p);
        self
    }

    /// The one output, at slot 0. Returns a terminal taking one of
    /// `.capture(c)`, `.of_width(w)`, `.of_type(ty)`.
    pub fn output(self, slot: usize) -> OutputPat<Self> {
        OutputPat::at(self, Some(slot))
    }

    /// Some output rather than a fixed slot; otherwise
    /// [`output`](Self::output).
    pub fn any_output(self) -> OutputPat<Self> {
        OutputPat::at(self, None)
    }

    pub fn capture(mut self, c: Capture) -> Self {
        self.inner = self.inner.capture(c);
        self
    }

    fn configured(self) -> NodePat {
        let StorePat {
            mut inner,
            has_data,
            bit_width,
            region,
        } = self;
        if let Some(w) = bit_width {
            if !has_data {
                inner = inner.input(2, crate::typed::wildcards::anything());
            }
            // The stored value is an input, not an output, so the width pins
            // on that input's producer output.
            inner = inner.with_input_width(2, w);
        }
        region.apply(inner)
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MatchPat for StorePat {
    /// The new memory token is the only output, so a value slot never matches.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_anchored(b)
    }
}

impl MemPat for StorePat {}

pub fn store() -> StorePat {
    StorePat {
        inner: NodePat::node(load_store_kind(
            NodeKind::Store(rsleigh::VnSpace::RAM),
            None,
        ))
        .with_mem_value(0),
        has_data: false,
        bit_width: None,
        region: RegionFilter::default(),
    }
}

delegate_with_output!(StorePat, inner);
