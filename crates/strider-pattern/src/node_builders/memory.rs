//! Slot conventions, per the IR `expected_signature`:
//!
//! * `Load` inputs `[mem(0), addr(1)]`, output the loaded value at slot 0.
//! * `Store` inputs `[mem(0), addr(1), data(2)]`, output the new memory token
//!   at slot 0.
//!
//! `Load` is value-producing and nests as a value operand; `Store` is a
//! memory-token root exposing its token via [`MemPat`].

use super::{delegate_node_pat, node_match_pat};
use crate::node_builders::delegate_with_output;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, Pattern};

use super::MemPat;
use crate::expr::Node;

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
        let Some(addr) = function.access_address(node) else {
            return matches!(self, RegionFilter::Any);
        };
        let (class, slot) = function.memory_decomp(addr);
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
    fn apply(self, n: Node) -> Node {
        if !self.active() {
            return n;
        }
        n.with_node_predicate(Box::new(move |matcher, node| {
            self.check(matcher.function(), node)
        }))
    }
}

/// The region filter every builder that holds a `RegionFilter` exposes
/// verbatim. `$region` names the field.
macro_rules! delegate_region_filter {
    ($ty:ty, $region:ident) => {
        impl $ty {
            /// Requires the address to decompose to exactly `sp + k`. Replaces any
            /// region filter set before it.
            pub fn stack_offset(mut self, k: i128) -> Self {
                self.$region.set_stack_offset(k);
                self
            }

            /// Keeps only accesses whose address decomposes to a stack base, keeping an
            /// offset a preceding [`stack_offset`](Self::stack_offset) pinned. The one
            /// region filter that does not discard what came before.
            pub fn stack_only(mut self) -> Self {
                self.$region.set_stack_only();
                self
            }

            /// Keeps only accesses proven to be heap-rooted or not memory-rooted.
            /// An address with no decomposition verdict is rejected. Replaces any
            /// region filter set before it.
            pub fn non_stack(mut self) -> Self {
                self.$region.set_non_stack();
                self
            }

            /// Keeps only accesses whose address decomposes to a heap base (a pure
            /// allocator's return pointer). Replaces any region filter set before it.
            pub fn heap_only(mut self) -> Self {
                self.$region.set_heap_only();
                self
            }
        }
    };
}

/// Inputs `[mem(0), addr(1)]`, single output the loaded value.
pub struct LoadPat {
    inner: Node,
    region: RegionFilter,
}

impl LoadPat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.inner = self.inner.with_kind(KindSpec::Exact(NodeKind::Load(s)));
        self
    }

    /// The address, input [`NodeKind::ACCESS_ADDR_SLOT`].
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(NodeKind::ACCESS_ADDR_SLOT, p);
        self
    }

    /// Input [`NodeKind::ACCESS_MEM_SLOT`], taking a `store` / `mem_phi` /
    /// `call`.
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(NodeKind::ACCESS_MEM_SLOT, p);
        self
    }

    /// Pins the value output's width, which is the anchor.
    pub fn bit_width(mut self, n: u32) -> Self {
        self.inner = self.inner.with_output_width(n);
        self
    }

    fn configured(self) -> Node {
        self.region.apply(self.inner)
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

pub fn load() -> LoadPat {
    LoadPat {
        inner: Node::value(
            KindSpec::variant_of(&NodeKind::Load(rsleigh::VnSpace::RAM)),
            0,
        ),
        region: RegionFilter::default(),
    }
}

delegate_with_output!(LoadPat, inner);
delegate_node_pat!(LoadPat, inner, [capture, input, any_input]);
delegate_region_filter!(LoadPat, region);

/// Inputs `[mem(0), addr(1), data(2)]`, single output the new memory token.
pub struct StorePat {
    inner: Node,
    /// A width needs SOME wired producer at the data slot to pin on, so
    /// `configured` synthesises one when `data` was never called.
    has_data: bool,
    bit_width: Option<u32>,
    region: RegionFilter,
}

impl StorePat {
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.inner = self.inner.with_kind(KindSpec::Exact(NodeKind::Store(s)));
        self
    }

    /// The address, input [`NodeKind::ACCESS_ADDR_SLOT`].
    pub fn addr<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(NodeKind::ACCESS_ADDR_SLOT, p);
        self
    }

    /// The stored value, input [`NodeKind::STORE_DATA_SLOT`].
    pub fn data<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(NodeKind::STORE_DATA_SLOT, p);
        self.has_data = true;
        self
    }

    /// Input [`NodeKind::ACCESS_MEM_SLOT`], taking a `store` / `mem_phi` /
    /// `call`.
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(NodeKind::ACCESS_MEM_SLOT, p);
        self
    }

    /// Pins the width of the data input, [`NodeKind::STORE_DATA_SLOT`].
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }

    fn configured(self) -> Node {
        let StorePat {
            mut inner,
            has_data,
            bit_width,
            region,
        } = self;
        if let Some(w) = bit_width {
            if !has_data {
                inner = inner.input(
                    NodeKind::STORE_DATA_SLOT,
                    crate::typed::wildcards::anything(),
                );
            }
            // The stored value is an input, not an output, so the width pins
            // on that input's producer output.
            inner = inner.with_input_width(NodeKind::STORE_DATA_SLOT, w);
        }
        region.apply(inner)
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MemPat for StorePat {}

pub fn store() -> StorePat {
    StorePat {
        inner: Node::new(KindSpec::variant_of(&NodeKind::Store(
            rsleigh::VnSpace::RAM,
        )))
        .with_mem_value(NodeKind::MEM_OUT_SLOT),
        has_data: false,
        bit_width: None,
        region: RegionFilter::default(),
    }
}

delegate_with_output!(StorePat, inner);
delegate_node_pat!(StorePat, inner, [capture, input, any_input]);
delegate_region_filter!(StorePat, region);

node_match_pat! {
    LoadPat => |p| p.configured(),
    StorePat => |p| p.configured(),
}
