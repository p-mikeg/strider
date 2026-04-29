use rustc_hash::FxHashMap;

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{BuiltFunctionGraph, ExtendOp, IntBinaryOp, IntUnaryOp};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, OptimizerOnBuilt};
use crate::worklist::WorkSet;

#[cfg(test)]
mod tests;

// ── Known-bits representation ─────────────────────────────────────────────────

/// Returns the all-ones bit mask for `ty` as a `u64`, or `None` if `ty` is not
/// an integer type or its width exceeds 64 bits.  Used by [`KnownBits`] to gate
/// out U80/U128/U256 (and Bool/floats) from the u64-bounded analysis.
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
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Kb {
    /// Bits that are definitely 1.
    pub ones: u64,
    /// Bits that are definitely 0.
    pub zeros: u64,
}

impl Kb {
    fn from_const(val: u128, ty: NodeOutputType) -> Self {
        let masked = ty.get_unsigned_int(val).unwrap_or(0);
        let type_mask = u64_type_mask(ty).unwrap_or(0);
        let masked_u64 = u64::try_from(masked).unwrap_or(0);
        Kb {
            ones: masked_u64,
            zeros: type_mask ^ masked_u64,
        }
    }

    /// Returns `true` if merging `other` into `self` changed anything.
    ///
    /// On conflict (a bit known 1 in one source and 0 in the other), the
    /// `ones` set wins and the conflicting bit is cleared from `zeros`,
    /// preserving the `ones & zeros == 0` invariant.
    fn merge(&mut self, other: Kb) -> bool {
        let new_ones = self.ones | other.ones;
        let new_zeros = (self.zeros | other.zeros) & !new_ones;
        if new_ones != self.ones || new_zeros != self.zeros {
            self.ones = new_ones;
            self.zeros = new_zeros;
            true
        } else {
            false
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
/// value output.  Returns `(output_id, Kb)` or `None` if the node has no
/// integer value output or no useful information can be extracted.
pub fn node_known_bits(
    fg: &BuiltFunctionGraph,
    node_id: NodeId,
    known: &FxHashMap<NodeOutputId, Kb>,
) -> Result<Option<(NodeOutputId, Kb)>> {
    let kind = *fg.graph.node_kind(node_id);

    // Find the first integer value output.
    let Some(out) = fg
        .graph
        .node_outputs(node_id)
        .into_iter()
        .find(|&o| fg.graph.output_kind(o).is_integer())
    else {
        return Ok(None);
    };
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value_or_err()?;
    // KnownBits tracks 64-bit masks only; types wider than U64 (U128/U256,
    // produced by some x86 SIMD / misc. lifted ops) fall outside this pass.
    let Some(type_mask) = u64_type_mask(ty) else {
        return Ok(None);
    };

    let kb = match kind {
        NodeKind::IntConst(v) => Kb::from_const(v, ty),

        NodeKind::IntBinaryOp(op) => {
            let [lhs, rhs] = fg.graph.node_inputs_exact::<2>(node_id)?;
            let l = known.get(&lhs).copied().unwrap_or_default();
            let r = known.get(&rhs).copied().unwrap_or_default();
            match op {
                IntBinaryOp::And => Kb {
                    ones: l.ones & r.ones,
                    zeros: (l.zeros | r.zeros) & type_mask,
                },
                IntBinaryOp::Or => Kb {
                    ones: (l.ones | r.ones) & type_mask,
                    zeros: l.zeros & r.zeros,
                },
                IntBinaryOp::Xor => Kb {
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
                    let rhs_mask = fg
                        .graph
                        .output_kind(rhs)
                        .as_value()
                        .and_then(u64_type_mask)
                        .unwrap_or(u64::MAX);
                    let rhs_kb = known.get(&rhs).copied().unwrap_or_default();
                    if rhs_kb.all_known(rhs_mask) {
                        let bit_width = ty.bit_width() as u64;
                        if rhs_kb.ones >= bit_width {
                            return Ok(Some((
                                out,
                                Kb {
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
                            Kb {
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
                    let rhs_mask = fg
                        .graph
                        .output_kind(rhs)
                        .as_value()
                        .and_then(u64_type_mask)
                        .unwrap_or(u64::MAX);
                    let rhs_kb = known.get(&rhs).copied().unwrap_or_default();
                    if rhs_kb.all_known(rhs_mask) {
                        let bit_width = ty.bit_width() as u64;
                        if rhs_kb.ones >= bit_width {
                            return Ok(Some((
                                out,
                                Kb {
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
                            Kb {
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

        NodeKind::IntUnaryOp(IntUnaryOp::Neg) => {
            // The IR's `IntUnaryOp::Neg` is *bitwise NOT* (Sleigh `IntNeg`),
            // not arithmetic negation — the name is counter-intuitive but
            // matches the Sleigh opcode mapping.  Bitwise NOT swaps known
            // ones and zeros.  (Two's complement negate — `IntUnaryOp::Not` —
            // has no closed-form known-bits propagation: it depends on the
            // borrow chain across the input's bits.)
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let kb = known.get(&input).copied().unwrap_or_default();
            Kb {
                ones: kb.zeros & type_mask,
                zeros: kb.ones & type_mask,
            }
        }

        NodeKind::Truncate => {
            // Upper bits of the source are discarded; lower bits are preserved.
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let kb = known.get(&input).copied().unwrap_or_default();
            Kb {
                ones: kb.ones & type_mask,
                zeros: kb.zeros & type_mask,
            }
        }

        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            // Upper bits are explicitly zeroed by the extension.
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let input_kind = fg.graph.output_kind(input);
            let input_ty = input_kind.as_value_or_err()?;
            let input_mask = u64_type_mask(input_ty).unwrap_or(0);
            let kb = known.get(&input).copied().unwrap_or_default();
            Kb {
                ones: kb.ones,
                zeros: kb.zeros | (type_mask ^ input_mask), // upper bits are 0
            }
        }

        NodeKind::Popcount | NodeKind::Lzcount => {
            // Result is in [0, bit_width(input)].  Bits above ceil_log2(bit_width+1) are zero.
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let input_kind = fg.graph.output_kind(input);
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
            Kb {
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
/// resulting [`Kb`] map keyed by [`NodeOutputId`].  Pure — does not mutate
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
/// information; treat them as the all-unknown default `Kb { ones: 0,
/// zeros: 0 }`.
///
/// # Errors
///
/// Returns an `Err` if a per-node Kb derivation fails — e.g. a node
/// whose recorded output type is wider than 64 bits combined with a
/// shape that requires `node_inputs_exact` to read a fixed input
/// arity.  In practice the only path to error is malformed IR;
/// well-formed graphs always converge.
pub fn analyze(function: &BuiltFunctionGraph) -> Result<FxHashMap<NodeOutputId, Kb>> {
    // Seed with every reachable node; consumers re-enqueue on input
    // change via `output_uses`.  `WorkSet` is the shared dedup-FIFO
    // worklist used by ConstantFold and DeadBranchElimination — no
    // local re-implementation.
    //
    // Detached "zombie" nodes (left behind by RedundantPhis,
    // DeadBranchElimination, etc.) are deliberately excluded:
    // `node_known_bits` calls `node_inputs_exact::<N>` which would
    // surface a hard error on a zero-input zombie.  Reachability is
    // the validator's existing scope-of-correctness boundary
    // (Layer A in `ir::validate`), so it's the right scope here too.
    let mut known: FxHashMap<NodeOutputId, Kb> = FxHashMap::default();
    let mut work = WorkSet::seeded(function.preorder());
    while let Some(node_id) = work.pop() {
        let Some((out, kb)) = node_known_bits(function, node_id, &known)? else {
            continue;
        };
        let merged = known.entry(out).or_default().merge(kb);
        if !merged {
            continue;
        }
        for (consumer, _idx) in function.graph.output_uses(out) {
            work.push(consumer);
        }
    }
    Ok(known)
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Propagates known-bit information and replaces outputs whose every bit is
/// determined with an equivalent integer constant.
///
/// Handles `IntConst`, `And`, `Or`, `Xor`, `Not`, `Truncate`, `ZeroExtend`,
/// and constant-shift nodes.  Runs a fixed-point inner loop to propagate
/// information along data-dependency chains before deciding replacements.
pub struct KnownBits;

impl OptimizerOnBuilt for KnownBits {
    fn optimize_built(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        // Phase 1 — propagate known bits to fixed point.  Read-only;
        // shared with the jump-table classifier (and any other caller
        // that needs bit-knowledge without graph rewrites).
        let known = analyze(function)?;

        // Phase 2 — replace fully-determined outputs with constants.
        // Snapshot reachable nodes so we can mutate the graph below
        // without holding a borrow on the preorder iterator.
        let nodes: Vec<_> = function.preorder().collect();
        let mut result = OptimizationResult::NoChange;
        // Reused across iterations: snapshot of `node_outputs` so the body can
        // call `replace_all_uses` (which mutates `function.graph`) without
        // holding a borrow into the graph's output slice.
        let mut outputs: Vec<NodeOutputId> = Vec::new();
        for &node_id in &nodes {
            outputs.clear();
            outputs.extend(function.graph.node_outputs(node_id));
            for &out in &outputs {
                let Some(ty) = function.graph.output_kind(out).as_value() else {
                    continue;
                };
                if !ty.is_integer() {
                    continue;
                }
                // Skip types KnownBits doesn't track (U128/U256).
                let Some(type_mask) = u64_type_mask(ty) else {
                    continue;
                };
                let Some(&kb) = known.get(&out) else { continue };
                if !kb.all_known(type_mask) {
                    continue;
                }
                // Skip nodes that are already constants (avoids busy-loop).
                if matches!(*function.graph.node_kind(node_id), NodeKind::IntConst(_)) {
                    continue;
                }
                let new_out = function.make_int_const(kb.ones, ty)?;
                result |= OptimizationResult::from_changed(function.replace_all_uses(out, new_out)?);
            }
        }
        Ok(result)
    }
}

