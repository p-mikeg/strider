use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{BuiltFunctionGraph, ExtendOp, IntBinaryOp, IntUnaryOp};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

// ── Known-bits representation ─────────────────────────────────────────────────

/// Known-bit information for a single output.
///
/// Both `ones` and `zeros` are masked to the output type's width and must
/// never overlap (`ones & zeros == 0`).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Kb {
    /// Bits that are definitely 1.
    ones: u64,
    /// Bits that are definitely 0.
    zeros: u64,
}

impl Kb {
    fn from_const(val: u64, ty: NodeOutputType) -> Self {
        let masked = ty.get_unsigned_int(val).unwrap_or(0);
        let type_mask = ty.get_unsigned_int(u64::MAX).unwrap_or(0);
        Kb {
            ones: masked,
            zeros: type_mask ^ masked,
        }
    }

    /// Returns `true` if merging `other` into `self` changed anything.
    fn merge(&mut self, other: Kb) -> bool {
        let new_ones = self.ones | other.ones;
        let new_zeros = self.zeros | other.zeros;
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
}

// ── Per-node known-bits computation ───────────────────────────────────────────

/// Computes the known bits contributed by `node_id` toward its single integer
/// value output.  Returns `(output_id, Kb)` or `None` if the node has no
/// integer value output or no useful information can be extracted.
fn node_known_bits(
    fg: &BuiltFunctionGraph,
    node_id: NodeId,
    known: &FxHashMap<NodeOutputId, Kb>,
) -> Result<Option<(NodeOutputId, Kb)>> {
    let kind = *fg.graph.node_kind(node_id);

    // Find the first integer value output.
    let out = match fg
        .graph
        .node_outputs(node_id)
        .into_iter()
        .find(|&o| fg.graph.output_kind(o).is_integer())
    {
        Some(o) => o,
        None => return Ok(None),
    };
    let out_kind = fg.graph.output_kind(out);
    let ty = out_kind.as_value_or_err()?;
    // KnownBits tracks 64-bit masks only; types wider than U64 (U128/U256,
    // produced by some x86 SIMD / misc. lifted ops) fall outside this pass.
    let Some(type_mask) = ty.get_unsigned_int(u64::MAX) else {
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
                    // Lower bits of a left-shifted value are known zero.
                    let rhs_mask = fg
                        .graph
                        .output_kind(rhs)
                        .as_value()
                        .and_then(|t| t.get_unsigned_int(u64::MAX))
                        .unwrap_or(u64::MAX);
                    let rhs_kb = known.get(&rhs).copied().unwrap_or_default();
                    if rhs_kb.all_known(rhs_mask) {
                        let shift = (rhs_kb.ones & (ty.bit_width() as u64 - 1)) as u32;
                        let lower_mask = (1u64 << shift).wrapping_sub(1) & type_mask;
                        return Ok(Some((
                            out,
                            Kb {
                                ones: 0,
                                zeros: lower_mask,
                            },
                        )));
                    }
                    return Ok(None);
                }
                IntBinaryOp::ShiftRight => {
                    // Logical right-shift: upper bits become 0.
                    let rhs_mask = fg
                        .graph
                        .output_kind(rhs)
                        .as_value()
                        .and_then(|t| t.get_unsigned_int(u64::MAX))
                        .unwrap_or(u64::MAX);
                    let rhs_kb = known.get(&rhs).copied().unwrap_or_default();
                    if rhs_kb.all_known(rhs_mask) {
                        let shift = (rhs_kb.ones & (ty.bit_width() as u64 - 1)) as u32;
                        let upper_mask = !((type_mask) >> shift) & type_mask;
                        return Ok(Some((
                            out,
                            Kb {
                                ones: 0,
                                zeros: upper_mask,
                            },
                        )));
                    }
                    return Ok(None);
                }
                _ => return Ok(None),
            }
        }

        NodeKind::IntUnaryOp(IntUnaryOp::Not) => {
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let kb = known.get(&input).copied().unwrap_or_default();
            // NOT swaps known ones and zeros.
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
            let input_mask = input_ty.get_unsigned_int(u64::MAX).unwrap_or(0);
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

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Propagates known-bit information and replaces outputs whose every bit is
/// determined with an equivalent integer constant.
///
/// Handles `IntConst`, `And`, `Or`, `Xor`, `Not`, `Truncate`, `ZeroExtend`,
/// and constant-shift nodes.  Runs a fixed-point inner loop to propagate
/// information along data-dependency chains before deciding replacements.
pub struct KnownBits;

impl Optimizer for KnownBits {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        // Collect once; mutations only add new nodes (never change existing ones).
        let nodes: Vec<_> = function.preorder().collect();

        // ── Phase 1: propagate known bits via worklist ────────────────────────
        // Re-evaluate a node only when one of its inputs' Kb just changed.
        let mut known: FxHashMap<NodeOutputId, Kb> = FxHashMap::default();
        let mut queued: FxHashSet<NodeId> = nodes.iter().copied().collect();
        let mut work: VecDeque<NodeId> = nodes.iter().copied().collect();
        while let Some(node_id) = work.pop_front() {
            queued.remove(&node_id);
            let Some((out, kb)) = node_known_bits(function, node_id, &known)? else {
                continue;
            };
            let merged = known.entry(out).or_default().merge(kb);
            if !merged {
                continue;
            }
            // Re-queue every consumer of `out`.
            for (consumer, _idx) in function.graph.output_uses(out) {
                if queued.insert(consumer) {
                    work.push_back(consumer);
                }
            }
        }

        // ── Phase 2: replace fully-determined outputs with constants ──────────
        let mut result = OptimizationResult::NoChange;
        for &node_id in &nodes {
            let outputs: Vec<_> = function.graph.node_outputs(node_id).into_iter().collect();
            for out in outputs {
                let Some(ty) = function.graph.output_kind(out).as_value() else {
                    continue;
                };
                if !ty.is_integer() {
                    continue;
                }
                // Skip types KnownBits doesn't track (U128/U256).
                let Some(type_mask) = ty.get_unsigned_int(u64::MAX) else {
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

