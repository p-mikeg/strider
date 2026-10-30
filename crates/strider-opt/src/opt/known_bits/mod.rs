use cranelift_entity::SecondaryMap;
use entity_utils::{DenseEntitySet, Worklist};

use strider_ir::eval;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{ExtendOp, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, UBig};

use crate::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

/// Unrecorded outputs read back as `KnownBitsFacts::default()` (`{0, 0}` =
/// "no info").
pub type KnownBitsMap = SecondaryMap<ValueId, KnownBitsFacts>;

/// Known-bit lattice for one output.  `ones` and `zeros` are masked to the
/// output type's width and must never overlap: `ones & zeros == 0`, and a bit
/// in neither set is unknown.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct KnownBitsFacts {
    pub(crate) ones: UBig,
    pub(crate) zeros: UBig,
}

impl KnownBitsFacts {
    /// `None` for a float, which has no integer bits.
    fn from_const(val: &UBig, ty: ValueType) -> Option<Self> {
        if !ty.is_integer() {
            return None;
        }
        let masked = eval::mask(val, ty);
        Some(KnownBitsFacts {
            zeros: eval::complement(&masked, ty),
            ones: masked,
        })
    }

    /// Whether every bit of `ty` is determined.  False for a float, whose
    /// zero-width bit set would otherwise read as fully known.
    fn all_known(&self, ty: ValueType) -> bool {
        ty.is_integer() && eval::mask(&(&self.ones | &self.zeros), ty) == eval::all_ones(ty)
    }

    /// The known-zero bits within the `u128` carrier, for a caller whose own
    /// question is a `u128` one.
    pub(crate) fn zeros_u128(&self) -> u128 {
        u128::try_from(self.zeros.clone().split_bits(128).0).expect("128 bits fit a u128")
    }

