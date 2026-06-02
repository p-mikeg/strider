use cranelift_entity::SecondaryMap;
use entity_utils::Worklist;

use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use strider_ir::{ExtendOp, IntBinaryOp};

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Per-output known-bits side-table.  Defaults to `KnownBitsFacts::default()`
/// (`{ones: 0, zeros: 0}` = "no info") for unrecorded outputs, which is
/// equivalent to "absent" in the previous `FxHashMap`-based form.
/// Migrated from `FxHashMap<NodeOutputId, KnownBitsFacts>` to `SecondaryMap` to
/// avoid hashing in the inner loop — at 10k+ nodes this is the
/// hottest probe in the entire `KnownBits` pass.
pub type KnownBitsMap = SecondaryMap<NodeOutputId, KnownBitsFacts>;

// ── Known-bits representation ─────────────────────────────────────────────────

/// Returns the all-ones bit mask for `ty` as a `u64`, or `None` if `ty` is not
/// an integer type or its width exceeds 64 bits.  Used by [`KnownBits`] to gate
/// out the wide integers (I80/I128/I256/I512) and the float types from the
/// u64-bounded analysis; the narrow integers (including the 1-bit `I1`) pass.
fn u64_type_mask(ty: NodeOutputType) -> Option<u64> {
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
/// Construct via [`KnownBitsFacts::from_const`] / [`KnownBitsFacts::default`] / [`KnownBitsFacts::merge`],
/// which preserve the invariant by construction; struct-literal
/// construction is `pub(crate)` and only used inside the analysis
/// where the masks are derived from already-validated `KnownBitsFacts` values.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct KnownBitsFacts {
    /// Bits that are definitely 1.
    ///
    /// `pub(crate)` because the `ones & zeros == 0` invariant is
    /// enforced only by [`KnownBitsFacts::merge`] / [`KnownBitsFacts::from_const`] — external
    /// struct-literal construction (`KnownBitsFacts { ones: 0xFF, zeros: 0xFF }`)
    /// would silently violate it.
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
    fn from_const(val: u128, ty: NodeOutputType) -> Option<Self> {
        let type_mask = u64_type_mask(ty)?;
        let masked = ty.get_unsigned_int(val)?;
        let masked_u64 = u64::try_from(masked).ok()?;
        Some(KnownBitsFacts {
            ones: masked_u64,
            zeros: type_mask ^ masked_u64,
        })
    }

    /// Returns `Ok(true)` if merging `other` into `self` changed anything.
    ///
    /// Returns `Err` on contradiction — a bit provably 1 in one source and
    /// provably 0 in the other.  That can only happen if the analyzer's
    /// inputs disagree: either KnownBits derived something wrong, or the
    /// IR contains incompatible constants reaching the same output.  Both
    /// are real bugs we want to surface rather than silently let `ones`
    /// win and lose the conflicting `zeros` info.
    fn merge(&mut self, other: KnownBitsFacts) -> Result<bool> {
        if self.ones & other.zeros != 0 || self.zeros & other.ones != 0 {
            return Err(anyhow::anyhow!(
                "KnownBitsFacts::merge contradiction: self={{ones:{:#x}, zeros:{:#x}}} other={{ones:{:#x}, zeros:{:#x}}}",
                self.ones, self.zeros, other.ones, other.zeros,
            ));
        }
        let new_ones = self.ones | other.ones;
        let new_zeros = self.zeros | other.zeros;
        if new_ones != self.ones || new_zeros != self.zeros {
            self.ones = new_ones;
            self.zeros = new_zeros;
            Ok(true)
        } else {
            Ok(false)
        }
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
    #[must_use]
    pub fn max_value(self, type_mask: u64) -> u64 {
        (!self.zeros) & type_mask
    }

}

// ── Per-node known-bits computation ───────────────────────────────────────────

/// Computes the known bits contributed by `node_id` toward its single integer
/// value output.  Returns `(output_id, KnownBitsFacts)` or `None` if the node has no
/// integer value output or no useful information can be extracted.
pub(crate) fn node_known_bits(
    ctx: strider_pattern::RewriteCtxView<'_>,
    node_id: NodeId,
    known: &KnownBitsMap,
) -> Result<Option<(NodeOutputId, KnownBitsFacts)>> {
    let kind = *ctx.node_kind(node_id);

    // Find the first integer value output.
    let Some(&out) = ctx
        .node_outputs(node_id)
        .iter()
        .find(|&&o| ctx.output_kind(o).is_integer())
    else {
        return Ok(None);
    };
    let out_kind = ctx.output_kind(out);
    let ty = out_kind.as_value_or_err()?;
    // KnownBits tracks 64-bit masks only; types wider than I64 (I128/I256,
    // produced by some x86 SIMD / misc. lifted ops) fall outside this pass.
    let Some(type_mask) = u64_type_mask(ty) else {
        return Ok(None);
    };

    let kb = match kind {
        NodeKind::IntConst(v) => match KnownBitsFacts::from_const(v, ty) {
            Some(kb) => kb,
            // Untracked type (Bool, float, I128, I256) — defer to default
            // "fully unknown" via the worklist's missing-entry path.
            None => return Ok(None),
        },

        NodeKind::IntBinaryOp(op) => {
            let [lhs, rhs] = ctx.node_inputs_exact::<2>(node_id)?;
            let l = known[lhs];
            let r = known[rhs];
            match op {
                IntBinaryOp::And => KnownBitsFacts {
                    ones: l.ones & r.ones,
                    zeros: (l.zeros | r.zeros) & type_mask,
                },
                IntBinaryOp::Or => KnownBitsFacts {
                    ones: (l.ones | r.ones) & type_mask,
                    zeros: l.zeros & r.zeros,
                },
                IntBinaryOp::Xor => KnownBitsFacts {
                    // bit is known 1 if exactly one input is known 1.
                    ones: (l.ones & r.zeros) | (l.zeros & r.ones),
                    // bit is known 0 if both inputs agree (both 0 or both 1).
                    zeros: (l.ones & r.ones) | (l.zeros & r.zeros),
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
                    let rhs_mask = ctx
                        .output_kind(rhs)
                        .as_value()
                        .and_then(u64_type_mask)
                        .unwrap_or(u64::MAX);
                    let rhs_kb = known[rhs];
                    if rhs_kb.all_known(rhs_mask) {
                        let bit_width = ty.bit_width() as u64;
                        if rhs_kb.ones >= bit_width {
                            return Ok(Some((
                                out,
                                KnownBitsFacts {
                                    ones: 0,
                                    zeros: type_mask,
                                },
                            )));
                        }
                        let shift = rhs_kb.ones as u32;
                        let lower_mask = (1u64 << shift).wrapping_sub(1) & type_mask;
                        let shifted_ones = (l.ones << shift) & type_mask;
                        let shifted_zeros = ((l.zeros << shift) & type_mask) | lower_mask;
                        return Ok(Some((
                            out,
                            KnownBitsFacts {
                                ones: shifted_ones,
                                zeros: shifted_zeros & !shifted_ones,
                            },
                        )));
                    }
                    return Ok(None);
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
                    let rhs_mask = ctx
                        .output_kind(rhs)
                        .as_value()
                        .and_then(u64_type_mask)
                        .unwrap_or(u64::MAX);
                    let rhs_kb = known[rhs];
                    if rhs_kb.all_known(rhs_mask) {
                        let bit_width = ty.bit_width() as u64;
                        if rhs_kb.ones >= bit_width {
                            return Ok(Some((
                                out,
                                KnownBitsFacts {
                                    ones: 0,
                                    zeros: type_mask,
                                },
                            )));
                        }
                        let shift = rhs_kb.ones as u32;
                        let upper_mask = !(type_mask >> shift) & type_mask;
                        let shifted_ones = (l.ones & type_mask) >> shift;
                        let shifted_zeros = ((l.zeros & type_mask) >> shift) | upper_mask;
                        return Ok(Some((
                            out,
                            KnownBitsFacts {
                                ones: shifted_ones,
                                zeros: shifted_zeros & !shifted_ones,
                            },
                        )));
                    }
                    return Ok(None);
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
            let [input] = ctx.node_inputs_exact::<1>(node_id)?;
            let kb = known[input];
            KnownBitsFacts {
                ones: kb.ones & type_mask,
                zeros: kb.zeros & type_mask,
            }
        }

        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            // Upper bits are explicitly zeroed by the extension.
            let [input] = ctx.node_inputs_exact::<1>(node_id)?;
            let input_kind = ctx.output_kind(input);
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
            let kb = known[input];
            KnownBitsFacts {
                ones: kb.ones,
                zeros: kb.zeros | (type_mask ^ input_mask), // upper bits are 0
            }
        }

        NodeKind::Extend(ExtendOp::SignExtend) => {
            // Upper bits replicate the input's sign bit.  When the sign bit
            // is statically known, the entire upper region is determined;
            // otherwise we still pass the lower bits through.
            let [input] = ctx.node_inputs_exact::<1>(node_id)?;
            let input_kind = ctx.output_kind(input);
            let input_ty = input_kind.as_value_or_err()?;
            let Some(input_mask) = u64_type_mask(input_ty) else {
                return Ok(None);
            };
            let kb = known[input];
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
            let [input] = ctx.node_inputs_exact::<1>(node_id)?;
            let input_kind = ctx.output_kind(input);
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

    Ok(Some((out, kb)))
}

// ── Read-only analyzer ────────────────────────────────────────────────────────

/// Runs the known-bits worklist analysis to fixed point and returns the
/// resulting `KnownBitsFacts` map keyed by [`NodeOutputId`].  Pure — does not mutate
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
/// Returns an `Err` if a per-node KnownBitsFacts derivation fails — e.g. a node
/// whose recorded output type is wider than 64 bits combined with a
/// shape that requires `node_inputs_exact` to read a fixed input
/// arity.  In practice the only path to error is malformed IR;
/// well-formed graphs always converge.
pub fn analyze(ctx: strider_pattern::RewriteCtxView<'_>) -> Result<KnownBitsMap> {
    // Seed with every reachable node; consumers re-enqueue on input
    // change via `output_uses`.  `Worklist` is the shared dedup-FIFO
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
    let mut work: Worklist<NodeId> = ctx.graph_ref().walk_from(ctx.entry()).collect();
    while let Some(node_id) = work.dequeue() {
        let Some((out, kb)) = node_known_bits(ctx, node_id, &known)? else {
            continue;
        };
        let merged = known[out].merge(kb)?;
        if !merged {
            continue;
        }
        for (consumer, _idx) in ctx.output_uses(out) {
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
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        _opt_ctx: &crate::opt::OptCtx<'_>,
    ) -> crate::opt::Result<OptimizationResult> {
        // Analyze pass — propagate known bits to fixed point.  Read-only;
        // shared with the jump-table classifier (and any other caller
        // that needs bit-knowledge without graph rewrites).
        let known = analyze(ctx.as_view())?;

        // Rewrite pass — replace fully-determined outputs with constants.  Drive
        // via Worklist so a rewritten node's consumers are re-checked in the
        // same call: a freshly-introduced IntConst can let a sibling whose
        // *other* operand was previously unknown become fully-determined.
        let mut work: Worklist<NodeId> = ctx.walk().collect();
        let mut result = OptimizationResult::NoChange;
        // Inline up to 4 outputs / 8 consumers per iteration — these
        // bounds cover the vast majority of IR nodes (most ops have
        // 1 output and ≤ 8 consumers); larger nodes spill to the heap
        // transparently.  Saves a heap allocation per worklist pop on
        // the hot rewrite path.
        let mut outputs: smallvec::SmallVec<[NodeOutputId; 4]> = smallvec::SmallVec::new();
        let mut consumers: smallvec::SmallVec<[NodeId; 8]> = smallvec::SmallVec::new();
        while let Some(node_id) = work.dequeue() {
            // Already-constant nodes have nothing to rewrite.
            if matches!(*ctx.node_kind(node_id), NodeKind::IntConst(_)) {
                continue;
            }
            outputs.clear();
            outputs.extend(ctx.node_outputs(node_id).iter().copied());
            for &out in &outputs {
                let Some(ty) = ctx.output_kind(out).as_value() else {
                    continue;
                };
                if !ty.is_integer() {
                    continue;
                }
                // Skip types KnownBits doesn't track (I128/I256).
                let Some(type_mask) = u64_type_mask(ty) else {
                    continue;
                };
                // SecondaryMap returns `KnownBitsFacts::default()` (zero-known) for
                // unrecorded outputs — `all_known` then returns false,
                // which short-circuits the same way the prior
                // `Some(&kb) = known.get(&out) else { continue };` did.
                let kb = known[out];
                if !kb.all_known(type_mask) {
                    continue;
                }
                consumers.clear();
                for (consumer, _) in ctx.output_uses(out) {
                    consumers.push(consumer);
                }
                let new_out = ctx.make_int_const(kb.ones, ty)?;
                // Absorb the rewritten node's fingerprint into the new
                // const via `after_replace` (handles fingerprint union +
                // replace_all_uses).
                let after = OptimizationResult::NoChange.after_replace(ctx, out, new_out)?;
                if after.changed() {
                    result = OptimizationResult::Changed;
                    for &consumer in &consumers {
                        work.enqueue(consumer);
                    }
                }
            }
        }
        Ok(result)
    }
}

