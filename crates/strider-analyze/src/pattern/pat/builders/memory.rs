//! Memory-access builders: `LoadPat`, `StorePat`.  All use
//! `InputsSpec::Indexed` for sparse positional sub-pattern constraints.
//!
//! Capture the matched output with `.capture(v)` from
//! [`crate::pattern::pat::IntoPat`].  Value-kind filtering at bind time (see
//! [`crate::pattern::pat::any::CapturePat`]) ensures the captured `NodeOutputId`
//! refers to the node's value output, not its memory / control slots.

use std::sync::Arc;

use strider_ir::node::NodeKind;

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{InputsSpec, KindSpec, NodePat};

// ── StackOffsetFilter ─────────────────────────────────────────────────────────

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

// ── LoadPat ───────────────────────────────────────────────────────────────────

/// Builder for `Load` node patterns.  Created by [`crate::pattern::pat::load`].
///
/// Note: `Load` is a single-output node (the loaded value at `outputs[0]`).
/// It does not produce a memory edge, so there is no `.next_mem(p)` method
/// on `LoadPat` — only `.mem_in(p)` for the backward-walk constraint and
/// `.bit_width(n)` for the value-width filter.
pub struct LoadPat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    mem_in: Option<Pat>,
    bit_width: Option<u32>,
    stack_offset_filter: Option<StackOffsetFilter>,
}

impl LoadPat {
    pub(crate) fn new() -> Self {
        Self {
            space: None,
            addr: None,
            mem_in: None,
            bit_width: None,
            stack_offset_filter: None,
        }
    }
    /// Restrict the match to loads in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Constrain the load's address operand (`inputs[1]`).
    pub fn addr(mut self, p: impl Into<Pat>) -> Self {
        self.addr = Some(p.into());
        self
    }
    /// Constrain the load's memory predecessor (`inputs[0]`).  The
    /// pattern walks back from the input edge to its producer in the
    /// standard data-flow direction.
    pub fn mem_in(mut self, p: impl Into<Pat>) -> Self {
        self.mem_in = Some(p.into());
        self
    }
    /// Restrict the match to loads whose value output (`outputs[0]`) is
    /// `n` bits wide.  Matches both integer and float types of the
    /// same width (e.g. `bit_width(32)` matches U32 and F32).
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }
    /// Restrict the match to loads whose address decomposes to exactly
    /// `sp + k`.  Reads `Function::stack_offset` in O(1) — no
    /// per-match address re-decomposition.
    ///
    /// Requires that `AliasSplit` has run before the matcher is invoked
    /// (the side-table is populated by that pass).
    #[must_use]
    pub fn stack_offset(mut self, k: i64) -> Self {
        self.stack_offset_filter = Some(StackOffsetFilter::Exact(k));
        self
    }
    /// Restrict the match to loads whose address decomposes to `sp + k`
    /// for some `k` in `ks`.  Reads `Function::stack_offset` in O(1).
    #[must_use]
    pub fn stack_offset_any(mut self, ks: impl Into<Vec<i64>>) -> Self {
        self.stack_offset_filter = Some(StackOffsetFilter::Set(ks.into()));
        self
    }
}

impl From<LoadPat> for Pat {
    fn from(b: LoadPat) -> Pat {
        let LoadPat { space, addr, mem_in, bit_width, stack_offset_filter } = b;
        // Load inputs = [mem(0), addr(1)]; outputs = [value(0)] (single output).
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(p) = mem_in {
            indexed.push((0, p));
        }
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
        let mut pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed));

        // Combined post-match closure: bit_width AND stack_offset checks.
        // `with_post_match` replaces the existing closure (single slot),
        // so both checks must live in one callback.
        if bit_width.is_some() || stack_offset_filter.is_some() {
            let want_width = bit_width;
            let off_filter = stack_offset_filter;
            pat = pat.with_post_match(Arc::new(move |ctx, node, _b| {
                if let Some(want) = want_width {
                    let outs = ctx.graph.node_outputs(node);
                    let Some(&value_out) = outs.first() else {
                        return false;
                    };
                    let Some(ty) = ctx.graph.output_kind(value_out).as_value() else {
                        return false;
                    };
                    if ty.bit_width() != want as usize {
                        return false;
                    }
                }
                if let Some(ref f) = off_filter {
                    let Some(offset) = ctx.graph.stack_offset(node) else {
                        return false;
                    };
                    if !f.matches(offset) {
                        return false;
                    }
                }
                true
            }));
        }

        pat.into_pat()
    }
}

// ── StorePat ──────────────────────────────────────────────────────────────────

