use cranelift_entity::SecondaryMap;
use entity_utils::Worklist;

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{ExtendOp, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Per-output known-bits side-table.  Defaults to `KnownBitsFacts::default()`
/// (`{ones: 0, zeros: 0}` = "no info") for unrecorded outputs, which is
/// equivalent to "absent" in the previous `FxHashMap`-based form.
/// Migrated from `FxHashMap<ValueId, KnownBitsFacts>` to `SecondaryMap` to
/// avoid hashing in the inner loop — at 10k+ nodes this is the
/// hottest probe in the entire `KnownBits` pass.
pub type KnownBitsMap = SecondaryMap<ValueId, KnownBitsFacts>;

// ── Known-bits representation ─────────────────────────────────────────────────

/// Returns the all-ones bit mask for `ty` as a `u64`, or `None` if `ty` is not
/// an integer type or its width exceeds 64 bits.  Used by [`KnownBits`] to gate
/// out the wide integers (I80/I128/I256/I512) and the float types from the
/// u64-bounded analysis; the narrow integers (including the 1-bit `I1`) pass.
pub(crate) fn u64_type_mask(ty: ValueType) -> Option<u64> {
    if !ty.is_integer() || !ty.fits_u64() {
        return None;
    }
    u64::try_from(ty.bit_mask_u128()).ok()
}

/// Known-bit information for a single output.
///
/// Both `ones` and `zeros` are masked to the output type's width and must
/// never overlap (`ones & zeros == 0`).
///
/// Construct via [`KnownBitsFacts::from_const`] / [`KnownBitsFacts::default`],
/// which preserve the invariant by construction; struct-literal
/// construction is `pub(crate)` and only used inside the analysis
/// where the masks are derived from already-validated `KnownBitsFacts` values.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct KnownBitsFacts {
    /// Bits that are definitely 1.
    ///
    /// `pub(crate)` because the `ones & zeros == 0` invariant is
    /// enforced only by [`KnownBitsFacts::from_const`] and the transfer
    /// function — external struct-literal construction
    /// (`KnownBitsFacts { ones: 0xFF, zeros: 0xFF }`) would silently
    /// violate it.
    pub(crate) ones: u64,
    /// Bits that are definitely 0.  Same caveat as [`Self::ones`].
    pub(crate) zeros: u64,
}

impl KnownBitsFacts {
    /// Build the `KnownBitsFacts` for an integer constant.  Returns `None` for
    /// types this analysis doesn't track (`Bool`, floats, I128, I256):
    /// the caller treats `None` as "fully unknown" and skips
    /// propagation, which is the correct sound behaviour for a
    /// 64-bit-bound bit-tracker.  Previously this collapsed to
    /// all-ones-zeros (i.e. `ones=0, zeros=0`) silently — same effect
    /// as "unknown" but indistinguishable from a deliberate zero.
    fn from_const(val: u128, ty: ValueType) -> Option<Self> {
        let type_mask = u64_type_mask(ty)?;
        let masked = ty.get_unsigned_int(val)?;
        let masked_u64 = u64::try_from(masked).ok()?;
        Some(KnownBitsFacts {
            ones: masked_u64,
            zeros: type_mask ^ masked_u64,
        })
    }

    /// Returns `true` if all bits of `type_mask` are determined.
    fn all_known(self, type_mask: u64) -> bool {
        (self.ones | self.zeros) & type_mask == type_mask
    }

    /// Upper bound on the runtime value of an output with these known bits.
    ///
    /// `(!zeros) & type_mask` is the OR of every bit position that *could*
    /// be 1, so the runtime value is `<=` this number.  Used by analyses
    /// that need a single-number bound rather than separate ones/zeros
    /// (e.g. the jump-table classifier's index-bound check).
    pub fn max_value(self, type_mask: u64) -> u64 {
        (!self.zeros) & type_mask
    }
}

/// Outcome of resolving a shift's RHS against the output width, shared by the
/// `ShiftLeft` / `ShiftRight` arms.
enum ConstShift {
    /// The shift amount is fully known and strictly below the output width.
    InRange(u32),
    /// The shift amount is fully known but at-or-past the output width;
    /// Sleigh returns 0, so every output bit is known zero.
    OverWidth,
    /// The shift amount is not fully known — no information.
    Unknown,
}

