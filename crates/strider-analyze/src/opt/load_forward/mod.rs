//! Forwards the value of a `Store(addr=sp+K)` to a subsequent `Load[sp + K]`
//! when the load's memory input traces back to that store with no aliasing
//! writes in between.  When a `MemPhi` sits between store and load and every
//! predecessor resolves to a store at the same offset, the load is replaced
//! with a synthesized anonymous `NodeKind::Phi` sharing the `MemPhi`'s
//! phi-token.
//!
//! Must be wired into the pipeline with the calling convention's stack-pointer
//! varnode and the target's endianness (see [`LoadForward::new`]).

use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use strider_target::Endianness;

use crate::opt::error::Result;
use crate::opt::mem_walk::{CyclePolicy, MemChainStep, StepResult, walk_mem_chain};
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{SpExpr, SpExprMemo, decompose_sp, ranges_disjoint};
use crate::opt::worklist::seeded_kind;

/// Store-to-load forwarding for SP-relative stack slots.
///
/// Runs inside the main fixed-point loop so that stack stores classified by
/// `StackOffsetDetect` become visible to the walker on subsequent iterations,
/// and so that forwarded constants fed into expressions are in turn
/// simplified by `ConstantFold` / `KnownBits`.
#[derive(Clone)]
pub struct LoadForward {
    /// Stack-pointer varnode used by [`decompose_sp`] to recognise
    /// SP-relative addresses.  Extracted from the calling convention at
    /// construction time — the pass consults nothing else from the CC.
    stack_vn: rsleigh::Vn,
    /// Target endianness — controls how a narrow load from a wider store is
    /// synthesised (LE: low bytes via `Truncate`; BE: high bytes via
    /// `Truncate(ShiftRight(data, (store_size - load_size) * 8))`).
    ///
    /// Carried separately from the CC because endianness is a
    /// per-arch property (lives on [`strider_target::SleighArch`])
    /// rather than a per-CC property.
    endianness: Endianness,
    /// Alias-analysis precision for the backward chain walk.  Default
    /// is [`crate::opt::AliasMode::AssumeStackGlobalDisjoint`].
    alias_mode: crate::opt::AliasMode,
}

impl LoadForward {
    /// Creates a new pass for the given stack-pointer varnode and target
    /// endianness.  Convenience constructor; production paths prefer
    /// [`Self::from_convention`] so the same CC is shared with the
    /// other SP-aware passes.
    #[must_use]
    pub const fn new(stack_vn: rsleigh::Vn, endianness: Endianness) -> Self {
        Self {
            stack_vn,
            endianness,
            alias_mode: crate::opt::AliasMode::AssumeStackGlobalDisjoint,
        }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from `cc` and
    /// whose endianness is taken from `arch`.
    #[must_use]
    pub fn from_convention(
        cc: &strider_target::BuiltCallingConvention,
        arch: &strider_target::SleighArch,
    ) -> Self {
        Self::new(cc.stack_vn, arch.endianness())
    }

    /// Overrides the alias-analysis precision used by the chain walk.
    /// See [`crate::opt::AliasMode`] for the soundness/coverage trade-off.
    #[must_use]
    pub const fn alias_mode(mut self, mode: crate::opt::AliasMode) -> Self {
        self.alias_mode = mode;
        self
    }
}

impl Optimizer for LoadForward {
    fn apply(
        &self,
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        _opt_ctx: &crate::opt::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let mut work = seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        let stack_vn = self.stack_vn;
        while let Some(load) = work.dequeue() {
            result |= try_forward_load(ctx, load, stack_vn, self.endianness, &mut memo, self.alias_mode)?;
        }
        Ok(result)
    }
}

/// Tries to forward a single `Load` to the value of a matching
/// upstream `Store`.  Address-class dispatch lives in
/// [`classify_addr`] and the per-pair match/disjoint/may-alias verdict
/// in [`alias_verdict`].  Returns `Changed` iff the load's uses were
/// rewired.
fn try_forward_load(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    load: NodeId,
    stack_vn: rsleigh::Vn,
    endianness: Endianness,
    memo: &mut SpExprMemo,
    alias_mode: crate::opt::AliasMode,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr].
    let [mem, addr] = ctx.node_inputs_exact::<2>(load)?;
    let [load_out] = ctx.node_outputs_exact::<1>(load)?;
    let Some(load_ty) = ctx.output_kind(load_out).as_value() else {
        return Ok(OptimizationResult::NoChange);
    };

