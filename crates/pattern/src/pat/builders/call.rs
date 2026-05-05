//! `CallPat`, `CallOtherPat` — call-site builders. Inputs are sparse-indexed
//! over `[ctrl, mem, target/args…]`; `CallPat` additionally supports output
//! constraints on ret-value slots.
//!
//! Use [`crate::pat::IntoPat::capture`] to bind the matched call node id.

use std::sync::Arc;

use ir::node::NodeKind;

use crate::pat::node_pat::{InputsSpec, KindSpec, NodePat, OutputsSpec};
use crate::pat::{Pat, int_const, int_const_any_of};

// ── CallPat ───────────────────────────────────────────────────────────────────

/// Builder for `Call` node patterns.  Created by [`crate::pat::call`].
pub struct CallPat {
    target: Option<Pat>,
    args: Vec<(usize, Pat)>,
    ret_outputs: Vec<(usize, Pat)>,
}

impl CallPat {
    pub(crate) fn new() -> Self {
        Self { target: None, args: Vec::new(), ret_outputs: Vec::new() }
    }
    /// Constrain the call target with an arbitrary pattern.
    pub fn target(mut self, p: impl Into<Pat>) -> Self {
        self.target = Some(p.into());
        self
    }
    /// Constrain argument at position `idx` (0-based, after ctrl and mem inputs).
    pub fn arg(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.args.push((idx, p.into()));
        self
    }
    /// Capture or constrain the Call's return-value output at ABI position
    /// `idx` — e.g. `.ret_output(0, var(c))` binds `c` to the
    /// `NodeOutputId` of the calling convention's first return register
    /// (`rax` on x86_64, `x0` on AArch64).  The inner pattern should be
    /// `var(c)` or `any()`; richer patterns are matched against the
    /// value output but will typically fail because the Call itself
    /// produces the value.  If the ret reg at `idx` is callee-saved,
    /// it does not appear as a Call output and the match fails.
    pub fn ret_output(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.ret_outputs.push((idx, p.into()));
        self
    }
    /// Constrain the call target to the literal address `addr`.
    #[must_use]
    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(addr))
    }
    /// Constrain the call target to any address in `addrs`.
    /// Set-membership variant of [`Self::at`] — useful when the same
    /// query should fire on multiple known callees (e.g. all
    /// lock-acquire helpers in a kernel binary).  An empty `addrs`
    /// vacuously fails, matching nothing.  Equivalent to
    /// `target(int_const_any_of(addrs))`.
    #[must_use]
    pub fn at_any<I>(self, addrs: I) -> Self
    where
        I: IntoIterator<Item = u64>,
    {
        self.target(int_const_any_of(addrs))
    }
}

impl From<CallPat> for Pat {
    fn from(b: CallPat) -> Pat {
        let CallPat { target, args, ret_outputs } = b;
        // Call inputs: [ctrl(0), mem(1), target(2), arg0(3), arg1(4), ...].
        let mut indexed_inputs: Vec<(usize, Pat)> = Vec::new();
        if let Some(tgt) = target {
            indexed_inputs.push((2, tgt));
        }
        for (i, p) in args {
            indexed_inputs.push((3 + i, p));
        }
        // Call outputs: [ctrl(0), mem(1), retval0(2), retval1(3), ...].
        let outputs_spec = if ret_outputs.is_empty() {
            OutputsSpec::None
        } else {
            OutputsSpec::Indexed(ret_outputs.into_iter().map(|(i, p)| (2 + i, p)).collect())
        };
        NodePat::matcher(KindSpec::Exact(NodeKind::Call), InputsSpec::Indexed(indexed_inputs))
            .with_outputs(outputs_spec)
            .into_pat()
    }
}

// ── CallOtherPat ──────────────────────────────────────────────────────────────

/// Builder for `CallOther` node patterns.  Created by [`crate::pat::call_other`].
pub struct CallOtherPat {
    user_op_id: Option<u64>,
    name: Option<String>,
    args: Vec<(usize, Pat)>,
}

impl CallOtherPat {
    pub(crate) fn new() -> Self {
        Self {
            user_op_id: None,
            name: None,
            args: Vec::new(),
        }
    }
    /// Constrain the matched node to a specific user-op id.
    #[must_use]
    pub fn user_op_id(mut self, v: u64) -> Self {
        self.user_op_id = Some(v);
        self
    }
    /// Constrain the matched node's user-op name (read from
    /// [`ir::Graph::call_other_name`]) to equal `n`.  Combinable
    /// with [`Self::user_op_id`] (both must match) and [`Self::arg`].
    #[must_use]
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }
    /// Constrain argument at position `idx` (0-based, after ctrl and mem inputs).
    pub fn arg(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.args.push((idx, p.into()));
        self
    }
}

impl From<CallOtherPat> for Pat {
    fn from(b: CallOtherPat) -> Pat {
        let CallOtherPat { user_op_id, name, args } = b;
        // CallOther inputs: [ctrl(0), mem(1), arg0(2), arg1(3), ...].
        let indexed_inputs: Vec<(usize, Pat)> =
            args.into_iter().map(|(i, p)| (2 + i, p)).collect();
        let exemplar = NodeKind::CallOther { user_op_id: 0 };
        let kind = match user_op_id {
            None => KindSpec::variant(&exemplar),
            Some(expected) => KindSpec::variant_with(&exemplar, move |k| {
                matches!(k, NodeKind::CallOther { user_op_id } if *user_op_id == expected)
            }),
        };
        let mut pat = NodePat::matcher(kind, InputsSpec::Indexed(indexed_inputs));
        if let Some(want) = name {
            pat = pat.with_post_match(Arc::new(move |ctx, node, _b| {
                ctx.graph
                    .graph
                    .call_other_name(node)
                    .is_some_and(|s| s == want.as_str())
            }));
        }
        pat.into_pat()
    }
}
