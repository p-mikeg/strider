//! Slot conventions, per the IR `expected_signature`:
//!
//! * `Load` inputs `[mem(0), addr(1)]`, output the loaded value at slot 0.
//! * `Store` inputs `[mem(0), addr(1), data(2)]`, output the new memory token
//!   at slot 0.
//!
//! `Load` is value-producing and nests as a value operand; `Store` is a
//! memory-token root exposing its token via [`MemPat`].

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef, Pattern};

use super::MemPat;
use super::flow::{OutputPat, WithOutput};
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

/// What `LoadPat` and `StorePat` share verbatim, embedded as `common`.
#[derive(Default)]
struct MemAccessSpec {
    space: Option<rsleigh::VnSpace>,
    mem: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
    /// Applied in order; each call adds a separate constraint.
    any_input: Vec<Box<dyn FnOnce(NodePat) -> NodePat>>,
    /// Raw input and sibling-output slots, from `input` / `output`.
    slots: Vec<Box<dyn FnOnce(NodePat) -> NodePat>>,
    bit_width: Option<u32>,
    region: RegionFilter,
    capture: Option<Capture>,
}

impl MemAccessSpec {
    fn wire_mem_and_capture(&mut self, mut n: NodePat) -> NodePat {
        if let Some(m) = self.mem.take() {
            n = m(n);
        }
        for f in self.any_input.drain(..).chain(self.slots.drain(..)) {
            n = f(n);
        }
        if let Some(c) = self.capture {
            n = n.capture(c);
        }
        n
    }

    fn apply_region(self, n: NodePat) -> NodePat {
        self.region.apply(n)
    }
}

/// Inputs `[mem(0), addr(1)]`, single output the loaded value.
#[derive(Default)]
pub struct LoadPat {
    common: MemAccessSpec,
    addr: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
}

impl LoadPat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.common.space = Some(s);
        self
    }

    /// `inputs[1]`.
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.addr = Some(Box::new(move |n: NodePat| n.input(1, p)));
        self
    }

    /// `inputs[0]`, taking a `store` / `mem_phi` / `call`.
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.common.mem = Some(Box::new(move |n: NodePat| n.input_mem(0, p)));
        self
    }

    /// Candidates are mem and addr. A typed value sub binds only addr;
    /// `var` / `anything` also reaches the memory edge. Repeatable.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.common
            .any_input
            .push(Box::new(move |n: NodePat| n.input_any(p)));
        self
    }

    /// Pins the value output's width.
    pub fn bit_width(mut self, n: u32) -> Self {
        self.common.bit_width = Some(n);
        self
    }

    /// Requires the address to decompose to exactly `sp + k`. Replaces any
    /// region filter set before it.
    pub fn stack_offset(mut self, k: i128) -> Self {
        self.common.region.set_stack_offset(k);
        self
    }

    /// Keeps only accesses whose address decomposes to a stack base, keeping an
    /// offset a preceding [`stack_offset`](Self::stack_offset) pinned. The one
    /// region filter that does not discard what came before.
    pub fn stack_only(mut self) -> Self {
        self.common.region.set_stack_only();
        self
    }

    /// Keeps only accesses proven to be heap-rooted or not memory-rooted.
    /// An address with no decomposition verdict is rejected. Replaces any
    /// region filter set before it.
    pub fn non_stack(mut self) -> Self {
        self.common.region.set_non_stack();
        self
    }

    /// Keeps only accesses whose address decomposes to a heap base (a pure
    /// allocator's return pointer). Replaces any region filter set before it.
    pub fn heap_only(mut self) -> Self {
        self.common.region.set_heap_only();
        self
    }

    /// Raw input slot `slot`. Slot numbering is per node kind, laid out by the
    /// IR's `expected_signature`; the named accessors above are the intended
    /// surface and this is the escape hatch beneath them.
    pub fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.input(slot, p)));
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
        self.common.capture = Some(c);
        self
    }

    fn configured(self) -> NodePat {
        let LoadPat { mut common, addr } = self;
        let exemplar = NodeKind::Load(rsleigh::VnSpace::RAM);
        let mut n = NodePat::value(load_store_kind(exemplar, common.space), 0);
        n = common.wire_mem_and_capture(n);
        if let Some(a) = addr {
            n = a(n);
        }
        if let Some(w) = common.bit_width {
            // The loaded value IS the anchor output, so pin it there.
            n = n.with_output_width(w);
        }
        common.apply_region(n)
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
    LoadPat::default()
}