/// Classify a shift's RHS known bits against the output type width.
///
/// Mirrors Sleigh's `OpBehaviorInt{Left,Right}::evaluateBinary`, which return 0
/// for any shift amount `>= bit_width`.
fn classify_const_shift(rhs_kb: KnownBitsFacts, rhs_mask: u64, bit_width: u64) -> ConstShift {
    if !rhs_kb.all_known(rhs_mask) {
        return ConstShift::Unknown;
    }
    if rhs_kb.ones >= bit_width {
        return ConstShift::OverWidth;
    }
    ConstShift::InRange(rhs_kb.ones as u32)
}

/// Direction of a logical shift, selecting the per-direction `InRange` bit-math
/// in [`shift_known_bits`].
#[derive(Clone, Copy)]
enum ShiftDir {
    Left,
    Right,
}

/// Known-bits transfer for the `ShiftLeft` / `ShiftRight` arms.
///
/// Reads the RHS mask, classifies the (constant) shift against the output
/// width, and — for an in-range shift — applies the direction-specific
/// bit-math (left: low bits become known-zero, surviving bits move up; right:
/// upper bits become known-zero, surviving bits move down).  Returns `None`
/// for an unknown shift amount (caller defers to "fully unknown"); an
/// over-width shift yields all-zeros.  Shared scaffolding for both arms — only
/// the per-direction math differs.
fn shift_known_bits(
    ctx: &strider_ir::Function,
    l: KnownBitsFacts,
    rhs: ValueId,
    rhs_kb: KnownBitsFacts,
    ty: ValueType,
    type_mask: u64,
    dir: ShiftDir,
) -> Option<KnownBitsFacts> {
    let rhs_mask = ctx
        .value_type_opt(rhs)
        .and_then(u64_type_mask)
        .unwrap_or(u64::MAX);
    match classify_const_shift(rhs_kb, rhs_mask, ty.bit_width() as u64) {
        ConstShift::Unknown => None,
        ConstShift::OverWidth => Some(KnownBitsFacts {
            ones: 0,
            zeros: type_mask,
        }),
        ConstShift::InRange(shift) => {
            let (shifted_ones, shifted_zeros) = match dir {
                ShiftDir::Left => {
                    let lower_mask = (1u64 << shift).wrapping_sub(1) & type_mask;
                    (
                        (l.ones << shift) & type_mask,
                        ((l.zeros << shift) & type_mask) | lower_mask,
                    )
                }
                ShiftDir::Right => {
                    let upper_mask = !(type_mask >> shift) & type_mask;
                    (
                        (l.ones & type_mask) >> shift,
                        ((l.zeros & type_mask) >> shift) | upper_mask,
                    )
                }
            };
            Some(KnownBitsFacts {
                ones: shifted_ones,
                zeros: shifted_zeros & !shifted_ones,
            })
        }
    }
}

// ── Per-node known-bits computation ───────────────────────────────────────────

