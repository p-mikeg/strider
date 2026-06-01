//! Memory-op chained builders: `LoadPat`, `StorePat`.
//!
//! Both builders accumulate sparse positional sub-pattern constraints
//! and merge each child `PatGraph` into the parent on `.into()`.  The
//! kind discriminant is `Load(VnSpace)` / `Store(VnSpace)`; an exact
//! address space is recorded via `KindSpec::VariantWith` so the space
//! check fires at kind-match time (no `post_match` widening needed).
//!
//! ## Role handling
//!
//! Memory builders are arity-N with arbitrary sub-pattern roles, so
//! they don't fit the binary-op `Combine` model directly.  We take the
//! Wildcard-fallback path from the plan: each sub-pattern is widened to
//! `Pat<Wildcard>` (via [`Pat::into_wildcard`]) at insertion time, and
//! the resulting `LoadPat` / `StorePat` finalises to `Pat<Wildcard>`.
//! In practice rewrite rules rarely build a `Load` / `Store` on the
//! RHS, and the existing role-aware unary / binary builders already
//! cover the buildable RHS surface.
//!
//! ## Deferred features
//!
//! The proven semantics in
//! `strider-analyze::pattern::pat::builders::memory` also expose
//! `.bit_width(n)`, `.stack_offset(k)`, `.stack_offset_any(ks)`, and
//! `.stack_only()`.  Each of those reads either `Function::output_kind`
//! or `Function::stack_offset` at match time — both require a
//! `post_match` closure with access to `MatchCtx`, but the current
//! `PostMatchFn` is the stub `Box<dyn Fn() -> bool>` (see
//! `pat_graph::node_data::PostMatchFn`).  They land alongside the
//! widened closure signature in a follow-up.

use strider_ir::node::NodeKind;

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, EdgeData, KindSpec, NodeData, PatGraph, Role, Wildcard,
    merge_subgraph,
};

use super::Pat;

// ── LoadPat ───────────────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`load`].
///
/// `Load` inputs are `[mem(0), addr(1)]`; the single output is the
/// loaded value.
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat<Wildcard>>,
    mem_in: Option<Pat<Wildcard>>,
}

impl LoadPat {
    fn new() -> Self {
        Self { space: None, addr: None, mem_in: None }
    }

    /// Restrict the match to loads in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }

    /// Constrain the load's address operand (`inputs[1]`).  The
    /// sub-pattern's role is erased to `Wildcard`.
    #[must_use]
    pub fn addr<R: Role>(mut self, p: Pat<R>) -> Self {
        self.addr = Some(p.into_wildcard());
        self
    }

    /// Constrain the load's memory predecessor (`inputs[0]`).  The
    /// sub-pattern's role is erased to `Wildcard`.
    #[must_use]
    pub fn mem_in<R: Role>(mut self, p: Pat<R>) -> Self {
        self.mem_in = Some(p.into_wildcard());
        self
    }
}

impl From<LoadPat> for Pat<Wildcard> {
    fn from(b: LoadPat) -> Pat<Wildcard> {
        let LoadPat { space, addr, mem_in } = b;
        let mut parent: PatGraph<Wildcard> = PatGraph::new();
        let exemplar = NodeKind::Load(rsleigh::VnSpace::RAM);
        let kind = match space {
            None => KindSpec::Variant(std::mem::discriminant(&exemplar)),
            Some(s) => KindSpec::VariantWith {
                discriminant: std::mem::discriminant(&exemplar),
                check: Box::new(move |k| matches!(k, NodeKind::Load(actual) if *actual == s)),
            },
        };
        // BuildSpec uses the RAM exemplar; LoadPat is Wildcard-rooted so
        // it can't be used as a Template — the build_spec is here purely
        // for shape uniformity with the rest of the crate.
        let root = parent.add_node(NodeData {
            kind,
            output_ty: None,
            capture: None,
            post_match: None,
            build_spec: Some(BuildSpec {
                kind: BuildKind::Exact(exemplar),
                ty: BuildTy::InheritRoot,
            }),
        
            force_ordered: false,
        });
        if let Some(mem_pat) = mem_in {
            let mem_root = merge_subgraph(&mut parent, mem_pat.0);
            parent.add_edge(
                mem_root,
                root,
                EdgeData {
                    consumer_slot: 0,
                    producer_output_slot: 0,
                },
            );
        }
        if let Some(addr_pat) = addr {
            let addr_root = merge_subgraph(&mut parent, addr_pat.0);
            parent.add_edge(
                addr_root,
                root,
                EdgeData {
                    consumer_slot: 1,
                    producer_output_slot: 0,
                },
            );
        }
        parent.set_root(root);
        Pat::from_graph(parent)
    }
}