    let load_class = classify_addr(ctx.function_ref(), addr, stack_vn, memo);
    let load_size = load_ty.byte_size() as i64;
    // Two-phase walk: probe is read-only and decides whether forwarding
    // can succeed; only on full success does realize commit fresh nodes
    // (Truncate / ShiftRight / anonymous Phi) to the graph. This prevents
    // partial walks that fail downstream from leaving orphan nodes in
    // the arena.
    let mut visited: entity_utils::DenseEntitySet<NodeOutputId> = entity_utils::DenseEntitySet::new();
    let Some(shape) = probe(
        ctx,
        mem,
        load_class,
        load_size,
        load_ty,
        stack_vn,
        memo,
        &mut visited,
        alias_mode,
    )?
    else {
        return Ok(OptimizationResult::NoChange);
    };
    let forwarded = realize(ctx, shape, load_ty, endianness, load)?;

    // `replace_value` absorbs the rewritten Load's asm-fingerprint into the
    // forwarded producer and redirects all uses.  `realize` may have returned
    // an existing-attributed node or a freshly synthesised one (Truncate /
    // ShiftRight / anonymous Phi); multi-node BE chains have each intermediate
    // already attributed via `create_node_attributed(..., &[load])` inside
    // `realize`, so this covers the outermost LE narrow and Existing cases.
    let changed = ctx.replace_value(load_out, forwarded)?;
    if changed {
        ctx.detach_node_inputs(load);
    }
    Ok(OptimizationResult::from_changed(changed))
}

/// Description of how to materialize a forwarded value.  Built by
/// [`probe`] (which is read-only) and consumed by [`realize`] (which is
/// the only function that creates fresh IR nodes for forwarding).  Splitting
/// the walk this way prevents a partial probe — one that succeeds for some
/// MemPhi predecessors and fails for others — from leaking orphan nodes
/// (`Truncate`, `ShiftRight`, anonymous `Phi`) into the graph arena.
enum ResolveShape {
    /// The forwarded value is an existing graph output and no new IR is
    /// needed.
    Existing(NodeOutputId),
    /// Narrow-load-from-wider-store at a matching offset.  `realize`
    /// synthesizes `Truncate(data)` (LE) or `Truncate(ShiftRight(data, k))`
    /// (BE) using `data_ty` to size the shift.
    Narrow {
        data: NodeOutputId,
        data_ty: strider_ir::node::NodeOutputType,
    },
    /// MemPhi resolution.  `realize` recursively materializes each
    /// predecessor first; if every predecessor materializes to the same
    /// `NodeOutputId` it returns that one without creating a `Phi`,
    /// otherwise it creates an anonymous `Phi { phi_token, vals... }`.
    Phi {
        phi_token: NodeOutputId,
        preds: Vec<ResolveShape>,
    },
}

