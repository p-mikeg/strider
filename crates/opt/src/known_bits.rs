use std::collections::HashMap;

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{BuiltFunctionGraph, ExtendOp, IntBinaryOp, IntUnaryOp};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

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
    known: &HashMap<NodeOutputId, Kb>,
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

        NodeKind::Extract { lsb, len } => {
            // Output is exactly `len` bits; upper bits of the output type are zero.
            let mask = if len >= 64 {
                u64::MAX
            } else {
                (1u64 << len) - 1
            };
            let upper_zeros = type_mask & !mask;
            // Propagate known bits from the input for the extracted window.
            let [input] = fg.graph.node_inputs_exact::<1>(node_id)?;
            let kb_in = known.get(&input).copied().unwrap_or_default();
            let shifted_ones = (kb_in.ones >> lsb) & mask;
            let shifted_zeros = (kb_in.zeros >> lsb) & mask;
            Kb {
                ones: shifted_ones,
                zeros: shifted_zeros | upper_zeros,
            }
        }

        NodeKind::Piece => {
            let [hi, lo] = fg.graph.node_inputs_exact::<2>(node_id)?;
            let lo_kind = fg.graph.output_kind(lo);
            let lo_ty = lo_kind.as_value_or_err()?;
            let lo_bits = lo_ty.bit_width() as u32;
            let lo_mask = lo_ty.get_unsigned_int(u64::MAX).unwrap_or(0);
            let hi_kb = known.get(&hi).copied().unwrap_or_default();
            let lo_kb = known.get(&lo).copied().unwrap_or_default();
            Kb {
                ones: ((hi_kb.ones << lo_bits) | (lo_kb.ones & lo_mask)) & type_mask,
                zeros: ((hi_kb.zeros << lo_bits) | (lo_kb.zeros & lo_mask)) & type_mask,
            }
        }

        NodeKind::Insert { lsb, len } => {
            let mask = if len >= 64 {
                u64::MAX
            } else {
                (1u64 << len) - 1
            };
            let [dest, src] = fg.graph.node_inputs_exact::<2>(node_id)?;
            let dest_kb = known.get(&dest).copied().unwrap_or_default();
            let src_kb = known.get(&src).copied().unwrap_or_default();
            Kb {
                ones: ((dest_kb.ones & !(mask << lsb)) | ((src_kb.ones & mask) << lsb)) & type_mask,
                zeros: ((dest_kb.zeros & !(mask << lsb)) | ((src_kb.zeros & mask) << lsb))
                    & type_mask,
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

        // ── Phase 1: propagate known bits to a fixed point ────────────────────
        let mut known: HashMap<NodeOutputId, Kb> = HashMap::new();
        let mut any_changed = true;
        while any_changed {
            any_changed = false;
            for &node_id in &nodes {
                if let Some((out, kb)) = node_known_bits(function, node_id, &known)? {
                    any_changed |= known.entry(out).or_default().merge(kb);
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use ir::node::{NodeKind, NodeOutputType};
    use ir::{FunctionBuilder, IntBinaryOp};

    fn make_fn<F>(f: F) -> Result<ir::BuiltFunctionGraph>
    where
        F: FnOnce(&mut FunctionBuilder) -> Result<ir::Value>,
    {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let val = f(&mut b)?;
        b.build_return(Some(val), &[])?;
        Ok(b.build()?)
    }

    fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .ok_or(ErrorKind::NoReturnNode)?;
        let val = fg.graph.node_inputs(ret)[1];
        Ok(*fg.graph.node_kind(fg.graph.get_node_from_output(val)))
    }

    /// `(x | 7) & 4` — bits 0-2 of `Or` are known 1; after And with 4 every
    /// bit is determined → should fold to `IntConst(4)`.
    #[test]
    fn known_bits_or_then_and() -> Result<()> {
        // Re-build without ConstantFold touching x.
        let mut fg2 = make_fn(|b| {
            let x_seed = b.build_int_const(0, NodeOutputType::U64); // value 0; bits 0-2 = 0
            let c7 = b.build_int_const(7, NodeOutputType::U64);
            let c4 = b.build_int_const(4, NodeOutputType::U64);
            let ored =
                b.build_int_binary_operation(x_seed, c7, IntBinaryOp::Or, NodeOutputType::U64)?;
            Ok(b.build_int_binary_operation(ored, c4, IntBinaryOp::And, NodeOutputType::U64)?)
        })?;
        // Run KnownBits until convergence.
        let mut changed = true;
        while changed {
            changed = KnownBits.optimize(&mut fg2)?.changed();
        }
        assert_eq!(return_kind(&fg2)?, NodeKind::IntConst(4));
        Ok(())
    }

    /// `(x & 0xF0) & 0x0F` — the two masks have no overlap, so the result is
    /// always 0.
    #[test]
    fn known_bits_and_mask_then_and() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xFF, NodeOutputType::U8); // any value
            let f0 = b.build_int_const(0xF0, NodeOutputType::U8);
            let f = b.build_int_const(0x0F, NodeOutputType::U8);
            let inner =
                b.build_int_binary_operation(x, f0, IntBinaryOp::And, NodeOutputType::U8)?;
            Ok(b.build_int_binary_operation(inner, f, IntBinaryOp::And, NodeOutputType::U8)?)
        })?;
        let mut changed = true;
        while changed {
            changed = KnownBits.optimize(&mut fg)?.changed();
        }
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
        Ok(())
    }

    /// A plain `IntConst` already has all bits known — the optimizer must not
    /// loop or report spurious changes.
    #[test]
    fn known_bits_const_no_change() -> Result<()> {
        let mut fg = make_fn(|b| Ok(b.build_int_const(42, NodeOutputType::U64)))?;
        // KnownBits should see the const node but not replace it with itself.
        assert!(!KnownBits.optimize(&mut fg)?.changed());
        Ok(())
    }

    // ── Extract / Popcount / Piece known-bits ─────────────────────────────────

    /// `extract(x, lsb=0, len=4)` into U8 → upper nibble is always zero.
    /// Therefore `and(result, 0xF0)` should fold to 0.
    #[test]
    fn known_bits_extract_upper_zero() -> Result<()> {
        let mut fg =
            make_fn(|b| {
                let x = b.build_int_const(0xFF, NodeOutputType::U8);
                let extracted = b.build_extract(x, 0, 4, NodeOutputType::U8)?;
                let mask = b.build_int_const(0xF0, NodeOutputType::U8);
                Ok(b.build_int_binary_operation(
                    extracted,
                    mask,
                    IntBinaryOp::And,
                    NodeOutputType::U8,
                )?)
            })?;
        let mut changed = true;
        while changed {
            changed = KnownBits.optimize(&mut fg)?.changed();
        }
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
        Ok(())
    }

    /// `popcount(U8)` fits in 4 bits (max = 8), so bits 4..7 are known zero.
    /// `and(popcount(x), 0xF0)` should fold to 0.
    #[test]
    fn known_bits_popcount_range() -> Result<()> {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xFF, NodeOutputType::U8);
            let pc = b.build_popcount(x, NodeOutputType::U8)?;
            let mask = b.build_int_const(0xF0, NodeOutputType::U8);
            Ok(b.build_int_binary_operation(pc, mask, IntBinaryOp::And, NodeOutputType::U8)?)
        })?;
        let mut changed = true;
        while changed {
            changed = KnownBits.optimize(&mut fg)?.changed();
        }
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
        Ok(())
    }

    /// `piece(IntConst(0xAB), IntConst(0xCD))` → all bits are fully determined
    /// → KnownBits resolves it to `IntConst(0xABCD)`.
    #[test]
    fn known_bits_piece_propagation() -> Result<()> {
        let mut fg = make_fn(|b| {
            let hi = b.build_int_const(0xAB, NodeOutputType::U8);
            let lo = b.build_int_const(0xCD, NodeOutputType::U8);
            Ok(b.build_piece(hi, lo, NodeOutputType::U16)?)
        })?;
        let mut changed = true;
        while changed {
            changed = KnownBits.optimize(&mut fg)?.changed();
        }
        assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xABCD));
        Ok(())
    }
}
