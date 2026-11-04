use cranelift_entity::SecondaryMap;
use entity_utils::{DenseEntitySet, Worklist};
use rustc_hash::FxHashMap;

use strider_ir::eval::mask_below;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{ExtendOp, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};

use crate::const_eval::var_and_const;
use crate::pipeline::Optimizer;

#[cfg(test)]
mod tests;

/// Known bits per output; an unrecorded output reads as "no info".
pub type KnownBitsMap = SecondaryMap<ValueId, KnownBitsFacts>;

/// Known-bit lattice for one output. `ones` and `zeros` are masked to the
/// output width and never overlap; a bit in neither set is unknown.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct KnownBitsFacts {
    pub(crate) ones: u128,
    pub(crate) zeros: u128,
}

impl KnownBitsFacts {
    /// `None` for untracked types, meaning fully unknown, not a known zero.
    fn from_const(val: u128, ty: ValueType) -> Option<Self> {
        let type_mask = ty.int_mask_u128()?;
        let masked = val & type_mask;
        Some(KnownBitsFacts {
            ones: masked,
            zeros: type_mask ^ masked,
        })
    }

    fn all_known(self, type_mask: u128) -> bool {
        (self.ones | self.zeros) & type_mask == type_mask
    }

    /// Upper bound on the runtime value: every bit that could be 1, set.
    pub(crate) fn max_value(self, type_mask: u128) -> u128 {
        (!self.zeros) & type_mask
    }
}

#[derive(Clone, Copy)]
enum ShiftDir {
    Left,
    Right,
}

/// Transfer for both shifts: vacated bits become known-zero. `None` when the
/// shift amount is unknown. A shift `>= bit_width` gives 0, as in Sleigh.
fn shift_known_bits(
    function: &strider_ir::Function,
    l: KnownBitsFacts,
    rhs: ValueId,
    rhs_kb: KnownBitsFacts,
    ty: ValueType,
    dir: ShiftDir,
) -> Option<KnownBitsFacts> {
    let type_mask = ty.int_mask_u128()?;
    let rhs_mask = function
        .value_type_opt(rhs)
        .and_then(ValueType::int_mask_u128)
        .unwrap_or(u128::MAX);
    if !rhs_kb.all_known(rhs_mask) {
        return None;
    }
    let shift = u32::try_from(rhs_kb.ones).unwrap_or(u32::MAX);
    let (ones, zeros) = match dir {
        ShiftDir::Left => (
            l.ones.checked_shl(shift).unwrap_or(0),
            l.zeros.checked_shl(shift).unwrap_or(0) | mask_below(shift),
        ),
        ShiftDir::Right => (
            (l.ones & type_mask).checked_shr(shift).unwrap_or(0),
            (l.zeros & type_mask).checked_shr(shift).unwrap_or(0)
                | !type_mask.checked_shr(shift).unwrap_or(0),
        ),
    };
    Some(KnownBitsFacts {
        ones: ones & type_mask,
        zeros: zeros & type_mask & !ones,
    })
}

/// Operand facts from interned constants alone, for a one-hop
/// [`node_known_bits`] caller.
pub(crate) fn const_only(
    function: &strider_ir::Function,
) -> impl Fn(ValueId) -> KnownBitsFacts + '_ {
    move |v| {
        function
            .int_const_u128(v)
            .zip(function.value_type_opt(v))
            .and_then(|(k, ty)| KnownBitsFacts::from_const(k, ty))
            .unwrap_or_default()
    }
}

/// Known bits of `node_id`'s first integer value output, reading operand
/// facts through `operand`. `None` if there is no such output or nothing is
/// proven.
pub(crate) fn node_known_bits(
    function: &strider_ir::Function,
    node_id: NodeId,
    operand: &mut dyn FnMut(ValueId) -> KnownBitsFacts,
) -> Option<(ValueId, KnownBitsFacts)> {
    let kind = *function.node_kind(node_id);

    let (out, ty) = function
        .node_outputs(node_id)
        .iter()
        .find_map(|&o| Some((o, function.value_type_opt(o).filter(|t| t.is_integer())?)))?;
    let type_mask = ty.int_mask_u128()?;

    let kb = match kind {
        NodeKind::IntConst(_) => KnownBitsFacts::from_const(function.int_const_u128(out)?, ty)?,

        NodeKind::IntBinaryOp(op) => {
            let [lhs, rhs] = function
                .graph()
                .node_inputs_exact::<2>(node_id)
                .expect("IntBinaryOp has 2 inputs per node signature");
            let l = operand(lhs);
            let r = operand(rhs);
            match op {
                // Masked defensively: the graph is not re-validated
                // mid-pipeline.
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
                    shift_known_bits(function, l, rhs, r, ty, ShiftDir::Left)?
                }
                IntBinaryOp::ShiftRight => {
                    shift_known_bits(function, l, rhs, r, ty, ShiftDir::Right)?
                }
                _ => return None,
            }
        }

        NodeKind::Truncate | NodeKind::Extend(_) | NodeKind::Popcount | NodeKind::Lzcount => {
            let [value] = function
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("a unary kind has 1 input per node signature");
            let kb = operand(value);
            let input_ty = function.value_type_opt(value)?;
            match kind {
                NodeKind::Truncate => KnownBitsFacts {
                    ones: kb.ones & type_mask,
                    zeros: kb.zeros & type_mask,
                },
                NodeKind::Extend(op) => {
                    let input_mask = input_ty.int_mask_u128()?;
                    let upper_mask = type_mask & !input_mask;
                    match op {
                        ExtendOp::ZeroExtend => KnownBitsFacts {
                            ones: kb.ones,
                            zeros: kb.zeros | upper_mask,
                        },
                        ExtendOp::SignExtend => {
                            let sign_bit = (input_mask >> 1) + 1;
                            let fill = |set: u128| if set & sign_bit == 0 { 0 } else { upper_mask };
                            KnownBitsFacts {
                                ones: (kb.ones & input_mask) | fill(kb.ones),
                                zeros: (kb.zeros & input_mask) | fill(kb.zeros),
                            }
                        }
                    }
                }
                // Popcount / Lzcount: the result is at most the input's
                // `8 * byte_size`, the width Sleigh counts over.
                _ => {
                    let max_val = (input_ty.byte_size() * 8) as u64;
                    let bits_needed = u64::BITS - max_val.leading_zeros();
                    KnownBitsFacts {
                        ones: 0,
                        zeros: type_mask & !mask_below(bits_needed),
                    }
                }
            }
        }

        _ => return None,
    };

    debug_assert_eq!(
        kb.ones & kb.zeros,
        0,
        "node_known_bits produced overlapping ones/zeros: {kb:?}",
    );

    Some((out, kb))
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
/// cone.
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