/// Builder for `Store` node patterns.  Created by [`crate::pattern::pat::store`].
pub struct StorePat {
    space: Option<rsleigh::VnSpace>,
    addr: Option<Pat>,
    data: Option<Pat>,
    mem_in: Option<Pat>,
    next_mem: Option<Pat>,
    bit_width: Option<u32>,
    stack_offset_filter: Option<StackOffsetFilter>,
}

impl StorePat {
    pub(crate) fn new() -> Self {
        Self {
            space: None,
            addr: None,
            data: None,
            mem_in: None,
            next_mem: None,
            bit_width: None,
            stack_offset_filter: None,
        }
    }
    /// Restrict the match to stores in address space `s`.
    #[must_use]
    pub fn space(mut self, s: rsleigh::VnSpace) -> Self {
        self.space = Some(s);
        self
    }
    /// Constrain the store's address operand (`inputs[1]`).
    pub fn addr(mut self, p: impl Into<Pat>) -> Self {
        self.addr = Some(p.into());
        self
    }
    /// Constrain the value being stored (`inputs[2]`).
    pub fn data(mut self, p: impl Into<Pat>) -> Self {
        self.data = Some(p.into());
        self
    }
    /// Constrain the store's memory predecessor (`inputs[0]`).  The
    /// pattern walks back from the input edge to its producer.
    pub fn mem_in(mut self, p: impl Into<Pat>) -> Self {
        self.mem_in = Some(p.into());
        self
    }
    /// Match `p` against the unique consumer of the store's memory
    /// output (`outputs[0]`).  Returns no match if the output has zero
    /// or multiple consumers (deterministic; no arbitrary pick).
    pub fn next_mem(mut self, p: impl Into<Pat>) -> Self {
        self.next_mem = Some(p.into());
        self
    }
    /// Restrict the match to stores whose data input (`inputs[2]`) is
    /// `n` bits wide.  Matches both integer and float types of the
    /// same width (e.g. `bit_width(32)` matches U32 and F32).
    #[must_use]
    pub fn bit_width(mut self, n: u32) -> Self {
        self.bit_width = Some(n);
        self
    }
    /// Restrict the match to stores whose address decomposes to exactly
    /// `sp + k`.  Reads `Function::stack_offset` in O(1) — no
    /// per-match address re-decomposition.
    ///
    /// Requires that `AliasSplit` has run before the matcher is invoked
    /// (the side-table is populated by that pass).
    #[must_use]
    pub fn stack_offset(mut self, k: i64) -> Self {
        self.stack_offset_filter = Some(StackOffsetFilter::Exact(k));
        self
    }
    /// Restrict the match to stores whose address decomposes to `sp + k`
    /// for some `k` in `ks`.  Reads `Function::stack_offset` in O(1).
    #[must_use]
    pub fn stack_offset_any(mut self, ks: impl Into<Vec<i64>>) -> Self {
        self.stack_offset_filter = Some(StackOffsetFilter::Set(ks.into()));
        self
    }
}

impl From<StorePat> for Pat {
    fn from(b: StorePat) -> Pat {
        let StorePat {
            space,
            addr,
            data,
            mem_in,
            next_mem,
            bit_width,
            stack_offset_filter,
        } = b;
        // Store inputs = [mem(0), addr(1), data(2)]; outputs = [mem(0)].
        let mut indexed: Vec<(usize, Pat)> = Vec::new();
        if let Some(p) = mem_in {
            indexed.push((0, p));
        }
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
        let mut pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed));

        // Combined post-match closure: bit_width, next_mem, and
        // stack_offset checks.  `with_post_match` replaces the existing
        // closure (single slot), so all checks must live in one callback.
        if bit_width.is_some() || next_mem.is_some() || stack_offset_filter.is_some() {
            let want_width = bit_width;
            let next_mem_pat = next_mem;
            let off_filter = stack_offset_filter;
            pat = pat.with_post_match(Arc::new(move |ctx, node, b| {
                if let Some(w) = want_width {
                    // Store's data input is at `inputs[2]`; its producer's
                    // output type tells us the width.
                    let inputs = ctx.graph.node_inputs(node);
                    let Some(&data_in) = inputs.get(2) else {
                        return false;
                    };
                    let Some(ty) = ctx.graph.output_kind(data_in).as_value() else {
                        return false;
                    };
                    if ty.bit_width() != w as usize {
                        return false;
                    }
                }
                if let Some(ref p) = next_mem_pat
                    && !super::consumer_match::match_unique_output_consumer(ctx, node, 0, p, b)
                {
                    return false;
                }
                if let Some(ref f) = off_filter {
                    let Some(offset) = ctx.graph.stack_offset(node) else {
                        return false;
                    };
                    if !f.matches(offset) {
                        return false;
                    }
                }
                true
            }));
        }

        pat.into_pat()
    }
}