/// Computes the known bits contributed by `node_id` toward its single integer
/// value output.  Returns `(output_id, KnownBitsFacts)` or `None` if the node has no
/// integer value output or no useful information can be extracted.
pub(crate) fn node_known_bits(
    ctx: &strider_ir::Function,
    node_id: NodeId,
    known: &KnownBitsMap,
) -> Result<Option<(ValueId, KnownBitsFacts)>> {
    let kind = *ctx.node_kind(node_id);

    // Find the first integer value output.
    let Some(&out) = ctx
        .node_outputs(node_id)
        .iter()
        .find(|&&o| ctx.value_kind(o).is_integer())
    else {
        return Ok(None);
    };
    let out_kind = ctx.value_kind(out);
    let ty = out_kind.as_value_or_err()?;
    // KnownBits tracks 64-bit masks only; types wider than I64 (I128/I256,
    // produced by some x86 SIMD / misc. lifted ops) fall outside this pass.
    let Some(type_mask) = u64_type_mask(ty) else {
        return Ok(None);
    };

    let kb = match kind {
        NodeKind::IntConst(_) => match ctx
            .int_const_u128(out)
            .and_then(|v| KnownBitsFacts::from_const(v, ty))
        {
            Some(kb) => kb,
            // Untracked type (Bool, float, I128, I256) — defer to default
            // "fully unknown" via the worklist's missing-entry path.
            None => return Ok(None),
        },

        NodeKind::IntBinaryOp(op) => {
            // IntBinaryOp has exactly 2 inputs (validated structural invariant).
            let [lhs, rhs] = ctx
                .graph()
                .node_inputs_exact::<2>(node_id)
                .expect("IntBinaryOp has 2 inputs per node signature");
            let l = known[lhs];
            let r = known[rhs];
            match op {
                // All four lattice fields are masked to `type_mask`: operands
                // may legally be wider than this node's output (the validator
                // does not pin operand width == output width), so an operand's
                // above-width known bit must not survive into this node's facts.
                IntBinaryOp::And => KnownBitsFacts {
                    ones: l.ones & r.ones & type_mask,
                    zeros: (l.zeros | r.zeros) & type_mask,
                },
                IntBinaryOp::Or => KnownBitsFacts {
                    ones: (l.ones | r.ones) & type_mask,
                    zeros: l.zeros & r.zeros & type_mask,
                },
                IntBinaryOp::Xor => KnownBitsFacts {
                    // bit is known 1 if exactly one input is known 1.
                    ones: ((l.ones & r.zeros) | (l.zeros & r.ones)) & type_mask,
                    // bit is known 0 if both inputs agree (both 0 or both 1).
                    zeros: ((l.ones & r.ones) | (l.zeros & r.zeros)) & type_mask,
                },
                IntBinaryOp::ShiftLeft => {
                    // Lower bits of a left-shifted value are known zero;
                    // surviving lhs bits move up by `shift` positions and
                    // carry their `ones`/`zeros` with them.
                    //
                    // Sleigh's `OpBehaviorIntLeft::evaluateBinary` returns 0
                    // when the shift amount is `>= bit_width` (sleigh/src/
                    // opbehavior.cc:411).  Mirror that here — pre-fix the
                    // arm masked the shift with `(bit_width - 1)` and
                    // wrapped large literal shifts back into range,
                    // producing the wrong known-bits result for any
                    // literal shift at-or-past the type width.
                    return Ok(shift_known_bits(ctx, l, rhs, r, ty, type_mask, ShiftDir::Left)
                        .map(|facts| (out, facts)));
                }
                IntBinaryOp::ShiftRight => {
                    // Logical right-shift: upper bits become 0; lhs bits
                    // shift down by `shift` positions and bring their
                    // known-bit information with them.
                    //
                    // Sleigh `OpBehaviorIntRight::evaluateBinary`: shift
                    // `>= bit_width` returns 0 (sleigh/src/opbehavior.cc:432).
                    // Mirror that here — see the ShiftLeft arm for the
                    // pre-fix bug rationale.
                    return Ok(shift_known_bits(ctx, l, rhs, r, ty, type_mask, ShiftDir::Right)
                        .map(|facts| (out, facts)));
                }
                _ => return Ok(None),
            }
        }

        // Note: bitwise complement (`~x`) is `Xor(x, all_ones)` since
        // the former BitNot unary-op was removed in favour of the Xor shape.
        // The `IntBinaryOp::Xor` arm above already swaps known ones/zeros
        // correctly when one operand is a fully-known all-ones constant
        // (every bit position has `r.ones = type_mask`, `r.zeros = 0`,
        // making the result's `ones = l.zeros & type_mask` and
        // `zeros = l.ones & type_mask` — identical to the old BitNot
        // arm).  Two's-complement negate — `IntUnaryOp::Neg` — has no
        // closed-form known-bits propagation: it depends on the borrow
        // chain across the input's bits, so it falls through to the
        // unknown case.
        NodeKind::Truncate => {
            // Upper bits of the source are discarded; lower bits are preserved.
            // Truncate has exactly 1 input (validated structural invariant).
            let [value] = ctx
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Truncate has 1 input per node signature");
            let kb = known[value];
            KnownBitsFacts {
                ones: kb.ones & type_mask,
                zeros: kb.zeros & type_mask,
            }
        }

        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            // Upper bits are explicitly zeroed by the extension.
            // Extend has exactly 1 input (validated structural invariant).
            let [value] = ctx
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Extend has 1 input per node signature");
            let input_kind = ctx.value_kind(value);
            let input_ty = input_kind.as_value_or_err()?;
            // Bail when the input width is unsupported (I80/I128/I256) —
            // mirrors the SignExtend arm below.  Returning `Ok(None)`
            // leaves the output's KB at "fully unknown"; the previous
            // `unwrap_or(0)` here would have set `input_mask = 0` and
            // marked every bit as known-zero, silently corrupting
            // analysis on wide-to-wider ZeroExtends.
            let Some(input_mask) = u64_type_mask(input_ty) else {
                return Ok(None);
            };
            let kb = known[value];
            KnownBitsFacts {
                ones: kb.ones,
                zeros: kb.zeros | (type_mask ^ input_mask), // upper bits are 0
            }
        }

        NodeKind::Extend(ExtendOp::SignExtend) => {
            // Upper bits replicate the input's sign bit.  When the sign bit
            // is statically known, the entire upper region is determined;
            // otherwise we still pass the lower bits through.
            // Extend has exactly 1 input (validated structural invariant).
            let [value] = ctx
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Extend has 1 input per node signature");
            let input_kind = ctx.value_kind(value);
            let input_ty = input_kind.as_value_or_err()?;
            let Some(input_mask) = u64_type_mask(input_ty) else {
                return Ok(None);
            };
            let kb = known[value];
            // Sign bit = highest bit of the input width.
            let sign_bit = (input_mask >> 1) + 1;
            let upper_mask = type_mask & !input_mask;
            if kb.ones & sign_bit != 0 {
                // Sign bit known 1 → upper bits all known 1.
                KnownBitsFacts {
                    ones: (kb.ones & input_mask) | upper_mask,
                    zeros: kb.zeros & input_mask,
                }
            } else if kb.zeros & sign_bit != 0 {
                // Sign bit known 0 → upper bits all known 0.
                KnownBitsFacts {
                    ones: kb.ones & input_mask,
                    zeros: (kb.zeros & input_mask) | upper_mask,
                }
            } else {
                // Sign bit unknown → keep only the lower bits' knowledge.
                KnownBitsFacts {
                    ones: kb.ones & input_mask,
                    zeros: kb.zeros & input_mask,
                }
            }
        }

        NodeKind::Popcount | NodeKind::Lzcount => {
            // Result is in [0, bit_width(input)].  Bits above ceil_log2(bit_width+1) are zero.
            // Popcount / Lzcount have exactly 1 input (validated structural invariant).
            let [value] = ctx
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Popcount / Lzcount have 1 input per node signature");
            let input_kind = ctx.value_kind(value);
            let input_ty = input_kind.as_value_or_err()?;
            let max_val = input_ty.bit_width() as u64;
            let bits_needed = if max_val == 0 {
                1
            } else {
                u64::BITS - max_val.leading_zeros()
            } as u64;
            let result_mask = if bits_needed >= 64 {
                u64::MAX
            } else {
                (1u64 << bits_needed) - 1
            };
            let upper_zeros = type_mask & !result_mask;
            KnownBitsFacts {
                ones: 0,
                zeros: upper_zeros,
            }
        }

        _ => return Ok(None),
    };

    // The transfer function must never produce a bit that is provably both
    // 1 and 0.  A contradiction here would mean the lattice/transfer logic
    // itself is inconsistent — surface it in debug builds at the point of
    // origin rather than letting it silently propagate.  (This replaces the
    // old `KnownBitsFacts::merge` contradiction check, which could only ever
    // have fired on a transfer-function bug since each output's facts are
    // recomputed from scratch and overwritten — never unioned.)
    debug_assert_eq!(
        kb.ones & kb.zeros,
        0,
        "node_known_bits produced overlapping ones/zeros: {kb:?}",
    );

    Ok(Some((out, kb)))
}