/// Known-bits worklist analysis to fixed point.
pub fn analyze(function: &strider_ir::Function) -> KnownBitsMap {
    // Reachable nodes only: a detached zombie can have zero inputs.
    let mut known: KnownBitsMap = SecondaryMap::new();
    let mut work: Worklist<NodeId> = function.reverse_postorder_filter(|_| true).collect();
    while let Some(node_id) = work.dequeue() {
        let Some((out, kb)) = node_known_bits(function, node_id, &mut |v| known[v]) else {
            continue;
        };
        if known[out] == kb {
            continue;
        }
        known[out] = kb;
        for (consumer, _idx) in function.value_uses(out) {
            work.enqueue(consumer);
        }
    }
    known
}

/// Propagates known-bit information and replaces outputs whose every bit is
/// determined with an equivalent integer constant.
pub struct KnownBits;

impl Optimizer for KnownBits {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::OptCtx<'_>,
    ) -> crate::Result<()> {
        let known = analyze(edit.function());

        // Reverse post-order, so a fold's operand folds finish before its cone
        // walk, which stops at them.
        let to_fold: Vec<(ValueId, ValueType, u128)> = edit
            .live_reverse_postorder()
            .into_iter()
            .flat_map(|node| edit.node_outputs(node).to_vec())
            .filter_map(|value| {
                let kb = known[value];
                let ty = edit.value_type_opt(value)?;
                let type_mask = ty.int_mask_u128()?;
                if !kb.all_known(type_mask) {
                    return None;
                }
                let producer = edit.producer(value);
                if matches!(*edit.node_kind(producer), NodeKind::IntConst(_)) {
                    return None;
                }
                Some((value, ty, kb.ones))
            })
            .collect();

        // `And(x, C) -> x` where every bit `x` may set lies in `C`, unless
        // either side folds below; carries `C` for its fingerprint.
        let masks: Vec<(ValueId, ValueId, ValueId)> = edit
            .live_of_kind(|k| *k == NodeKind::IntBinaryOp(IntBinaryOp::And))
            .filter_map(|node| {
                let (x, c) = var_and_const(edit.function(), node)?;
                let (out, ty) = edit.single_value_output(node).ok()?;
                let type_mask = ty.int_mask_u128()?;
                let redundant = known[x].max_value(type_mask) & !c == 0
                    && !known[x].all_known(type_mask)
                    && !known[out].all_known(type_mask);
                let [l, r] = edit.node_inputs_exact::<2>(node).ok()?;
                redundant.then_some((out, x, if l == x { r } else { l }))
            })
            .collect();

        // Each fold's whole cone is absorbed into its old producer, which
        // `replace_value` carries to the new constant. Every walk runs over
        // pre-fold edges, so the rewire comes last. A finished old producer
        // stops a later walk, so a fold nested in an earlier fold's cone is not
        // walked again, but sibling folds over one shared cone each walk it;
        // `visited` is stamped per fold and only cuts diamonds (the cone kinds
        // form no cycle).
        let mut absorbed: DenseEntitySet<NodeId> = DenseEntitySet::new();
        let mut visited: SecondaryMap<NodeId, u32> = SecondaryMap::new();
        let mut stamp: u32 = 0;
        let mut stack: Vec<NodeId> = Vec::new();
        let mut folds: Vec<(ValueId, ValueId)> = Vec::with_capacity(to_fold.len());
        for (value, ty, ones) in to_fold {
            folds.push((value, edit.build_int_const(ones, ty)?));
            stamp += 1;
            let old_producer = edit.producer(value);
            stack.clear();
            stack.push(old_producer);
            while let Some(n) = stack.pop() {
                if std::mem::replace(&mut visited[n], stamp) == stamp {
                    continue;
                }
                bump_cone_steps();
                edit.absorb_node_fingerprint(old_producer, n);
                if absorbed.contains(n) || !propagates_known_bits(edit.node_kind(n)) {
                    continue;
                }
                stack.extend(crate::peephole::input_producers_iter(edit, n));
            }
            absorbed.insert(old_producer);
        }

        for (value, new_value) in folds {
            edit.replace_value(value, new_value);
        }
        // A mask over an earlier-dropped mask takes that one's operand.
        let mut dropped: FxHashMap<ValueId, ValueId> = FxHashMap::default();
        for (out, mut x, c) in masks {
            while let Some(&below) = dropped.get(&x) {
                x = below;
            }
            edit.absorb_fingerprint(x, c);
            edit.replace_value(out, x);
            dropped.insert(out, x);
        }
        Ok(())
    }
}