/// Coarse classification of a Load / Store address.  The verdict
/// table in [`alias_verdict`] is keyed on the `(load_class,
/// store_class)` pair: matching addresses use the diagonal of the
/// table, disjointness uses the off-diagonal.
#[derive(Clone, Copy, Debug)]
enum AddrClass {
    /// `decompose_sp` returned `Terminal { base, offset }`.  Two
    /// `SpRooted` addresses refer to the same byte range only when they
    /// share the same `base` (the SP-derived terminal node) AND offset;
    /// disjoint offsets on the SAME base are proven non-overlapping via
    /// [`ranges_disjoint`].  Different bases — e.g. `InitialVar(sp)` vs an
    /// alignment-masked `sp & -16` — differ by an unknown amount (the
    /// caller-dependent `sp mod align`), so their offsets are in different
    /// coordinate systems and are treated as may-alias.
    SpRooted { base: NodeOutputId, offset: i64 },
    /// `NodeKind::IntConst(_)` address — a literal `.data`/`.rodata`/
    /// `.bss`/MMIO pointer.  Two `Constant` addresses with equal
    /// values refer to the same byte range; disjoint values are
    /// proven non-overlapping via [`ranges_disjoint`].
    Constant { addr: i64 },
    /// Anything else (`Load`-of-pointer, `Add` of opaque values,
    /// `Phi`-of-offsets, …).  Two `Anchor` addresses are proven equal
    /// only by `NodeOutputId` equality; different ids can compute to
    /// the same address at runtime, so we treat them as
    /// possibly-aliasing.
    Anchor { out: NodeOutputId },
}

/// Classifies a load / store address.  Cheap: `decompose_sp` is memoised
/// across the function, the `IntConst` peek is a single match.
fn classify_addr(
    function: &strider_ir::Function,
    addr: NodeOutputId,
    stack_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> AddrClass {
    match decompose_sp(function, addr, stack_vn, memo) {
        Some(SpExpr::Terminal { base, offset }) => AddrClass::SpRooted { base, offset },
        Some(SpExpr::Phi { .. }) => AddrClass::Anchor { out: addr },
        None => {
            let node = function.node_for_output(addr);
            match function.node_kind(node) {
                NodeKind::IntConst(c) => AddrClass::Constant { addr: *c as i64 },
                _ => AddrClass::Anchor { out: addr },
            }
        }
    }
}

/// Pairwise verdict between a Load's address class + size and an
/// intervening Store's address class + size.  Implements the table
/// described in the [`crate::opt::AliasMode`] module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AliasVerdict {
    /// Same byte range — caller treats this Store as the forwarding
    /// source.
    Match,
    /// Provably non-overlapping byte range — caller steps through.
    Disjoint,
    /// Cannot prove either; caller bails.
    MayAlias,
}