    /// Upper bound on the runtime value: every bit that could be 1, set.
    /// `type_mask` is a width the carrier holds, so bits above it are not part
    /// of the answer.
    pub(crate) fn max_value(&self, type_mask: u128) -> u128 {
        !self.zeros_u128() & type_mask
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
fn classify_const_shift(
    rhs_kb: &KnownBitsFacts,
    rhs_ty: Option<ValueType>,
    ty: ValueType,
) -> ConstShift {
    if !rhs_ty.is_some_and(|rhs_ty| rhs_kb.all_known(rhs_ty)) {
        return ConstShift::Unknown;
    }
    // A count no `u128` holds is past every supported width.
    let Ok(amount) = u128::try_from(&rhs_kb.ones) else {
        return ConstShift::OverWidth;
    };
    if eval::shift_out_of_range(amount, ty) {
        return ConstShift::OverWidth;
    }
    ConstShift::InRange(amount as u32)
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
    l: &KnownBitsFacts,
    rhs: ValueId,
    rhs_kb: &KnownBitsFacts,
    ty: ValueType,
    dir: ShiftDir,
) -> Option<KnownBitsFacts> {
    match classify_const_shift(rhs_kb, function.value_type_opt(rhs), ty) {
        ConstShift::Unknown => None,
        ConstShift::OverWidth => Some(KnownBitsFacts {
            ones: UBig::ZERO,
            zeros: eval::all_ones(ty),
        }),
        ConstShift::InRange(shift) => {
            let shift = shift as usize;
            let (shifted_ones, shifted_zeros) = match dir {
                ShiftDir::Left => {
                    // Vacated low bits.
                    let lower_mask = eval::mask(&eval::ones_at(shift, 0), ty);
                    (
                        eval::mask(&(&l.ones << shift), ty),
                        eval::mask(&(&l.zeros << shift), ty) | lower_mask,
                    )
                }
                ShiftDir::Right => {
                    // Vacated high bits; `shift` is below the width here.
                    let upper_mask = eval::ones_at(shift, ty.bit_width() - shift);
                    (
                        eval::mask(&l.ones, ty) >> shift,
                        (eval::mask(&l.zeros, ty) >> shift) | upper_mask,
                    )
                }
            };
            let zeros = shifted_zeros & eval::complement(&shifted_ones, ty);
            Some(KnownBitsFacts {
                ones: shifted_ones,
                zeros,
            })
        }
    }
}

/// Operand facts read from the IR alone: an interned constant, nothing else.
/// The resolver a one-hop caller passes [`node_known_bits`].
pub(crate) fn const_only(
    function: &strider_ir::Function,
) -> impl Fn(ValueId) -> KnownBitsFacts + '_ {
    move |v| {
        function
            .int_const(v)
            .zip(function.value_type_opt(v))
            .and_then(|(k, ty)| KnownBitsFacts::from_const(k, ty))
            .unwrap_or_default()
    }
}

/// Known bits of `node_id`'s first integer value output, reading each
/// operand's facts through `operand`.  [`analyze`] passes its map; a one-hop
/// caller passes [`const_only`].  `None` if there is no such output, or
/// nothing can be proven about it.
pub(crate) fn node_known_bits(
    function: &strider_ir::Function,
    node_id: NodeId,
    operand: &dyn Fn(ValueId) -> KnownBitsFacts,
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
    if !ty.is_integer() {
        return Ok(None);
    }
    let type_mask = eval::all_ones(ty);

    let kb = match kind {
        NodeKind::IntConst(_) => match function
            .int_const(out)
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
            let l = operand(lhs);
            let r = operand(rhs);
            match op {
                // Every arm masks to `type_mask`, defensively: the graph is
                // not re-validated mid-pipeline, and an operand's above-width
                // known bits must not leak into these facts.
                IntBinaryOp::And => KnownBitsFacts {
                    ones: &l.ones & &r.ones & &type_mask,
                    zeros: (&l.zeros | &r.zeros) & &type_mask,
                },
                IntBinaryOp::Or => KnownBitsFacts {
                    ones: (&l.ones | &r.ones) & &type_mask,
                    zeros: &l.zeros & &r.zeros & &type_mask,
                },
                IntBinaryOp::Xor => KnownBitsFacts {
                    ones: ((&l.ones & &r.zeros) | (&l.zeros & &r.ones)) & &type_mask,
                    zeros: ((&l.ones & &r.ones) | (&l.zeros & &r.zeros)) & &type_mask,
                },
                IntBinaryOp::ShiftLeft => {
                    return Ok(shift_known_bits(function, &l, rhs, &r, ty, ShiftDir::Left)
                        .map(|facts| (out, facts)));
                }
                IntBinaryOp::ShiftRight => {
                    return Ok(shift_known_bits(function, &l, rhs, &r, ty, ShiftDir::Right)
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
            let kb = operand(value);
            KnownBitsFacts {
                ones: &kb.ones & &type_mask,
                zeros: &kb.zeros & &type_mask,
            }
        }

        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            let [value] = function
                .graph()
                .node_inputs_exact::<1>(node_id)
                .expect("Extend has 1 input per node signature");
            let input_kind = function.value_kind(value);
            let input_ty = input_kind.as_value_or_err()?;
            // A float input has no integer bits to widen, and an all-zero
            // `input_mask` would mark every bit known-zero.
            if !input_ty.is_integer() {
                return Ok(None);
            }
            let input_mask = eval::all_ones(input_ty);
            let kb = operand(value);
            KnownBitsFacts {
                zeros: &kb.zeros | eval::complement(&input_mask, ty), // upper bits are 0
                ones: kb.ones,
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
            if !input_ty.is_integer() {
                return Ok(None);
            }
            let input_mask = eval::all_ones(input_ty);
            let kb = operand(value);
            // Highest bit of the input width.
            let sign_bit = eval::ones_at(1, input_ty.bit_width() - 1);
            let upper_mask = eval::complement(&input_mask, ty);
            if &kb.ones & &sign_bit != UBig::ZERO {
                KnownBitsFacts {
                    ones: (&kb.ones & &input_mask) | upper_mask,
                    zeros: &kb.zeros & &input_mask,
                }
            } else if &kb.zeros & &sign_bit != UBig::ZERO {
                KnownBitsFacts {
                    ones: &kb.ones & &input_mask,
                    zeros: (&kb.zeros & &input_mask) | upper_mask,
                }
            } else {
                KnownBitsFacts {
                    ones: &kb.ones & &input_mask,
                    zeros: &kb.zeros & &input_mask,
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
            let bits_needed = (u64::BITS - max_val.leading_zeros()) as usize;
            KnownBitsFacts {
                ones: UBig::ZERO,
                zeros: eval::complement(&eval::ones_at(bits_needed, 0), ty),
            }
        }

        _ => return Ok(None),
    };

    // Overlapping ones/zeros can only mean a transfer-function bug.
    debug_assert_eq!(
        &kb.ones & &kb.zeros,
        UBig::ZERO,
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
        let Some((out, kb)) = node_known_bits(function, node_id, &|v| known[v].clone())? else {
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
        _opt_ctx: &crate::OptCtx<'_>,
    ) -> crate::Result<OptimizationResult> {
        let known = analyze(edit.function())?;

        // Collect first so the mutating loop below can take `&mut edit`.
        let to_fold: Vec<(ValueId, ValueType, UBig)> = known
            .iter()
            .filter_map(|(value, kb)| {
                let ty = edit.value_type_opt(value)?;
                if !kb.all_known(ty) {
                    return None;
                }
                // Folding an existing IntConst would be a no-op.
                let producer = edit.producer(value);
                if matches!(*edit.node_kind(producer), NodeKind::IntConst(_)) {
                    return None;
                }
                Some((value, ty, kb.ones.clone()))
            })
            .collect();

        // The cone is about to be cascade-culled with its asm-fingerprints.  A
        // contributor that never folds itself (the `x & 1` in
        // `((x & 1) | 2) & 0`) sits below the fold's direct inputs, so a
        // one-hop absorb loses it.
        //
        // Constants first, absorb second, rewire last: the cone walk reads
        // input edges and `replace_value` rewires them, so every walk must run
        // over the pre-fold edges.  Creating a constant only adds a node (or
        // dedups onto an existing one) and rewires nothing.  No `IntConst` is
        // ever a fold's old producer, so the dedup pool is stable across the
        // rewire pass either way.
        let mut folds: Vec<(ValueId, ValueId)> = Vec::with_capacity(to_fold.len());
        for (value, ty, ones) in to_fold {
            folds.push((value, edit.build_int_const(ones, ty)?));
        }

        // A fold's contributors belong to the REPLACEMENT, so the cone is
        // unioned into the fold's own producer, which the rewire below kills,
        // and `replace_value` carries that one set to the new constant.  A cone
        // node that is not itself fully known stays live and must not inherit
        // its inputs' addresses.
        //
        // A finished old producer covers its whole cone, so a fold above stops
        // there: nested folds (`t(i) = t(i-1) | C(i)`) would otherwise be
        // quadratic.  The cone kinds cannot form a value cycle (only a `Phi`
        // closes one, and it does not propagate), so `visited` only cuts
        // diamonds; it is stamped per fold rather than cleared, since clearing
        // costs the node space each time.
        let mut absorbed: DenseEntitySet<NodeId> = DenseEntitySet::new();
        let mut visited: SecondaryMap<NodeId, u32> = SecondaryMap::new();
        let mut stamp: u32 = 0;
        let mut stack: Vec<NodeId> = Vec::new();
        for &(value, _) in &folds {
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

        let mut result = OptimizationResult::NoChange;
        for (value, new_value) in folds {
            if edit.replace_value(value, new_value) {
                result = OptimizationResult::Changed;
            }
        }
        Ok(result)
    }
}
