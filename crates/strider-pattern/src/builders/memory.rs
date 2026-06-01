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
//! ## Width / stack-offset filters
//!
//! Both builders expose `.bit_width(n)`, `.stack_offset(k)`,
//! `.stack_offset_any(ks)`, and `.stack_only()`.  Each is enforced by
//! a `post_match` closure that reads `Function::output_kind` or
//! `Function::stack_offset` at match time.  Mirrors the proven
//! semantics of `strider-analyze::pattern::pat::builders::memory`.

use strider_ir::node::NodeKind;

use crate::pat_graph::{
    BuildKind, BuildSpec, BuildTy, EdgeData, KindSpec, NodeData, PatGraph, PostMatchFn, Role, Wildcard,
    merge_subgraph,
};

use super::Pat;

// ── Stack-access filter (shared by LoadPat / StorePat) ───────────────────────

/// Filter applied at match time by looking up `Function::stack_offset`
/// on the matched node (O(1) — no re-decomposition of the address).
#[derive(Clone, Debug)]
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

/// SP-relative match state shared verbatim by `LoadPat` and `StorePat`.
#[derive(Clone, Default)]
struct StackAccessSpec {
    stack_offset_filter: Option<StackOffsetFilter>,
    /// When `true`, rejects matches where `Function::stack_offset` is `None`.
    stack_only: bool,
}

impl StackAccessSpec {
    fn needs_post(&self) -> bool {
        self.stack_offset_filter.is_some() || self.stack_only
    }

    fn check(&self, function: &strider_ir::Function, node: strider_ir::node::NodeId) -> bool {
        if self.stack_only || self.stack_offset_filter.is_some() {
            let Some((_base, offset)) = function.stack_offset(node) else {
                return false;
            };
            if let Some(ref f) = self.stack_offset_filter
                && !f.matches(offset)
            {
                return false;
            }
        }
        true
    }
}

// ── LoadPat ───────────────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`load`].
///
/// `Load` inputs are `[mem(0), addr(1)]`; the single output is the
/// loaded value.
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat<Wildcard>>,
    mem_in: Option<Pat<Wildcard>>,
    bit_width: Option<u32>,
    stack: StackAccessSpec,
}

impl LoadPat {
    fn new() -> Self {
        Self {
            space: None,
            addr: None,
            mem_in: None,
            bit_width: None,
            stack: StackAccessSpec::default(),
        }
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

    /// Restrict the match to loads whose value output is `n` bits wide.
    /// Matches both integer and float types of the same width
    /// (e.g. `bit_width(32)` matches I32 and F32).
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }

    /// Restrict the match to loads whose address decomposes to exactly
    /// `sp + k`.  Reads `Function::stack_offset` in O(1).  Requires
    /// `StackOffsetDetect` to have populated the side-table.
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
    /// Use to find any SP-relative load without constraining the
    /// offset; combine with `.stack_offset(k)` to further restrict.
    #[must_use]
    pub fn stack_only(mut self) -> Self {
        self.stack.stack_only = true;
        self
    }
}

impl From<LoadPat> for Pat<Wildcard> {
    fn from(b: LoadPat) -> Pat<Wildcard> {
        let LoadPat { space, addr, mem_in, bit_width, stack } = b;
        let mut parent: PatGraph<Wildcard> = PatGraph::new();
        let exemplar = NodeKind::Load(rsleigh::VnSpace::RAM);
        let kind = match space {
            None => KindSpec::Variant(std::mem::discriminant(&exemplar)),
            Some(s) => KindSpec::VariantWith {
                discriminant: std::mem::discriminant(&exemplar),
                check: Box::new(move |k| matches!(k, NodeKind::Load(actual) if *actual == s)),
            },
        };
        let post_match: Option<PostMatchFn> = if bit_width.is_some() || stack.needs_post() {
            let want_width = bit_width;
            Some(Box::new(move |ctx, node, ty, _b| {
                if let Some(w) = want_width
                    && ty.bit_width() != w as usize
                {
                    return false;
                }
                stack.check(ctx.function, node)
            }))
        } else {
            None
        };
        // BuildSpec uses the RAM exemplar; LoadPat is Wildcard-rooted so
        // it can't be used as a Template — the build_spec is here purely
        // for shape uniformity with the rest of the crate.
        let root = parent.add_node(NodeData {
            kind,
            output_ty: None,
            capture: None,
            post_match,
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
    bit_width: Option<u32>,
    stack: StackAccessSpec,
}

impl StorePat {
    fn new() -> Self {
        Self {
            space: None,
            addr: None,
            data: None,
            mem_in: None,
            bit_width: None,
            stack: StackAccessSpec::default(),
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

    /// Restrict the match to stores whose data input (`inputs[2]`) is
    /// `n` bits wide.  Matches both integer and float types of the
    /// same width (e.g. `bit_width(32)` matches I32 and F32).
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }

    /// Restrict the match to stores whose address decomposes to exactly
    /// `sp + k`.  Reads `Function::stack_offset` in O(1).  Requires
    /// `StackOffsetDetect` to have populated the side-table.
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
}

impl From<StorePat> for Pat<Wildcard> {
    fn from(b: StorePat) -> Pat<Wildcard> {
        let StorePat {
            space,
            addr,
            data,
            mem_in,
            bit_width,
            stack,
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
        let post_match: Option<PostMatchFn> = if bit_width.is_some() || stack.needs_post() {
            let want_width = bit_width;
            Some(Box::new(move |ctx, node, _ty, _b| {
                if let Some(w) = want_width {
                    // Store's data input is at `inputs[2]`; its producer's
                    // output type tells us the width.  Store's own output
                    // is the new memory token, not a value.
                    let Ok(data_in) = ctx.function.node_input_id_at(node, 2) else {
                        return false;
                    };
                    let data_out = ctx.function.input_output_id(data_in);
                    let Some(data_ty) = ctx.function.output_kind(data_out).as_value() else {
                        return false;
                    };
                    if data_ty.bit_width() != w as usize {
                        return false;
                    }
                }
                stack.check(ctx.function, node)
            }))
        } else {
            None
        };
        let root = parent.add_node(NodeData {
            kind,
            output_ty: None,
            capture: None,
            post_match,
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