/// Diagonal verdict for two in-class offsets: equal → `Match`,
/// range-disjoint → `Disjoint`, otherwise `MayAlias`.  Shared by the
/// `SpRooted`/`SpRooted` and `Constant`/`Constant` arms of
/// [`alias_verdict`] (the `Anchor`/`Anchor` arm uses `NodeOutputId`
/// equality and has no offset/range shape).
fn cmp_same_class_offsets(
    load_off: i64,
    load_size: i64,
    store_off: i64,
    store_size: i64,
) -> AliasVerdict {
    if load_off == store_off {
        AliasVerdict::Match
    } else if ranges_disjoint(load_off, load_size, store_off, store_size) {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

fn alias_verdict(
    load_class: AddrClass,
    load_size: i64,
    store_class: AddrClass,
    store_size: i64,
    mode: crate::opt::AliasMode,
) -> AliasVerdict {
    use AddrClass::*;
    match (load_class, store_class) {
        // Diagonal: in-class equality + range-disjoint.  Two SP-rooted
        // addresses are only comparable when they share the same base node;
        // different SP bases (initial SP vs an alignment-masked SP) differ by
        // an unknown amount, so their offsets can't be related → may-alias.
        (SpRooted { base: lb, offset: lo }, SpRooted { base: sb, offset: so }) => {
            if lb == sb {
                cmp_same_class_offsets(lo, load_size, so, store_size)
            } else {
                AliasVerdict::MayAlias
            }
        }
        (Constant { addr: lo }, Constant { addr: so }) => {
            cmp_same_class_offsets(lo, load_size, so, store_size)
        }
        (Anchor { out: lout }, Anchor { out: sout }) => {
            if lout == sout {
                AliasVerdict::Match
            } else {
                // Different NodeOutputIds can compute to the same
                // address at runtime; no disjointness proof available.
                AliasVerdict::MayAlias
            }
        }
        // Off-diagonal: cross-class.  Strict cannot prove disjoint;
        // AssumeStackGlobalDisjoint admits SP↔Constant pairs.
        (SpRooted { .. }, Constant { .. }) | (Constant { .. }, SpRooted { .. }) => match mode {
            crate::opt::AliasMode::Strict => AliasVerdict::MayAlias,
            crate::opt::AliasMode::AssumeStackGlobalDisjoint => AliasVerdict::Disjoint,
        },
        // Every other cross-class pair (Anchor vs anything) still
        // bails under both modes; closing this requires escape
        // analysis.
        _ => AliasVerdict::MayAlias,
    }
}

/// [`MemChainStep`] implementation for [`probe`].
struct ProbeStep<'a> {
    load_class: AddrClass,
    load_size: i64,
    load_ty: strider_ir::node::NodeOutputType,
    stack_vn: rsleigh::Vn,
    memo: &'a mut SpExprMemo,
    alias_mode: crate::opt::AliasMode,
}

impl<'a> MemChainStep for ProbeStep<'a> {
    type Verdict = Option<ResolveShape>;

    fn classify(
        &mut self,
        function: &strider_ir::Function,
        _mem: NodeOutputId,
        node: NodeId,
    ) -> Result<StepResult<Option<ResolveShape>>> {
        match *function.node_kind(node) {
            NodeKind::Store(_) => {
                // Store inputs: [memory, addr, data].
                let inputs = function.node_inputs(node);
                if inputs.len() < 3 {
                    return Ok(StepResult::Verdict(None));
                }
                let addr = inputs[1];
                let data = inputs[2];
                let Some(data_ty) = function.output_kind(data).as_value() else {
                    return Ok(StepResult::Verdict(None));
                };
                let store_size = data_ty.byte_size() as i64;
                let store_class = classify_addr(function, addr, self.stack_vn, self.memo);
                match alias_verdict(
                    self.load_class,
                    self.load_size,
                    store_class,
                    store_size,
                    self.alias_mode,
                ) {
                    AliasVerdict::Match => {
                        // Forward the stored value, applying the
                        // narrow-from-wider rewrite when the load
                        // reads fewer bytes than the store wrote.
                        if data_ty == self.load_ty {
                            Ok(StepResult::Verdict(Some(ResolveShape::Existing(data))))
                        } else if data_ty.is_integer()
                            && self.load_ty.is_integer()
                            && self.load_ty.byte_size() < data_ty.byte_size()
                        {
                            Ok(StepResult::Verdict(Some(ResolveShape::Narrow {
                                data,
                                data_ty,
                            })))
                        } else {
                            Ok(StepResult::Verdict(None))
                        }
                    }
                    AliasVerdict::Disjoint => Ok(StepResult::Continue(inputs[0])),
                    AliasVerdict::MayAlias => Ok(StepResult::Verdict(None)),
                }
            }
            NodeKind::MemPhi => {
                // MemPhi inputs: [phi_token, mem_pred_0, mem_pred_1, ...].
                let inputs = function.node_inputs(node);
                if inputs.len() < 2 {
                    return Ok(StepResult::Verdict(None));
                }
                let phi_token = inputs[0];
                let preds = inputs.iter().skip(1).collect();
                Ok(StepResult::JoinPhi {
                    phi_node: node,
                    phi_token,
                    preds,
                })
            }
            _ => Ok(StepResult::Verdict(None)),
        }
    }

    fn cycle_verdict(&mut self) -> Option<ResolveShape> {
        // Cycle guard: loop-header MemPhis feed their own region
        // indirectly.  Fail closed.
        None
    }

    fn combine_phi(
        &mut self,
        _phi_node: NodeId,
        phi_token: NodeOutputId,
        preds: Vec<Option<ResolveShape>>,
    ) -> Option<ResolveShape> {
        // If any predecessor failed, the whole MemPhi fails closed.
        let mut collected: Vec<ResolveShape> = Vec::with_capacity(preds.len());
        for p in preds {
            collected.push(p?);
        }
        Some(ResolveShape::Phi {
            phi_token,
            preds: collected,
        })
    }
}

/// Iterative read-only walk of the memory chain backward from `mem`
/// looking for a provable source of the bytes
/// `[offset, offset + load_size)` at type `load_ty`.  Stack-safe at any
/// memory-chain depth via the shared [`walk_mem_chain`] driver.
///
/// Returns `None` if forwarding cannot be proven (alias, malformed
/// inputs, or a `MemPhi` self-cycle).
// Eight arguments are the minimum needed to thread cycle-guards, the SP
// decomposition memo, and the search-target byte range through the probe;
// bundling them into a context struct would just add indirection without
// clarifying the call sites.
#[allow(clippy::too_many_arguments)]
fn probe(
    ctx: &strider_pattern::RewriteCtx<'_>,
    initial_mem: NodeOutputId,
    load_class: AddrClass,
    load_size: i64,
    load_ty: strider_ir::node::NodeOutputType,
    stack_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visited: &mut entity_utils::DenseEntitySet<NodeOutputId>,
    alias_mode: crate::opt::AliasMode,
) -> Result<Option<ResolveShape>> {
    let mut step = ProbeStep {
        load_class,
        load_size,
        load_ty,
        stack_vn,
        memo,
        alias_mode,
    };
    walk_mem_chain(
        ctx.function_ref(),
        initial_mem,
        CyclePolicy::GuardPhiOnly,
        visited,
        |node| matches!(ctx.node_kind(node), NodeKind::MemPhi),
        &mut step,
    )
}

/// Materializes a [`ResolveShape`] into a concrete `NodeOutputId`,
/// creating any new IR nodes (`Truncate`, `ShiftRight`, anonymous `Phi`) only
/// once the entire shape is known.  The dedup of identical predecessor
/// values for `Phi` happens here as well: if every realized predecessor
/// shares the same output id, no `Phi` is created.
///
/// `Result<_, _>` is needed only because `make_int_const` can fail when
/// the IR rejects the requested constant; structurally the realization
/// is a deterministic walk over the shape tree.
///
/// Recursion-depth cap (`MAX_RESOLVE_DEPTH`): `probe` already snaps a
/// `Cycle` verdict on revisited MemPhi tokens via its `seen` set, so
/// the shape tree the realize walk consumes is always finite.  But a
/// pathological adversarial graph with thousands of nested
/// MemPhi-of-MemPhi shapes would blow the Rust stack before the
/// per-test wallclock budget triggers.  Surface an error at the cap
/// instead of UB-ing the host process.
fn realize(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    shape: ResolveShape,
    load_ty: strider_ir::node::NodeOutputType,
    endianness: Endianness,
    load: strider_ir::node::NodeId,
) -> crate::opt::Result<NodeOutputId> {
    realize_with_depth(ctx, shape, load_ty, endianness, load, 0)
}

const MAX_RESOLVE_DEPTH: usize = 512;

fn realize_with_depth(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    shape: ResolveShape,
    load_ty: strider_ir::node::NodeOutputType,
    endianness: Endianness,
    load: strider_ir::node::NodeId,
    depth: usize,
) -> crate::opt::Result<NodeOutputId> {
    if depth > MAX_RESOLVE_DEPTH {
        return Err(anyhow::anyhow!(
            "load_forward::realize exceeded MAX_RESOLVE_DEPTH={MAX_RESOLVE_DEPTH} \
             — refusing to recurse on pathological nested-MemPhi shape"
        ));
    }
    match shape {
        ResolveShape::Existing(out) => Ok(out),
        ResolveShape::Narrow { data, data_ty } => {
            // - LE: load bytes are the low `load_size` bytes of the stored
            //   value → `Truncate(data)`.
            // - BE: load bytes are the high `load_size` bytes →
            //   `Truncate(ShiftRight(data, (store_size - load_size) * 8))`.
            //   `ShiftRight` is the *logical* right-shift (zero-fill), the
            //   correct synthesis since we want the high bytes positioned
            //   in the low end before truncating.
            //
            // Use `create_node_attributed(..., &[load])` for every
            // freshly-synthesised node so the asm-fingerprint contract
            // holds at every intermediate node — not just the outermost.
            // The caller in `try_forward_load` only absorbs into the
            // returned outermost node, so a plain `create_node` would
            // leave the BE-path `ShiftRight` node reachable with an
            // empty fingerprint.
            let shifted = match endianness {
                Endianness::Little => data,
                Endianness::Big => {
                    let shift_bits =
                        ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
                    // `make_int_const` does NOT stamp asm-fingerprints (it's
                    // the low-level `Graph` method, not the `FunctionBuilder`
                    // one).  Build the IntConst via `create_node_attributed`
                    // so the freshly-introduced constant inherits the
                    // rewritten load's fingerprint — otherwise the Layer-C
                    // always-on check trips on the BE narrow-shift constant
                    // (e.g. `IntConst(32)` for a I64→I32 narrow on aarch64be).
                    let shift_const_node = ctx.create_node_attributed(
                        NodeKind::IntConst(u128::from(shift_bits) & data_ty.bit_mask_u128()),
                        [],
                        [NodeOutputKind::OutputType(data_ty)],
                        &[load],
                    );
                    let [shift_const] = ctx.node_outputs_exact::<1>(shift_const_node)?;
                    let shr = ctx.create_node_attributed(
                        NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::ShiftRight),
                        [data, shift_const],
                        [NodeOutputKind::OutputType(data_ty)],
                        &[load],
                    );
                    let [out] = ctx.node_outputs_exact::<1>(shr)?;
                    out
                }
            };
            let trunc = ctx.create_node_attributed(
                NodeKind::Truncate,
                [shifted],
                [NodeOutputKind::OutputType(load_ty)],
                &[load],
            );
            let [out] = ctx.node_outputs_exact::<1>(trunc)?;
            Ok(out)
        }
        ResolveShape::Phi { phi_token, preds } => {
            let mut resolved: Vec<NodeOutputId> = Vec::with_capacity(preds.len());
            for p in preds {
                resolved.push(realize_with_depth(ctx, p, load_ty, endianness, load, depth + 1)?);
            }
            // Dedup: if all per-predecessor results coincide, skip the
            // anonymous Phi — returning the common value keeps the graph
            // smaller and exposes it to later passes more cleanly.
            // `windows(2).all` is vacuously true for len < 2, but `probe`
            // already rejects MemPhi with fewer than 2 mem predecessors,
            // so `resolved.first()` is the actual emptiness guard here.
            if let Some(&first) = resolved.first()
                && resolved.windows(2).all(|w| w[0] == w[1])
            {
                return Ok(first);
            }
            let value_phi = ctx.create_node_attributed(
                NodeKind::Phi,
                std::iter::once(phi_token).chain(resolved),
                [NodeOutputKind::OutputType(load_ty)],
                &[load],
            );
            let [out] = ctx.node_outputs_exact::<1>(value_phi)?;
            Ok(out)
        }
    }
}


