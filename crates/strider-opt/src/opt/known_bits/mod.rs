use cranelift_entity::SecondaryMap;
use entity_utils::{DenseEntitySet, Worklist};

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{ExtendOp, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Unrecorded outputs read back as `KnownBitsFacts::default()` (`{0, 0}` =
/// "no info").
pub(crate) type KnownBitsMap = SecondaryMap<ValueId, KnownBitsFacts>;

/// The all-ones mask for `ty`, or `None` for floats and for integers wider
/// than the `u128` lattice (`I256` / `I512`).
pub(crate) fn type_mask_u128(ty: ValueType) -> Option<u128> {
    if !ty.is_integer() || ty.bit_width() > 128 {
        return None;
    }
    Some(ty.bit_mask_u128())
}

/// Known-bit lattice for one output.  `ones` and `zeros` are masked to the
/// output type's width and must never overlap: `ones & zeros == 0`, and a bit
/// in neither set is unknown.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct KnownBitsFacts {
    pub(crate) ones: u128,
    pub(crate) zeros: u128,
}

impl KnownBitsFacts {
    /// `None` for untracked types, meaning fully unknown, not a known zero.
    fn from_const(val: u128, ty: ValueType) -> Option<Self> {
        let type_mask = type_mask_u128(ty)?;
        let masked = ty.get_unsigned_int(val)?;
        Some(KnownBitsFacts {
            ones: masked,
            zeros: type_mask ^ masked,
        })
    }

    fn all_known(self, type_mask: u128) -> bool {
        (self.ones | self.zeros) & type_mask == type_mask
    }

    /// Upper bound on the runtime value: every bit that could be 1, set.
    pub fn max_value(self, type_mask: u128) -> u128 {
        (!self.zeros) & type_mask
    }
}

enum ConstShift {
    InRange(u32),
    /// Shift amount known but at-or-past the output width; Sleigh returns 0,
    /// so every output bit is known zero.
    OverWidth,
    Unknown,
}

/// Mirrors Sleigh's `OpBehaviorInt{Left,Right}::evaluateBinary`, which return 0
/// for any shift amount `>= bit_width`.  Masking the amount to `bit_width - 1`
/// instead would wrap large literal shifts back into range.
fn classify_const_shift(rhs_kb: KnownBitsFacts, rhs_mask: u128, bit_width: u64) -> ConstShift {
    if !rhs_kb.all_known(rhs_mask) {
        return ConstShift::Unknown;
    }
    if rhs_kb.ones >= u128::from(bit_width) {
        return ConstShift::OverWidth;
    }
    ConstShift::InRange(rhs_kb.ones as u32)
}

#[derive(Clone, Copy)]
enum ShiftDir {
    Left,
    Right,
}

/// Transfer for both shift arms.  Vacated bits become known-zero; surviving
/// lhs bits carry their ones/zeros along.  `None` when the shift amount is
/// unknown, meaning fully unknown.
fn shift_known_bits(
    function: &strider_ir::Function,
    l: KnownBitsFacts,
    rhs: ValueId,
    rhs_kb: KnownBitsFacts,
    ty: ValueType,
    dir: ShiftDir,
) -> Option<KnownBitsFacts> {
    // Callers already gated on `type_mask_u128(ty)`; this re-derivation never
    // fails.
    let type_mask = type_mask_u128(ty)?;
    let rhs_mask = function
        .value_type_opt(rhs)
        .and_then(type_mask_u128)
        .unwrap_or(u128::MAX);
    match classify_const_shift(rhs_kb, rhs_mask, ty.bit_width() as u64) {
        ConstShift::Unknown => None,
        ConstShift::OverWidth => Some(KnownBitsFacts {
            ones: 0,
            zeros: type_mask,
        }),
        ConstShift::InRange(shift) => {
            let (shifted_ones, shifted_zeros) = match dir {
                ShiftDir::Left => {
                    let lower_mask = (1u128 << shift).wrapping_sub(1) & type_mask;
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

/// Known bits of `node_id`'s first integer value output.  `None` if it has no
/// such output, or nothing can be proven about it.
pub(crate) fn node_known_bits(
    function: &strider_ir::Function,
    node_id: NodeId,
    known: &KnownBitsMap,
) -> Result<Option<(ValueId, KnownBitsFacts)>> {
    let kind = *function.node_kind(node_id);

    let Some(&out) = function
        .node_outputs(node_id)
        .iter()
        .find(|&&o| function.value_kind(o).is_integer())
    else {
        return Ok(None);
    };
    let out_kind = function.value_kind(out);
    let ty = out_kind.as_value_or_err()?;
    let Some(type_mask) = type_mask_u128(ty) else {
        return Ok(None);
    };

    let kb = match kind {
        NodeKind::IntConst(_) => match function
            .int_const_u128(out)
            .and_then(|v| KnownBitsFacts::from_const(v, ty))
        {
            Some(kb) => kb,
            None => return Ok(None),
        },

        NodeKind::IntBinaryOp(op) => {
            let [lhs, rhs] = function
                .graph()
                .node_inputs_exact::<2>(node_id)
                .expect("IntBinaryOp has 2 inputs per node signature");
            let l = known[lhs];
            let r = known[rhs];
            match op {
                // Every arm masks to `type_mask`: an operand may legally be
                // wider than this node's output, and its above-width known bits
                // must not leak into these facts.
                IntBinaryOp::And => KnownBitsFacts {
                    ones: l.ones & r.ones & type_mask,
                    zeros: (l.zeros | r.zeros) & type_mask,
                },
                IntBinaryOp::Or => KnownBitsFacts {
                    ones: (l.ones | r.ones) & type_mask,
                    zeros: l.zeros & r.zeros & type_mask,
                },
                IntBinaryOp::Xor => KnownBitsFacts {
                    ones: ((l.ones & r.zeros) | (l.zeros & r.ones)) & type_mask,
                    zeros: ((l.ones & r.ones) | (l.zeros & r.zeros)) & type_mask,
                },
                IntBinaryOp::ShiftLeft => {
                    return Ok(shift_known_bits(function, l, rhs, r, ty, ShiftDir::Left)
                        .map(|facts| (out, facts)));
                }
                IntBinaryOp::ShiftRight => {
                    return Ok(shift_known_bits(function, l, rhs, r, ty, ShiftDir::Right)
                        .map(|facts| (out, facts)));
                }
                _ => return Ok(None),
            }
        }

        NodeKind::Truncate => {
            let [value] = function
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
            let [value] = function
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Extend has 1 input per node signature");
            let input_kind = function.value_kind(value);
            let input_ty = input_kind.as_value_or_err()?;
            // An untracked input width must bail, not default `input_mask` to
            // 0: that would mark every bit known-zero.
            let Some(input_mask) = type_mask_u128(input_ty) else {
                return Ok(None);
            };
            let kb = known[value];
            KnownBitsFacts {
                ones: kb.ones,
                zeros: kb.zeros | (type_mask ^ input_mask), // upper bits are 0
            }
        }

        NodeKind::Extend(ExtendOp::SignExtend) => {
            // Upper bits replicate the sign bit, so they are determined only
            // when the sign bit is; the lower bits pass through either way.
            let [value] = function
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Extend has 1 input per node signature");
            let input_kind = function.value_kind(value);
            let input_ty = input_kind.as_value_or_err()?;
            let Some(input_mask) = type_mask_u128(input_ty) else {
                return Ok(None);
            };
            let kb = known[value];
            // Highest bit of the input width.
            let sign_bit = (input_mask >> 1) + 1;
            let upper_mask = type_mask & !input_mask;
            if kb.ones & sign_bit != 0 {
                KnownBitsFacts {
                    ones: (kb.ones & input_mask) | upper_mask,
                    zeros: kb.zeros & input_mask,
                }
            } else if kb.zeros & sign_bit != 0 {
                KnownBitsFacts {
                    ones: kb.ones & input_mask,
                    zeros: (kb.zeros & input_mask) | upper_mask,
                }
            } else {
                KnownBitsFacts {
                    ones: kb.ones & input_mask,
                    zeros: kb.zeros & input_mask,
                }
            }
        }

        NodeKind::Popcount | NodeKind::Lzcount => {
            // Result is in [0, bit_width(input)], so every bit above
            // ceil_log2(bit_width + 1) is known zero.
            let [value] = function
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Popcount / Lzcount have 1 input per node signature");
            let input_kind = function.value_kind(value);
            let input_ty = input_kind.as_value_or_err()?;
            // `8 * byte_size`, the width Sleigh counts over
            // (`opbehavior.cc:791`); only `I1` differs from `bit_width`.
            let max_val = (input_ty.byte_size() * 8) as u64;
            let bits_needed = u64::from(u64::BITS - max_val.leading_zeros());
            let result_mask = if bits_needed >= 128 {
                u128::MAX
            } else {
                (1u128 << bits_needed) - 1
            };
            let upper_zeros = type_mask & !result_mask;
            KnownBitsFacts {
                ones: 0,
                zeros: upper_zeros,
            }
        }

        _ => return Ok(None),
    };

    // Overlapping ones/zeros can only mean a transfer-function bug.
    debug_assert_eq!(
        kb.ones & kb.zeros,
        0,
        "node_known_bits produced overlapping ones/zeros: {kb:?}",
    );

    Ok(Some((out, kb)))
}

#[cfg(test)]
thread_local! {
    /// Cone members touched while transferring a fold's contributor
    /// fingerprints.
    pub(crate) static CONE_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_cone_steps() {
    CONE_STEPS.with(|c| c.set(c.get() + 1));
}

#[cfg(not(test))]
fn bump_cone_steps() {}

/// Kinds whose input edges are followed when building a fold's fingerprint
/// cone.  `Popcount` / `Lzcount` are included conservatively; their transfer
/// reads only the input width.
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

/// Known-bits worklist analysis to fixed point.  Non-mutating.  Outputs absent
/// from the map have no proven bits.
///
/// # Errors
///
/// Only a malformed per-node derivation errors.  Wrong input arity panics
/// instead (validated invariant); well-formed graphs always converge.
pub fn analyze(function: &strider_ir::Function) -> Result<KnownBitsMap> {
    // Reachable nodes only: a detached zombie can have zero inputs, which would
    // trip `node_inputs_exact` inside `node_known_bits`.  RPO seed order reduces
    // churn; the monotone fixpoint converges from any order.
    let mut known: KnownBitsMap = SecondaryMap::new();
    let mut work: Worklist<NodeId> = function.reverse_postorder_filter(|_| true).collect();
    while let Some(node_id) = work.dequeue() {
        let Some((out, kb)) = node_known_bits(function, node_id, &known)? else {
            continue;
        };
        // `kb` is recomputed from scratch each visit and is monotonically more
        // precise than what is stored, so overwrite rather than union.
        if known[out] == kb {
            continue;
        }
        known[out] = kb;
        for (consumer, _idx) in function.value_uses(out) {
            work.enqueue(consumer);
        }
    }
    Ok(known)
}

/// Propagates known-bit information and replaces outputs whose every bit is
/// determined with an equivalent integer constant.
#[derive(Clone)]
pub struct KnownBits;

impl Optimizer for KnownBits {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::OptCtx<'_>,
    ) -> crate::Result<OptimizationResult> {
        let known = analyze(edit.function())?;

        // Collect first so the mutating loop below can take `&mut edit`.
        let to_fold: Vec<(ValueId, ValueType, u128)> = known
            .iter()
            .filter_map(|(value, &kb)| {
                let ty = edit.value_type_opt(value)?;
                if !ty.is_integer() {
                    return None;
                }
                let type_mask = type_mask_u128(ty)?;
                if !kb.all_known(type_mask) {
                    return None;
                }
                // Folding an existing IntConst would be a no-op.
                let producer = edit.producer(value);
                if matches!(*edit.node_kind(producer), NodeKind::IntConst(_)) {
                    return None;
                }
                Some((value, ty, kb.ones))
            })
            .collect();

        // The cone is about to be cascade-culled with its asm-fingerprints.  A
        // contributor that establishes bits without being fully known itself
        // (the `x & 1` in `((x & 1) | 2) & 0`) never folds, so a one-hop absorb
        // of the direct inputs loses it.
        //
        // Constants first, absorb second, rewire last: the cone walk reads
        // input edges and `replace_value` rewires them, so every walk must run
        // over the pre-fold edges.  Creating a constant only adds a node (or
        // dedups onto an existing one) and rewires nothing.  No `IntConst` is
        // ever a fold's old producer, so the dedup pool is stable across the
        // rewire pass either way.
        let mut folds: Vec<(ValueId, ValueId)> = Vec::with_capacity(to_fold.len());
        for &(value, ty, ones) in &to_fold {
            folds.push((value, edit.build_int_const(ones, ty)?));
        }

        // A fingerprint union LINKS rather than copies, so a node's set already
        // covers everything unioned into it. Linking each cone node to its own
        // inputs ONCE therefore makes a fold's whole cone reachable from its
        // producer, and the fold itself one union.
        //
        // A union with a set-less `src` is a no-op, so a node is linked to its
        // inputs only once each has absorbed its own subtree: the `true` flag
        // marks that second visit, and the marker sits below everything the
        // first visit pushes.  The cone kinds cannot form a value cycle (only
        // a `Phi` closes one, and it does not propagate), so the second visit
        // always comes after every descendant's.
        //
        // `linked` is never cleared, so each edge is walked once across all
        // folds; nested folds (`t(i) = t(i-1) | C(i)`) would otherwise be
        // quadratic.
        let mut linked: DenseEntitySet<NodeId> = DenseEntitySet::new();
        let mut stack: Vec<(NodeId, bool)> = Vec::new();
        let mut inputs: Vec<NodeId> = Vec::new();
        for &(value, new_value) in &folds {
            let new_producer = edit.producer(new_value);
            let old_producer = edit.producer(value);
            stack.clear();
            stack.push((old_producer, false));
            while let Some((n, subtrees_absorbed)) = stack.pop() {
                if subtrees_absorbed {
                    inputs.clear();
                    inputs.extend(crate::peephole::input_producers_iter(edit, n));
                    for &input in &inputs {
                        edit.function_mut()
                            .side_tables_mut()
                            .extend_asm_fingerprint_from(n, input);
                    }
                    continue;
                }
                if !linked.insert(n) {
                    continue;
                }
                bump_cone_steps();
                if !propagates_known_bits(edit.node_kind(n)) {
                    continue;
                }
                stack.push((n, true));
                stack.extend(crate::peephole::input_producers_iter(edit, n).map(|i| (i, false)));
            }
            // The producer is absorbed by `replace_value` below, but its own
            // fingerprint is not: the cone hangs off it, so link it here.
            edit.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint_from(new_producer, old_producer);
        }

        let mut result = OptimizationResult::NoChange;
        for (value, new_value) in folds {
            if edit.replace_value(value, new_value)? {
                result = OptimizationResult::Changed;
            }
        }
        Ok(result)
    }
}
