use cranelift_entity::{EntityRef, SecondaryMap};
use entity_utils::Worklist;

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{ExtendOp, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Unrecorded outputs read back as `KnownBitsFacts::default()` (`{0, 0}` =
/// "no info").  A `SecondaryMap` rather than a hash map: this is the hottest
/// probe in the pass.
pub(crate) type KnownBitsMap = SecondaryMap<ValueId, KnownBitsFacts>;

/// `None` for floats and for the integers too wide for the `u128` lattice
/// (`I256` / `I512`).  Everything up to `I128` is tracked, including `I1`.
pub(crate) fn type_mask_u128(ty: ValueType) -> Option<u128> {
    if !ty.is_integer() || ty.bit_width() > 128 {
        return None;
    }
    Some(ty.bit_mask_u128())
}

/// Known-bit lattice for one output.  `ones` and `zeros` are masked to the
/// output type's width and must never overlap: `ones & zeros == 0`, and a bit
/// in neither set is unknown.
///
/// The fields are `pub(crate)` because nothing enforces disjointness on a
/// struct literal; only `from_const`, `default`, and the transfer function
/// preserve it.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct KnownBitsFacts {
    pub(crate) ones: u128,
    pub(crate) zeros: u128,
}

impl KnownBitsFacts {
    /// `None` for untracked types; the caller must treat that as fully
    /// unknown, not as a deliberate zero.
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
/// for any shift amount `>= bit_width`.  Masking the amount to
/// `bit_width - 1` instead would wrap large literal shifts back into range and
/// produce wrong known bits.
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
/// lhs bits carry their ones/zeros along.  `None` means the shift amount is
/// unknown, so the caller must fall back to fully unknown.
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
                    // Known 1 iff exactly one input is known 1; known 0 iff
                    // both inputs are known and agree.
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

        // There is no unary-complement arm: `~x` is `Xor(x, all_ones)`, and the
        // Xor arm swaps ones/zeros correctly against a fully-known all-ones
        // operand.  `IntUnaryOp::Neg` has no closed-form transfer (it depends on
        // the borrow chain), so it falls through to unknown.
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
            let max_val = input_ty.bit_width() as u64;
            // `.max(1)` only matters for `max_val == 0`, where the subtraction
            // yields 0.
            let bits_needed = u64::from((u64::BITS - max_val.leading_zeros()).max(1));
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

    // Overlapping ones/zeros can only mean a transfer-function bug; catch it at
    // the point of origin instead of letting it propagate.
    debug_assert_eq!(
        kb.ones & kb.zeros,
        0,
        "node_known_bits produced overlapping ones/zeros: {kb:?}",
    );

    Ok(Some((out, kb)))
}

/// Kinds whose `node_known_bits` arm reads `known[input]`, i.e. the edges along
/// which known-bits provenance flows.  The fold-time fingerprint walk recurses
/// through these and stops everywhere else, so an opaque kind (`Load` / `Phi` /
/// `Call`) is a leaf and its address / memory / control cone is never tainted.
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

/// Known-bits worklist analysis to fixed point.  Non-mutating, so other passes
/// (e.g. the jump-table classifier) can call it for a bit-knowledge query
/// without running the rewriting [`KnownBits`] pass.
///
/// Outputs absent from the map have no proven bits; treat them as the
/// all-unknown default.
///
/// # Errors
///
/// Only a malformed per-node derivation errors.  Wrong input arity panics
/// instead (validated invariant); well-formed graphs always converge.
pub fn analyze(function: &strider_ir::Function) -> Result<KnownBitsMap> {
    // Seeded from reachable nodes only: detached zombies (left by PhiCollapse,
    // DeadBranchElimination, ...) can have zero inputs, which would trip
    // `node_inputs_exact` inside `node_known_bits`.  Reachability is also the
    // validator's scope, so the two agree.
    //
    // RPO seed order (operands before consumers) is just churn reduction; the
    // monotone fixpoint converges to the same map from any order.
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

        // The fixpoint is already done, so folding is a flat per-output
        // decision: no second worklist, no consumer re-enqueue, order
        // irrelevant.  `SecondaryMap::iter` covers defaulted entries too, but
        // `all_known` rejects `{0, 0}` against any non-zero mask.
        //
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

        // A fold's contributor cone is about to be cascade-culled, taking its
        // asm-fingerprints (the proof of why the result is constant) with it.
        // A one-hop absorb of the direct inputs is not enough: a contributor
        // that establishes bits without being fully known itself (the `x & 1`
        // in `((x & 1) | 2) & 0`) never folds, so the fixpoint can never carry
        // its fingerprint upward.  Absorb the whole cone instead, recursing
        // only through `propagates_known_bits` kinds.
        //
        // Folds share cones, so per-fold walking would be O(folds*cone); memoize
        // once over the pre-fold graph, before `replace_value` starts culling.
        let cone_nodes = build_cone_node_memo(edit, &to_fold);

        let mut result = OptimizationResult::NoChange;
        for (value, ty, ones) in to_fold {
            let new_value = edit.build_int_const(ones, ty)?;
            let folded_producer = edit.producer(value);
            let new_producer = edit.producer(new_value);
            // Seed from the producer's input cones; the producer itself is
            // absorbed by `replace_value` below.
            for p in crate::peephole::input_producers(edit, folded_producer) {
                if let Some(cone) = cone_nodes.get(&p) {
                    for &q in cone {
                        edit.function_mut()
                            .side_tables_mut()
                            .extend_asm_fingerprint_from(new_producer, q);
                    }
                }
            }
            if edit.replace_value(value, new_value)? {
                result = OptimizationResult::Changed;
            }
        }
        Ok(result)
    }
}