// ── Public helper for the indirect-branch classifier ──────
//
// `try_forward_load` rewrites the load by bottoming-out the memory chain at
// a stack-tagged `Store` and re-using its data slot.  When the load address has a
// concrete SP-relative offset, that's straightforward.  But the
// computed-goto-via-stack-array shape has a *symbolic* offset
// (`sp + base + idx*stride`) — the per-i target lives at offset
// `base + i*stride` for i in [0, N), bounded by KnownBits.
//
// The indirect-branch classifier needs to enumerate per-i values without rewriting
// the load (no IR primitive expresses "value depends on idx" without a
// `Region` for an anonymous `Phi` to bind to).  This helper exposes the
// stack-tagged-`Store`-chain walk as a pub function: given a memory chain root
// and a concrete offset, return the `NodeOutputId` of the value stored
// there (or `None` when the chain has no matching store, has an aliasing
// intermediate, or terminates at `InitialMemory`).
//
// SOUNDNESS — same algorithm as [`probe`]'s stack-tagged / raw `Store`
// arms, restricted to the no-MemPhi case (the classifier asks one
// concrete offset at a time):
//   * stack-tagged `Store { offset == requested }` with matching value type:
//     return the stored `data` output.  This is sound because no later
//     write can have aliased the slot — we walked here from the load's
//     memory input through strictly-earlier stores, and the offset
//     equality check is exact (StackOffsetDetect tagged it).
//   * stack-tagged `Store` at a different offset: skip iff the byte ranges are
//     provably disjoint (`ranges_disjoint`); recurse on the prior
//     memory.
//   * `Store(_)` (raw, untagged): probe its address.  If it's
//     not SP-rooted (`decompose_sp` returns `None`), it cannot alias
//     a stack slot; recurse.  If it IS SP-rooted (`Terminal`), recurse
//     iff disjoint.  `SpExpr::Phi` (SP through a phi) is conservatively
//     treated as aliasing → bail.
//   * `MemPhi`: cross-region join.  This helper does NOT recurse
//     across MemPhi (returns `None`) — the case is single-
//     region (the prologue stores and the dispatch load live in the
//     same region) and the classifier asks one offset at a time, so
//     the "all preds agree" reasoning the existing `probe` does for
//     anonymous-`Phi` synthesis is unnecessary here.  Future extension:
//     handle MemPhi by recursing into preds and requiring all to
//     return the same `NodeOutputId`.
//   * `InitialMemory` / anything else: return `None`.
//
// Type strictness: the helper returns `None` if the stack-tagged Store's value
// type doesn't equal `value_type` exactly.  Narrow-load-from-wider-store
// (which `probe` handles via `ResolveShape::Narrow`) is intentionally
// NOT implemented here — the classifier only consumes IntConst targets,
// and a Truncate(IntConst) folds to IntConst via ConstantFold, so the
// narrow case shows up as a wide-typed IntConst-valued store that the
// classifier can read directly.

