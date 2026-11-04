use super::{delegate_node_pat, node_match_pat};
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, Pattern};

use super::MemPat;
use crate::expr::Node;

/// A `Load`/`Store` address-region filter; the last builder call wins. Every
/// arm but `Any` needs a decomposition verdict, so without one it matches nothing.
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

    fn apply(self, n: Node) -> Node {
        if matches!(self, RegionFilter::Any) {
            return n;
        }
        n.with_node_predicate(Box::new(move |matcher, node| {
            self.check(matcher.function(), node)
        }))
    }
}

/// The region-filter builder methods; `$region` names the field.
macro_rules! delegate_region_filter {
    ($ty:ty, $region:ident) => {
        impl $ty {
            /// Requires the address to decompose to exactly `sp + k`. Replaces any
            /// earlier region filter.
            pub fn stack_offset(mut self, k: i128) -> Self {
                self.$region = RegionFilter::Stack { offset: Some(k) };
                self
            }

            /// Keeps only accesses whose address decomposes to a stack base. Keeps an
            /// earlier [`stack_offset`](Self::stack_offset); replaces any other filter.
            pub fn stack_only(mut self) -> Self {
                if !matches!(self.$region, RegionFilter::Stack { .. }) {
                    self.$region = RegionFilter::Stack { offset: None };
                }
                self
            }

            /// Keeps only accesses proven heap-rooted or not memory-rooted. Replaces
            /// any earlier region filter.
            pub fn non_stack(mut self) -> Self {
                self.$region = RegionFilter::NonStack;
                self
            }

            /// Keeps only accesses whose address decomposes to a heap base. Replaces
            /// any earlier region filter.
            pub fn heap_only(mut self) -> Self {
                self.$region = RegionFilter::Heap;
                self
            }
        }
    };
}

/// A `Load`: inputs `[mem, addr]`, output the loaded value.
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

    /// Input [`NodeKind::ACCESS_MEM_SLOT`].
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(NodeKind::ACCESS_MEM_SLOT, p);
        self
    }

    /// Pins the loaded value's width.
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

delegate_node_pat! { LoadPat, inner, [capture, input, any_input], with_output }
delegate_region_filter!(LoadPat, region);

/// A `Store`: inputs `[mem, addr, data]`, output the new memory token.
pub struct StorePat {
    inner: Node,
    /// Whether `data` was set; a width needs a data producer to pin on.
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

    /// Input [`NodeKind::ACCESS_MEM_SLOT`].
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(NodeKind::ACCESS_MEM_SLOT, p);
        self
    }

    /// Pins the stored value's width.
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

delegate_node_pat! { StorePat, inner, [capture, input, any_input], with_output }
delegate_region_filter!(StorePat, region);

node_match_pat! {
    LoadPat => |p| p.configured(),
    StorePat => |p| p.configured(),
}