/// Deduplicated contributor node ids in one node's known-bits cone.
type ConeNodes = smallvec::SmallVec<[NodeId; 8]>;

/// Memoizes `cone(n) = {n} ∪ (propagates(n) ? ⋃ₚ cone(p) : ∅)` over `p` in
/// `n`'s input producers, for the cones reachable from the `to_fold`
/// producers' inputs.
///
/// Reads only node ids and kinds, never a fingerprint, so the caller can turn
/// each cone node into an O(1) `extend_asm_fingerprint_from` link.  The cone is
/// acyclic (the propagating kinds exclude Phi and Region, the only sources of
/// data cycles), so the iterative postorder needs no cycle handling.
fn build_cone_node_memo(
    edit: &crate::EditFunction<'_>,
    to_fold: &[(ValueId, ValueType, u128)],
) -> rustc_hash::FxHashMap<NodeId, ConeNodes> {
    let mut memo: rustc_hash::FxHashMap<NodeId, ConeNodes> = rustc_hash::FxHashMap::default();
    // Iterative postorder: the bool is "children already pushed".  On the
    // second pop every child's set is in `memo`.
    let mut stack: Vec<(NodeId, bool)> = Vec::new();
    let seed_inputs = to_fold.iter().flat_map(|&(value, _, _)| {
        crate::peephole::input_producers_iter(edit, edit.producer(value))
    });
    for n in seed_inputs {
        stack.push((n, false));
    }
    while let Some((n, expanded)) = stack.pop() {
        if expanded {
            let mut nodes: ConeNodes = smallvec::smallvec![n];
            if propagates_known_bits(edit.node_kind(n)) {
                for p in crate::peephole::input_producers_iter(edit, n) {
                    if let Some(child) = memo.get(&p) {
                        nodes.extend_from_slice(child);
                    }
                }
            }
            nodes.sort_unstable_by_key(|id| id.index());
            nodes.dedup();
            memo.insert(n, nodes);
            continue;
        }
        if memo.contains_key(&n) {
            continue;
        }
        stack.push((n, true));
        if propagates_known_bits(edit.node_kind(n)) {
            for p in crate::peephole::input_producers_iter(edit, n) {
                if !memo.contains_key(&p) {
                    stack.push((p, false));
                }
            }
        }
    }
    memo
}