/// Per-call memo for `find_stack_stored_value_at_offset`, keyed on
/// `(memory_token, offset, value_type)`.  Threaded through the
/// indirect-branch classifier loops so repeated lookups across
/// enumerated jump-table indices share their walks.
pub type StackStoredValueMemo =
    rustc_hash::FxHashMap<(NodeOutputId, i64, NodeOutputType), Option<NodeOutputId>>;

/// Walks the memory chain backward from `mem` looking for a
/// `Store(addr=sp+offset)` whose stored value has type `value_type`.
/// Returns the stored value's output id on success, or `None` when no
/// matching store dominates the chain.
///
/// See the module-level "Public helper for the indirect-branch
/// classifier" notes for the soundness rules.
///
/// # Permissiveness (do not rely on this for cross-base disjointness)
///
/// This is a deliberately permissive stack-slot lookup written for the
/// indirect-branch stack-array classifier, and it is *more* permissive
/// than the shared `crate::opt::sp_expr::walk` step:
///
/// - **Walks past non-SP-rooted stores unconditionally.**  When the
///   store's address does not decompose to an SP expression (the `None`
///   arm), it skips the store and continues down `inputs[0]`, with no
///   `AliasMode` gate and accepting opaque pointer addresses — assuming
///   stack and non-stack memory are disjoint.  The shared
///   `step_through_store` gates the same skip behind
///   `AliasMode::AssumeStackGlobalDisjoint` *and* requires a literal
///   `IntConst` address; this helper does neither.
/// - **Keys slots by offset only, not by base.**  The
///   `SpExpr::Terminal { base: _, offset: k }` arm matches on `k == offset`
///   alone and ignores the SP `base`, so two distinct SP-relative bases
///   that share an offset are treated as the same slot.
///
/// Both are sound for the single-frame jump-table-array use this helper
/// serves, but callers MUST NOT rely on it for cross-base disjointness.
///
/// # Parameters
///
/// - `graph` — the IR graph to walk (read-only).
/// - `mem` — the chain root (typically a Load's memory-input slot).
/// - `offset` — the SP-relative offset of the requested slot.
/// - `value_type` — the expected stored value's type.  Mismatched
///   types return `None` (no Truncate / ShiftRight synthesis here).
/// - `sp_vn` — the calling convention's stack-pointer varnode (used
///   to interpret raw `Store(_)` addresses; matches the pass's
///   [`LoadForward::stack_vn`] field).
/// - `sp_memo` — a per-call SP-decomposition memo.  Reuse the same memo
///   across multiple calls for the same graph to amortise the cost
///   of decomposing repeated SP expressions.
/// - `walk_memo` — a per-call result memo keyed on `(mem, offset,
///   value_type)`.  Reuse it across multiple per-index lookups in the
///   indirect-branch classifier so shared chain prefixes pay O(1) per node.
#[must_use]
pub(crate) fn find_stack_stored_value_at_offset(
    function: &strider_ir::Function,
    mem: NodeOutputId,
    offset: i64,
    value_type: NodeOutputType,
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    walk_memo: &mut StackStoredValueMemo,
) -> Option<NodeOutputId> {
    // Iterative form (was recursive; deep prologues blew the stack).
    // Walks the memory-chain backward via the Store's inputs[0] or
    // Store-passthrough's prev_mem.  Stack-safe at any chain depth.
    //
    // Visited stack records every `mem` node we passed through so we
    // can populate `walk_memo` for ALL of them once the terminal
    // result is known — preserves the prior memoisation behaviour
    // where every revisited prefix saved its result.
    let load_size = value_type.byte_size() as i64;
    let mut visited: Vec<(NodeOutputId, i64, NodeOutputType)> = Vec::new();
    let mut cur_mem = mem;

    let result: Option<NodeOutputId> = loop {
        let key = (cur_mem, offset, value_type);
        if let Some(&cached) = walk_memo.get(&key) {
            break cached;
        }
        visited.push(key);
        let node = function.node_for_output(cur_mem);
        match *function.node_kind(node) {
            NodeKind::Store(_) => {
                let inputs = function.node_inputs(node);
                if inputs.len() < 3 {
                    break None;
                }
                let addr = inputs[1];
                let data = inputs[2];
                match decompose_sp(function, addr, stack_vn, sp_memo) {
                    Some(SpExpr::Terminal { base: _, offset: k }) => {
                        let data_ty = function.output_kind(data).as_value();
                        match data_ty {
                            None => break None,
                            Some(data_ty) if k == offset => {
                                if data_ty == value_type {
                                    break Some(data);
                                }
                                break None;
                            }
                            Some(data_ty) => {
                                let store_size = data_ty.byte_size() as i64;
                                if ranges_disjoint(k, store_size, offset, load_size) {
                                    cur_mem = inputs[0];
                                    continue;
                                }
                                break None;
                            }
                        }
                    }
                    Some(SpExpr::Phi { .. }) => break None,
                    None => {
                        cur_mem = inputs[0];
                        continue;
                    }
                }
            }
            // MemPhi / InitialMemory / anything else: bail.  See module
            // notes for why MemPhi handling is intentionally future work.
            _ => break None,
        }
    };

    // Memoise every prefix on the way back so future queries reuse work.
    for key in visited {
        walk_memo.insert(key, result);
    }
    result
}

#[cfg(test)]
mod tests;