/// Whether [`node_known_bits`] derives this kind's known bits **from its
/// inputs** — i.e. the kinds whose match arms read `known[input]`.  These are
/// the node kinds along which known-bits *provenance* flows: the fold-time
/// fingerprint walk recurses through them and stops at every other kind, so a
/// `Load` / `Phi` / `Call` (which `node_known_bits` treats as opaque, reading
/// none of their inputs) is a leaf — its memory / address / control cone
/// never contributed a bit and so is never tainted.
fn propagates_known_bits(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::IntBinaryOp(_)
            | NodeKind::Truncate
            | NodeKind::Extend(_)
            | NodeKind::Popcount
            | NodeKind::Lzcount
    )
}

// ── Read-only analyzer ────────────────────────────────────────────────────────

/// Runs the known-bits worklist analysis to fixed point and returns the
/// resulting `KnownBitsFacts` map keyed by [`ValueId`].  Pure — does not mutate
/// the graph; the [`KnownBits`] optimizer pass is layered on top of this
/// to perform constant-replacement rewrites.
///
/// Other passes (e.g. the indirect-branch jump-table classifier) call this
/// directly when they need a non-mutating bit-knowledge query rather than
/// a graph-rewriting optimizer pass.  The fixed-point analysis is at least
/// as tight as any single-pass local recurrence: it propagates across more
/// node kinds and follows data-dependency chains farther.
///
/// Outputs absent from the returned map have no statically-proven bit
/// information; treat them as the all-unknown default `KnownBitsFacts { ones: 0,
/// zeros: 0 }`.
///
/// # Errors
///
/// Returns an `Err` if a per-node KnownBitsFacts derivation fails — e.g. an
/// arithmetic op whose recorded output type is wider than 64 bits.  Wrong
/// input *arity* is **not** surfaced as `Err`: the `node_inputs_exact`
/// accessors `.expect(...)` (panic) on a malformed reachable node, per the
/// panic-on-validator-invariant policy — the validator runs before KnownBits,
/// so a reachable node always has its signature arity.  Well-formed graphs
/// always converge.
pub fn analyze(ctx: &strider_ir::Function) -> Result<KnownBitsMap> {
    // Seed with every reachable node; consumers re-enqueue on input
    // change via `value_uses`.  `Worklist` is the shared dedup-FIFO
    // worklist used by ConstantFold and DeadBranchElimination — no
    // local re-implementation.
    //
    // Detached "zombie" nodes (left behind by PhiCollapse,
    // DeadBranchElimination, etc.) are deliberately excluded:
    // `node_known_bits` calls `node_inputs_exact::<N>` which would
    // surface a hard error on a zero-input zombie.  Reachability is
    // the validator's existing scope-of-correctness boundary
    // (the local-typing check in `strider_ir::validate`), so it's the right scope here too.
    let mut known: KnownBitsMap = SecondaryMap::new();
    // Seed in reverse-post-order (operands before consumers) so the first
    // visit of each node already sees its inputs' facts — the monotone
    // worklist fixpoint converges to the same result from any seed order, but
    // RPO minimises the re-enqueue churn.
    let mut work: Worklist<NodeId> = ctx.reverse_postorder_filter(|_| true).collect();
    while let Some(node_id) = work.dequeue() {
        let Some((out, kb)) = node_known_bits(ctx, node_id, &known)? else {
            continue;
        };
        // The transfer function recomputes `kb` from scratch from the
        // inputs' *current* facts every visit, and the recompute is
        // monotonically more precise than the stored value (which starts at
        // the all-unknown default).  So we overwrite directly — there is no
        // union-with-previous to perform.  Re-enqueue consumers only when
        // the freshly computed facts differ from what was already stored.
        if known[out] == kb {
            continue;
        }
        known[out] = kb;
        for (consumer, _idx) in ctx.graph().value_uses(out) {
            work.enqueue(consumer);
        }
    }
    Ok(known)
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Propagates known-bit information and replaces outputs whose every bit is
/// determined with an equivalent integer constant.
///
/// Handles `IntConst`, `And`, `Or`, `Xor`, `Truncate`, `ZeroExtend`,
/// and constant-shift nodes.  (Bitwise complement is `Xor(x, all_ones)`,
/// already covered by the Xor arm.)  Runs a fixed-point inner loop to
/// propagate information along data-dependency chains before deciding
/// replacements.
#[derive(Clone)]
pub struct KnownBits;

impl Optimizer for KnownBits {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::OptCtx<'_>,
    ) -> crate::Result<OptimizationResult> {
        // Analyze pass — propagate known bits to fixed point.  Read-only;
        // shared with the jump-table classifier (and any other caller
        // that needs bit-knowledge without graph rewrites).
        let known = analyze(ctx.function())?;

        // Rewrite pass — a flat iteration over the finished fixed-point map.
        // The fixpoint already happened in `analyze`, so a fully-determined
        // output is a pure per-output decision: replace it with the
        // equivalent constant.  No second worklist and no consumer
        // re-enqueue are needed — order is irrelevant, and each output is
        // visited exactly once (the map holds one entry per output the
        // analysis populated, so detached/zombie outputs never appear).
        //
        // `SecondaryMap::iter` densely covers every `ValueId` up to the
        // high-water mark the analysis touched; the per-entry guards below
        // skip the default ("fully unknown") entries naturally, since
        // `all_known` is false for `ones == 0 && zeros == 0` against any
        // non-zero `type_mask`.
        //
        // Collect the targets first (releasing the read borrow on `known` /
        // `ctx`) before mutating, so the rewrite loop owns `&mut ctx`.
        let to_fold: Vec<(ValueId, ValueType, u64)> = known
            .iter()
            .filter_map(|(value, &kb)| {
                // Skip outputs whose kind is not an integer value
                // (control / memory / phi-token).
                let ty = ctx.value_type_opt(value)?;
                if !ty.is_integer() {
                    return None;
                }
                // Skip types KnownBits doesn't track (I80/I128/I256/…).
                let type_mask = u64_type_mask(ty)?;
                // Skip outputs that are not fully determined.
                if !kb.all_known(type_mask) {
                    return None;
                }
                // Skip outputs whose producer is already an `IntConst`
                // (folding it would be a no-op).
                let producer = ctx.producer(value);
                if matches!(*ctx.node_kind(producer), NodeKind::IntConst(_)) {
                    return None;
                }
                Some((value, ty, kb.ones))
            })
            .collect();

        // Each fold is justified by the KNOWN BITS feeding `value`'s producer
        // — the contributor cone is about to be cascade-culled, so its
        // asm-fingerprints (the proof of WHY the result is this constant)
        // would be lost.  A one-hop absorb of the direct inputs is NOT enough:
        // a contributor that establishes known bits but is itself not fully
        // known (e.g. the `x & 1` in `((x & 1) | 2) & 0`) never folds, so the
        // fixpoint can never carry its fingerprint up.  Each fold must absorb
        // the union of its producer's CONTRIBUTOR cone — recursing only
        // through kinds whose known bits `node_known_bits` derives from their
        // inputs (`propagates_known_bits`), stopping at opaque kinds (`Load` /
        // `Phi` / `Call` / …) whose memory / address / control cones never
        // contributed a bit and must not be tainted.
        //
        // Many folds share a large upstream cone, so per-fold walking is
        // O(folds·cone).  Instead, precompute ONCE — over the pre-fold graph,
        // before the mutating `replace_value` cascade culls contributors — a
        // memo of each cone node's fingerprint-address set.  Each fold then
        // unions its inputs' memoized cone sets in O(arity).  The cone is
        // acyclic (the propagates kinds are IntBinaryOp / Truncate / Extend /
        // Popcount / Lzcount — never Phi / Region, the only sources of data
        // cycles), so a memoized DFS needs no cycle handling.  Over-tainting
        // WITHIN a contributor cone (e.g. both operands of `& 0`) is
        // intentional — the fingerprint is a generous superset proof aid.
        let cone_fps = build_cone_fingerprint_memo(ctx, &to_fold);

        let mut result = OptimizationResult::NoChange;
        for (value, ty, ones) in to_fold {
            let new_value = ctx.build_int_const(ones, ty)?;
            let folded_producer = ctx.producer(value);
            let new_producer = ctx.producer(new_value);
            // Seed from the producer's INPUT cones (the producer itself is
            // absorbed by `replace_value` below).  Mirrors the previous walk's
            // seeding exactly, so the resulting fingerprint set is identical.
            for p in crate::peephole::input_producers(ctx, folded_producer) {
                if let Some(addrs) = cone_fps.get(&p) {
                    ctx.function_mut()
                        .extend_asm_fingerprint(new_producer, addrs);
                }
            }
            // `replace_value` absorbs the rewritten node's fingerprint into
            // the new const (superset-only union) and redirects every use.
            if ctx.replace_value(value, new_value)? {
                result = OptimizationResult::Changed;
            }
        }
        Ok(result)
    }
}