impl WithOutput for LoadPat {
    fn capture_output(mut self, slot: Option<usize>, c: Capture) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.capture_output(slot, c)));
        self
    }
    fn output_width(mut self, slot: Option<usize>, bits: u32) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.output_width(slot, bits)));
        self
    }
    fn output_ty(mut self, slot: Option<usize>, ty: strider_ir::node::ValueType) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.output_ty(slot, ty)));
        self
    }
}

/// Inputs `[mem(0), addr(1), data(2)]`, single output the new memory token.
#[derive(Default)]
pub struct StorePat {
    common: MemAccessSpec,
    addr: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
    data: Option<Box<dyn FnOnce(NodePat) -> NodePat>>,
}

impl StorePat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.common.space = Some(s);
        self
    }

    /// `inputs[1]`.
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.addr = Some(Box::new(move |n: NodePat| n.input(1, p)));
        self
    }

    /// The stored value, `inputs[2]`.
    pub fn data<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.data = Some(Box::new(move |n: NodePat| n.input(2, p)));
        self
    }

    /// `inputs[0]`, taking a `store` / `mem_phi` / `call`.
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.common.mem = Some(Box::new(move |n: NodePat| n.input_mem(0, p)));
        self
    }

    /// Candidates are mem, addr and data. A typed value sub binds only addr
    /// or data; `var` / `anything` also reaches the memory edge. Repeatable.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.common
            .any_input
            .push(Box::new(move |n: NodePat| n.input_any(p)));
        self
    }

    /// Pins the width of the data input, `inputs[2]`.
    pub fn bit_width(mut self, n: u32) -> Self {
        self.common.bit_width = Some(n);
        self
    }

    /// Requires the address to decompose to exactly `sp + k`. Replaces any
    /// region filter set before it.
    pub fn stack_offset(mut self, k: i128) -> Self {
        self.common.region.set_stack_offset(k);
        self
    }

    /// Keeps only accesses whose address decomposes to a stack base, keeping an
    /// offset a preceding [`stack_offset`](Self::stack_offset) pinned. The one
    /// region filter that does not discard what came before.
    pub fn stack_only(mut self) -> Self {
        self.common.region.set_stack_only();
        self
    }

    /// Keeps only accesses proven to be heap-rooted or not memory-rooted.
    /// An address with no decomposition verdict is rejected. Replaces any
    /// region filter set before it.
    pub fn non_stack(mut self) -> Self {
        self.common.region.set_non_stack();
        self
    }

    /// Keeps only accesses whose address decomposes to a heap base (a pure
    /// allocator's return pointer). Replaces any region filter set before it.
    pub fn heap_only(mut self) -> Self {
        self.common.region.set_heap_only();
        self
    }

    /// Raw input slot `slot`. Slot numbering is per node kind, laid out by the
    /// IR's `expected_signature`; the named accessors above are the intended
    /// surface and this is the escape hatch beneath them.
    pub fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.input(slot, p)));
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
        self.common.capture = Some(c);
        self
    }

    fn configured(self) -> NodePat {
        let StorePat {
            mut common,
            addr,
            data,
        } = self;
        let exemplar = NodeKind::Store(rsleigh::VnSpace::RAM);
        let mut n = NodePat::node(load_store_kind(exemplar, common.space)).with_mem_value(0);
        n = common.wire_mem_and_capture(n);
        if let Some(a) = addr {
            n = a(n);
        }
        if let Some(d) = data {
            n = d(n);
        } else if common.bit_width.is_some() {
            // A width constraint needs SOME wired producer output at the data
            // slot to pin itself on.
            n = n.input(2, crate::typed::wildcards::anything());
        }
        if let Some(w) = common.bit_width {
            // The stored value is an input, not an output, so the width pins
            // on that input's producer output.
            n = n.with_input_width(2, w);
        }
        common.apply_region(n)
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
    StorePat::default()
}

impl WithOutput for StorePat {
    fn capture_output(mut self, slot: Option<usize>, c: Capture) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.capture_output(slot, c)));
        self
    }
    fn output_width(mut self, slot: Option<usize>, bits: u32) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.output_width(slot, bits)));
        self
    }
    fn output_ty(mut self, slot: Option<usize>, ty: strider_ir::node::ValueType) -> Self {
        self.common
            .slots
            .push(Box::new(move |n: NodePat| n.output_ty(slot, ty)));
        self
    }
}
