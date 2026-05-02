//! Memory-access builders: `LoadPat`, `StorePat`, `StackStorePat`,
//! `StackStorePhiPat`.  All use `InputsSpec::Indexed` for sparse positional
//! sub-pattern constraints.
//!
//! Capture the matched output with `.capture(v)` from
//! [`crate::pat::IntoPat`].  Value-kind filtering at bind time (see
//! [`crate::pat::any::CapturePat`]) ensures the captured `NodeOutputId`
//! refers to the node's value output, not its memory / control slots.

use std::sync::Arc;

use ir::node::NodeKind;

use crate::pat::Pat;
use crate::pat::node_pat::{InputsSpec, KindSpec, NodeKindCheck, NodePat};

// ── LoadPat ───────────────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`crate::pat::load`].
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
}

impl LoadPat {
    pub(crate) fn new() -> Self {
        Self { space: None, addr: None }
    }
    /// Restrict the match to loads in address space `s`.
    #[must_use]
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

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        let LoadPat { space, addr } = b;
        // Load inputs = [mem(0), addr(1)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        let kind = match space {
            None => KindSpec::variant(&NodeKind::Load(rsleigh::VnSpace::RAM)),
            Some(s) => KindSpec::variant_with(
                &NodeKind::Load(rsleigh::VnSpace::RAM),
                move |k| matches!(k, NodeKind::Load(actual) if *actual == s),
            ),
        };
        NodePat::matcher(kind, InputsSpec::Indexed(indexed)).into_pat()
    }
}

// ── StorePat ──────────────────────────────────────────────────────────────────

/// Builder for `Store` node patterns.  Created by [`crate::pat::store`].
pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    data: Option<Pat>,
}

impl StorePat {
    pub(crate) fn new() -> Self {
        Self { space: None, addr: None, data: None }
    }
    /// Restrict the match to stores in address space `s`.
    #[must_use]
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

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        let StorePat { space, addr, data } = b;
        // Store inputs = [mem(0), addr(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(addr_pat) = addr {
            indexed.push((1, addr_pat));
        }
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        let kind = match space {
            None => KindSpec::variant(&NodeKind::Store(rsleigh::VnSpace::RAM)),
            Some(s) => KindSpec::variant_with(
                &NodeKind::Store(rsleigh::VnSpace::RAM),
                move |k| matches!(k, NodeKind::Store(actual) if *actual == s),
            ),
        };
        NodePat::matcher(kind, InputsSpec::Indexed(indexed)).into_pat()
    }
}

// ── StackStorePat ─────────────────────────────────────────────────────────────

/// Builder for `StackStore` node patterns.  Created by [`crate::pat::stack_store`].
pub struct StackStorePat {
    space: Option<rsleigh::VnSpace>,
    offset: Option<i64>,
    /// Set-membership constraint over the offset, applied alongside
    /// (and AND-combined with) [`Self::offset`] when both are set.
    /// `Some(empty)` means "match nothing" (vacuous false); `None`
    /// means "no set constraint".
    offset_any: Option<Vec<i64>>,
    data: Option<Pat>,
}

impl StackStorePat {
    pub(crate) fn new() -> Self {
        Self { space: None, offset: None, offset_any: None, data: None }
    }
    /// Restrict the match to stack-stores in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Match only the stack-store at the given SP-relative offset.
    #[must_use]
    pub fn offset(mut self, o: i64) -> Self {
        self.offset = Some(o);
        self
    }
    /// Match only stack-stores whose offset is in `offsets`.  Useful
    /// when scanning for "any of these known field offsets".  Empty
    /// `offsets` vacuously fails (matches nothing) — mirrors the
    /// contract of [`crate::int_const_any_of`].
    #[must_use]
    pub fn offset_any<I>(mut self, offsets: I) -> Self
    where
        I: IntoIterator<Item = i64>,
    {
        self.offset_any = Some(offsets.into_iter().collect());
        self
    }
    /// Constrain the stored value.
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
}

impl From<StackStorePat> for Pat {
    fn from(b: StackStorePat) -> Pat {
        let StackStorePat { space, offset, offset_any, data } = b;
        // StackStore inputs = [memory(0), base(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        let exemplar = NodeKind::StackStore {
            space: rsleigh::VnSpace::RAM,
            offset: 0,
        };
        let kind = if space.is_none() && offset.is_none() && offset_any.is_none() {
            KindSpec::variant(&exemplar)
        } else {
            KindSpec::variant_with(&exemplar, move |k| {
                let NodeKind::StackStore {
                    space: actual_space,
                    offset: actual_offset,
                } = k
                else {
                    return false;
                };
                if space.is_some_and(|s| *actual_space != s) {
                    return false;
                }
                if offset.is_some_and(|o| *actual_offset != o) {
                    return false;
                }
                if let Some(set) = &offset_any
                    && !set.iter().any(|o| *o == *actual_offset)
                {
                    return false;
                }
                true
            })
        };
        NodePat::matcher(kind, InputsSpec::Indexed(indexed)).into_pat()
    }
}

// ── StackStorePhiPat ──────────────────────────────────────────────────────────

/// Builder for `StackStorePhi` node patterns.  Created by [`crate::pat::stack_store_phi`].
pub struct StackStorePhiPat {
    space: Option<rsleigh::VnSpace>,
    offsets: Option<Vec<i64>>,
    data: Option<Pat>,
}

impl StackStorePhiPat {
    pub(crate) fn new() -> Self {
        Self { space: None, offsets: None, data: None }
    }
    #[must_use]
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

impl From<StackStorePhiPat> for Pat {
    fn from(b: StackStorePhiPat) -> Pat {
        let StackStorePhiPat { space, offsets, data } = b;
        // StackStorePhi inputs = [phi_token(0), memory(1), data(2)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(data_pat) = data {
            indexed.push((2, data_pat));
        }
        let exemplar = NodeKind::StackStorePhi { space: rsleigh::VnSpace::RAM };

        // The space constraint is a pure payload check — handle it in the
        // kind spec.  Offsets need `Graph::stack_phi_offsets(node)` side-table
        // access, so they run in a post_match step.
        let kind = match space {
            None => KindSpec::variant(&exemplar),
            Some(expected) => KindSpec::variant_with(&exemplar, move |k| {
                matches!(k, NodeKind::StackStorePhi { space: actual } if *actual == expected)
            }),
        };

        let pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed));
        let pat = if let Some(expected_offsets) = offsets {
            // `expected_offsets` is already sorted (see
            // `StackStorePhiPat::offsets`).
            let check: NodeKindCheck = Arc::new(move |ctx, node, _b| {
                let actual_slice = ctx.graph.graph.stack_phi_offsets(node);
                if actual_slice.len() != expected_offsets.len() {
                    return false;
                }
                const INLINE: usize = 8;
                if actual_slice.len() <= INLINE {
                    let mut buf = [0i64; INLINE];
                    buf[..actual_slice.len()].copy_from_slice(actual_slice);
                    buf[..actual_slice.len()].sort();
                    &buf[..actual_slice.len()] == expected_offsets.as_slice()
                } else {
                    let mut actual: Vec<i64> = actual_slice.to_vec();
                    actual.sort();
                    actual == expected_offsets
                }
            });
            pat.with_post_match(check)
        } else {
            pat
        };
        pat.into_pat()
    }
}