/// The per-node set of asm-fingerprint addresses in a node's known-bits
/// contributor cone (sorted, deduplicated).
type ConeFps = smallvec::SmallVec<[u64; 8]>;

/// Precomputes, for every node in the known-bits contributor cone reachable
/// from the `to_fold` producers' inputs, the union of asm-fingerprint
/// addresses in that node's cone:
///
/// `cone_fps(n) = asm_fingerprint(n) ∪ (propagates(n) ? ⋃ₚ cone_fps(p) : ∅)`
///
/// where `p` ranges over `n`'s input producers.  This exactly mirrors the
/// per-fold DFS it replaces — every visited node contributes its own
/// fingerprint and recursion continues only through `propagates_known_bits`
/// kinds — but each node's set is computed once and returned from the memo on
/// every revisit, so F folds sharing a cone of size C cost O(C) rather than
/// O(F·C), while every fold still receives the FULL union (no attribution
/// lost to a cross-fold visited set).
///
/// Computed over the pre-fold graph before any `replace_value` runs:
/// contributor fingerprints don't change during folding (only new const nodes
/// are extended), so the snapshot is correct.  The cone is acyclic, so the
/// iterative postorder needs no cycle handling.
fn build_cone_fingerprint_memo(
    ctx: &crate::EditFunction<'_>,
    to_fold: &[(ValueId, ValueType, u64)],
) -> rustc_hash::FxHashMap<NodeId, ConeFps> {
    let mut memo: rustc_hash::FxHashMap<NodeId, ConeFps> = rustc_hash::FxHashMap::default();
    // Iterative postorder: push (node, children_emitted). On first pop, push
    // the node back as `true` and push its propagates-children; on the second
    // pop, every child's set is in `memo`, so fold them in.
    let mut stack: Vec<(NodeId, bool)> = Vec::new();
    let seed_inputs = to_fold
        .iter()
        .flat_map(|&(value, _, _)| crate::peephole::input_producers_iter(ctx, ctx.producer(value)));
    for n in seed_inputs {
        stack.push((n, false));
    }
    while let Some((n, expanded)) = stack.pop() {
        if expanded {
            // Children resolved — assemble this node's set.
            let mut addrs: ConeFps = ctx.function().asm_fingerprint(n).iter().copied().collect();
            if propagates_known_bits(ctx.node_kind(n)) {
                for p in crate::peephole::input_producers_iter(ctx, n) {
                    if let Some(child) = memo.get(&p) {
                        addrs.extend_from_slice(child);
                    }
                }
            }
            addrs.sort_unstable();
            addrs.dedup();
            memo.insert(n, addrs);
            continue;
        }
        if memo.contains_key(&n) {
            continue;
        }
        stack.push((n, true));
        if propagates_known_bits(ctx.node_kind(n)) {
            for p in crate::peephole::input_producers_iter(ctx, n) {
                if !memo.contains_key(&p) {
                    stack.push((p, false));
                }
            }
        }
    }
    memo
}