/// Construct a fresh [`LoadPat`].  Chain `.space(...)`, `.addr(...)`,
/// `.mem_in(...)` then call `.into()` to finalise.
#[must_use]
pub fn load() -> LoadPat {
    LoadPat::new()
}

// ── StorePat ──────────────────────────────────────────────────────────────────

/// Builder for `Store` node patterns.  Created by [`store`].
///
/// `Store` inputs are `[mem(0), addr(1), data(2)]`; the single output
/// is the new memory token.
pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat<Wildcard>>,
    data: Option<Pat<Wildcard>>,
    mem_in: Option<Pat<Wildcard>>,
}

impl StorePat {
    fn new() -> Self {
        Self {
            space: None,
            addr: None,
            data: None,
            mem_in: None,
        }
    }

    /// Restrict the match to stores in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }

    /// Constrain the store's address operand (`inputs[1]`).
    #[must_use]
    pub fn addr<R: Role>(mut self, p: Pat<R>) -> Self {
        self.addr = Some(p.into_wildcard());
        self
    }

    /// Constrain the value being stored (`inputs[2]`).
    #[must_use]
    pub fn data<R: Role>(mut self, p: Pat<R>) -> Self {
        self.data = Some(p.into_wildcard());
        self
    }

    /// Constrain the store's memory predecessor (`inputs[0]`).
    #[must_use]
    pub fn mem_in<R: Role>(mut self, p: Pat<R>) -> Self {
        self.mem_in = Some(p.into_wildcard());
        self
    }
}

impl From<StorePat> for Pat<Wildcard> {
    fn from(b: StorePat) -> Pat<Wildcard> {
        let StorePat {
            space,
            addr,
            data,
            mem_in,
        } = b;
        let mut parent: PatGraph<Wildcard> = PatGraph::new();
        let exemplar = NodeKind::Store(rsleigh::VnSpace::RAM);
        let kind = match space {
            None => KindSpec::Variant(std::mem::discriminant(&exemplar)),
            Some(s) => KindSpec::VariantWith {
                discriminant: std::mem::discriminant(&exemplar),
                check: Box::new(move |k| matches!(k, NodeKind::Store(actual) if *actual == s)),
            },
        };
        let root = parent.add_node(NodeData {
            kind,
            output_ty: None,
            capture: None,
            post_match: None,
            build_spec: Some(BuildSpec {
                kind: BuildKind::Exact(exemplar),
                ty: BuildTy::InheritRoot,
            }),
        
            force_ordered: false,
        });
        if let Some(mem_pat) = mem_in {
            let mem_root = merge_subgraph(&mut parent, mem_pat.0);
            parent.add_edge(
                mem_root,
                root,
                EdgeData {
                    consumer_slot: 0,
                    producer_output_slot: 0,
                },
            );
        }
        if let Some(addr_pat) = addr {
            let addr_root = merge_subgraph(&mut parent, addr_pat.0);
            parent.add_edge(
                addr_root,
                root,
                EdgeData {
                    consumer_slot: 1,
                    producer_output_slot: 0,
                },
            );
        }
        if let Some(data_pat) = data {
            let data_root = merge_subgraph(&mut parent, data_pat.0);
            parent.add_edge(
                data_root,
                root,
                EdgeData {
                    consumer_slot: 2,
                    producer_output_slot: 0,
                },
            );
        }
        parent.set_root(root);
        Pat::from_graph(parent)
    }
}

/// Construct a fresh [`StorePat`].  Chain `.space(...)`, `.addr(...)`,
/// `.data(...)`, `.mem_in(...)` then call `.into()` to finalise.
#[must_use]
pub fn store() -> StorePat {
    StorePat::new()
}
