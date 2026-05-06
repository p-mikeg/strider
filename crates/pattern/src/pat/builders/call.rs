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
///
/// Slot conventions (post v2 precise-ABI lifting):
///
/// **Inputs** — addressed by [`Self::arg`] using the raw `inputs[i]` index:
///   * `arg(0, p)` → control predecessor
///   * `arg(1, p)` → memory predecessor
///   * `arg(2 + k, p)` → pcode-explicit arg `k` (matches Sleigh's
///     `inputs[1..]` after the user-op id)
///   * `arg(2 + N + k, p)` → implicit-read `k` (matches
///     `abi.implicit_reads[k]`; depends on the matched node's ABI)
///
/// **Outputs** — addressed by [`Self::ret`] using the raw `outputs[i]`
/// index:
///   * `ret(0, p)` → control output
///   * `ret(1, p)` → memory output (dangles when `abi.memory_edge=false`)
///   * `ret(2, p)` → pcode-explicit value output (when present)
///   * `ret(2 + has_value + k, p)` → clobber output `k`
///     (matches `abi.implicit_writes[k]`)
///
/// Convenience aliases for the well-known slots:
/// [`Self::ctrl`], [`Self::mem`], [`Self::ctrl_out`], [`Self::mem_out`].
pub struct CallOtherPat {
    user_op_id: Option<u64>,
    name: Option<String>,
    inputs: Vec<(usize, Pat)>,
    outputs: Vec<(usize, Pat)>,
}

impl CallOtherPat {
    pub(crate) fn new() -> Self {
        Self {
            user_op_id: None,
            name: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
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
    /// with [`Self::user_op_id`] and [`Self::arg`].
    #[must_use]
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }

    /// Constrain `inputs[idx]` of the matched CallOther.  Unlike
    /// `CallPat::arg` (which skips `ctrl`/`mem`), this addresses the
    /// raw input slot so callers can match on control / memory /
    /// pcode-args / implicit-reads uniformly.  See the type-level docs
    /// for the slot layout.
    pub fn arg(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.inputs.push((idx, p.into()));
        self
    }

    /// Constrain `outputs[idx]` of the matched CallOther.  See the
    /// type-level docs for the slot layout (ctrl / mem / value? /
    /// clobbers).
    pub fn ret(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.outputs.push((idx, p.into()));
        self
    }

    /// Convenience: match the control input (`inputs[0]`).
    pub fn ctrl(self, p: impl Into<Pat>) -> Self {
        self.arg(0, p)
    }

    /// Convenience: match the memory input (`inputs[1]`).
    pub fn mem(self, p: impl Into<Pat>) -> Self {
        self.arg(1, p)
    }

    /// Convenience: match the control output (`outputs[0]`).
    pub fn ctrl_out(self, p: impl Into<Pat>) -> Self {
        self.ret(0, p)
    }

    /// Convenience: match the memory output (`outputs[1]`).  Dangles
    /// (no consumers) when the ABI's `memory_edge` is `false`.
    pub fn mem_out(self, p: impl Into<Pat>) -> Self {
        self.ret(1, p)
    }
}

impl From<CallOtherPat> for Pat {
    fn from(b: CallOtherPat) -> Pat {
        let CallOtherPat { user_op_id, name, inputs, outputs } = b;
        let exemplar = NodeKind::CallOther { user_op_id: 0 };
        let kind = match user_op_id {
            None => KindSpec::variant(&exemplar),
            Some(expected) => KindSpec::variant_with(&exemplar, move |k| {
                matches!(k, NodeKind::CallOther { user_op_id } if *user_op_id == expected)
            }),
        };
        let mut pat = NodePat::matcher(kind, InputsSpec::Indexed(inputs));
        if !outputs.is_empty() {
            pat = pat.with_outputs(OutputsSpec::Indexed(outputs));
        }
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
