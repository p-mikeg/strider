//! Memory-access builders: `LoadPat`, `StorePat`, `StackStorePat`,
//! `StackStorePhiPat`.  All use `InputsSpec::Indexed` for sparse positional
//! sub-pattern constraints.

use std::sync::Arc;

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::builders::CaptureBuilder;
use crate::pat::node_pat::{InputsSpec, KindFilter, NodePat};
use crate::var::{NodeVar, Var};

// ── LoadPat ───────────────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`crate::pat::load`].
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl LoadPat {
    pub(crate) fn new() -> Self {
        Self { space: None, addr: None, output_var: None, node_var: None }
    }
    /// Restrict the match to loads in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Constrain the load's address operand.
    pub fn addr(mut self, p: impl Into<Pat>) -> Self {
        self.addr = Some(p.into());
        self
    }
}

impl CaptureBuilder for LoadPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        let LoadPat { space, addr, output_var, node_var } = b;
        // Load inputs = [mem(0), addr(1)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        NodePat::matcher(
            KindFilter::exact(&NodeKind::Load(rsleigh::VnSpace::RAM)),
            Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::Load(actual) if space.is_none_or(|s| *actual == s)
                )
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
    }
}

// ── StorePat ──────────────────────────────────────────────────────────────────

/// Builder for `Store` node patterns.  Created by [`crate::pat::store`].
pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    data: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl StorePat {
    pub(crate) fn new() -> Self {
        Self { space: None, addr: None, data: None, output_var: None, node_var: None }
    }
    /// Restrict the match to stores in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Constrain the store's address operand.
    pub fn addr(mut self, p: impl Into<Pat>) -> Self {
        self.addr = Some(p.into());
        self
    }
    /// Constrain the value being stored.
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
}

impl CaptureBuilder for StorePat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        let StorePat { space, addr, data, output_var, node_var } = b;
        // Store inputs = [mem(0), addr(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        NodePat::matcher(
            KindFilter::exact(&NodeKind::Store(rsleigh::VnSpace::RAM)),
            Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::Store(actual) if space.is_none_or(|s| *actual == s)
                )
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
    }
}

// ── StackStorePat ─────────────────────────────────────────────────────────────

/// Builder for `StackStore` node patterns.  Created by [`crate::pat::stack_store`].
pub struct StackStorePat {
    space: Option<rsleigh::VnSpace>,
    offset: Option<i64>,
    data: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl StackStorePat {
    pub(crate) fn new() -> Self {
        Self { space: None, offset: None, data: None, output_var: None, node_var: None }
    }
    /// Restrict the match to stack-stores in address space `s`.
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Match only the stack-store at the given SP-relative offset.
    pub fn offset(mut self, o: i64) -> Self {
        self.offset = Some(o);
        self
    }
    /// Constrain the stored value.
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
}

impl CaptureBuilder for StackStorePat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<StackStorePat> for Pat {
    fn from(b: StackStorePat) -> Pat {
        let StackStorePat { space, offset, data, output_var, node_var } = b;
        // StackStore inputs = [memory(0), base(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        NodePat::matcher(
            KindFilter::exact(&NodeKind::StackStore {
                space: rsleigh::VnSpace::RAM,
                offset: 0,
            }),
            Arc::new(move |ctx, node, _b| {
                matches!(
                    ctx.graph.graph.node_kind(node),
                    NodeKind::StackStore { space: actual_space, offset: actual_offset }
                        if space.is_none_or(|s| *actual_space == s)
                            && offset.is_none_or(|o| *actual_offset == o)
                )
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
    }
}

// ── StackStorePhiPat ──────────────────────────────────────────────────────────

/// Builder for `StackStorePhi` node patterns.  Created by [`crate::pat::stack_store_phi`].
pub struct StackStorePhiPat {
    space: Option<rsleigh::VnSpace>,
    offsets: Option<Vec<i64>>,
    data: Option<Pat>,
    output_var: Option<Var>,
    node_var: Option<NodeVar>,
}

impl StackStorePhiPat {
    pub(crate) fn new() -> Self {
        Self { space: None, offsets: None, data: None, output_var: None, node_var: None }
    }
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
    /// Match the per-branch offsets exactly (multiset comparison).  The
    /// supplied list is sorted ascending before comparison, so caller order
    /// is irrelevant.
    pub fn offsets<I: IntoIterator<Item = i64>>(mut self, os: I) -> Self {
        let mut v: Vec<i64> = os.into_iter().collect();
        v.sort();
        self.offsets = Some(v);
        self
    }
}

impl CaptureBuilder for StackStorePhiPat {
    fn output_slot(&mut self) -> &mut Option<Var> { &mut self.output_var }
    fn node_slot(&mut self) -> &mut Option<NodeVar> { &mut self.node_var }
}

impl From<StackStorePhiPat> for Pat {
    fn from(b: StackStorePhiPat) -> Pat {
        let StackStorePhiPat { space, offsets, data, output_var, node_var } = b;
        // StackStorePhi inputs = [phi_token(0), memory(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        NodePat::matcher(
            KindFilter::exact(&NodeKind::StackStorePhi { space: rsleigh::VnSpace::RAM }),
            Arc::new(move |ctx, node, _b| {
                let NodeKind::StackStorePhi { space: actual_space } =
                    ctx.graph.graph.node_kind(node)
                else {
                    return false;
                };
                if let Some(expected_space) = space
                    && *actual_space != expected_space
                {
                    return false;
                }
                if let Some(expected_offsets) = &offsets {
                    // `expected_offsets` is already sorted (see
                    // `StackStorePhiPat::offsets`).  Compare as multisets
                    // without allocating: skip on length mismatch, then sort
                    // a fixed-size stack buffer for small arities and fall
                    // back to a heap Vec only in the unlikely arity > 8 case.
                    let actual_slice = ctx.graph.graph.stack_phi_offsets(node);
                    if actual_slice.len() != expected_offsets.len() {
                        return false;
                    }
                    const INLINE: usize = 8;
                    if actual_slice.len() <= INLINE {
                        let mut buf = [0i64; INLINE];
                        buf[..actual_slice.len()].copy_from_slice(actual_slice);
                        buf[..actual_slice.len()].sort();
                        if &buf[..actual_slice.len()] != expected_offsets.as_slice() {
                            return false;
                        }
                    } else {
                        let mut actual: Vec<i64> = actual_slice.to_vec();
                        actual.sort();
                        if &actual != expected_offsets {
                            return false;
                        }
                    }
                }
                true
            }),
            InputsSpec::Indexed(indexed),
        )
        .with_output_var(output_var)
        .with_node_var(node_var)
        .into_pat()
    }
}
