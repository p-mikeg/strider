//! Folds `And(phi, C)` to `v` when every arm of the phi, masked by `C`, is
//! `v`, an arm that reaches the `And` or the phi itself agreeing with any
//! `v`: a loop that saves `(hi & ~C) | (phi & C)` on its back edge keeps the
//! masked bits it entered with. The dominance argument is Braun's, as in the
//! parent module.
//!
//! Each arm is peeled through a bounded chain of `And` / `Or` / `Xor` with a
//! constant or known-zero operand, never through a phi, and a run spends at
//! most [`PEEL_VISITS_PER_ARM`] steps per arm of each phi, so a phi under many
//! masks stays linear; a mask past its phi's budget is left alone.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRBuilderExt, IRViewer, IntBinaryOp, UBig};

use crate::Result;
use crate::const_eval::var_and_ubig;
use crate::opt::known_bits::{const_only, node_known_bits};
use crate::pipeline::OptimizationResult;

/// Nodes one arm of a masked phi is peeled through.
pub(super) const PEEL_STEPS: usize = 8;

/// Peel steps the masked-phi fold may take per arm of a phi in one run, so a
/// phi with many masked uses stays linear in its arm count.
pub(super) const PEEL_VISITS_PER_ARM: usize = 2 * PEEL_STEPS;

/// Folds every masked use of each phi in `phis`.
pub(super) fn fold_masked_uses(
    edit: &mut crate::EditFunction<'_>,
    phis: &[NodeId],
) -> Result<OptimizationResult> {
    let mut overall = OptimizationResult::NoChange;
    let mut walked = Vec::new();
    for &phi in phis {
        if !edit.is_live(phi) || !matches!(edit.node_kind(phi), NodeKind::Phi) {
            continue;
        }
        let mut budget = (edit.node_inputs(phi).len() - 1).saturating_mul(PEEL_VISITS_PER_ARM);
        let phi_out = edit.node_outputs(phi)[0];
        let masks: Vec<(ValueId, UBig)> = edit
            .value_uses(phi_out)
            .filter_map(|(user, _)| masked_use(edit, user, phi_out))
            .collect();
        for (and_out, mask) in masks {
            if budget == 0 {
                break;
            }
            if !edit.is_live(edit.producer(and_out)) {
                continue;
            }
            walked.clear();
            let Some(value) =
                masked_phi_value(edit, phi, and_out, &mask, &mut walked, &mut budget)?
            else {
                continue;
            };
            let into = edit.producer(value);
            for &n in &walked {
                edit.absorb_node_fingerprint(into, n);
            }
            edit.absorb_node_fingerprint(into, phi);
            if edit.replace_value(and_out, value) {
                overall = OptimizationResult::Changed;
            }
        }
    }
    Ok(overall)
}

/// `user`'s output and mask when it is `And(phi_out, C)`.
fn masked_use(
    edit: &crate::EditFunction<'_>,
    user: NodeId,
    phi_out: ValueId,
) -> Option<(ValueId, UBig)> {
    if !matches!(
        edit.node_kind(user),
        NodeKind::IntBinaryOp(IntBinaryOp::And)
    ) {
        return None;
    }
    // The fold builds `IntConst`s of `ty`; a float `And` has none.
    let (out, ty) = edit.single_value_output(user).ok()?;
    if !ty.is_integer() {
        return None;
    }
    let [a, b] = edit.node_inputs_exact::<2>(user).ok()?;
    let other = if a == phi_out { b } else { a };
    Some((out, edit.int_const(other)?.clone()))
}

/// `And(arm, C)` after peeling.
#[derive(Clone, PartialEq, Eq)]
enum Masked {
    /// The arm reads the `And` being folded, or its phi.
    Itself,
    Const(UBig),
    /// An existing value equal to `And(arm, C)`.
    Value(ValueId),
    /// `And(x, C)`.
    Of(ValueId),
}

/// The value every arm of `phi` masked by `mask` agrees on, built if needed;
/// `None` when two arms disagree, every arm is [`Masked::Itself`], or the
/// peel `budget` runs out.
///
/// Each arm other than an `Itself` one is available on its predecessor edge,
/// and so is anything it is peeled to. An `Itself` arm's edge is dominated by
/// the phi's `Region`, so every first entry into the `Region` comes through an
/// agreeing arm, and the agreed value dominates the `Region`.
fn masked_phi_value(
    edit: &mut crate::EditFunction<'_>,
    phi: NodeId,
    and_out: ValueId,
    mask: &UBig,
    walked: &mut Vec<NodeId>,
    budget: &mut usize,
) -> Result<Option<ValueId>> {
    // `masked_use` read `and_out` off a single value output.
    let ty = edit
        .value_type_opt(and_out)
        .expect("the masked And's output is a value");
    let view = &*edit;
    let selves = [view.node_outputs(phi)[0], and_out];
    let mut agreed = None;
    let mut witness = None;
    for arm in view.phi_data_inputs(phi) {
        let Some((masked, seen)) = peel(view, arm, mask, selves, walked, budget) else {
            return Ok(None);
        };
        if masked == Masked::Itself {
            continue;
        }
        if agreed.as_ref().is_some_and(|a| *a != masked) {
            return Ok(None);
        }
        agreed = Some(masked);
        witness = witness.or(seen);
    }
    Ok(match agreed {
        Some(Masked::Const(k)) => Some(edit.build_int_const(k, ty)?),
        Some(Masked::Value(v)) => Some(v),
        Some(Masked::Of(x)) => match witness {
            Some(v) => Some(v),
            None => Some(edit.build_const_binop(mask.clone(), x, IntBinaryOp::And, ty)?),
        },
        // Every arm is `Itself`.
        _ => None,
    })
}

/// `And(arm, mask)` through at most [`PEEL_STEPS`] identities, and for a
/// [`Masked::Of`] an existing `And(x, mask)` when the last step peeled one;
/// `None` once `budget` is spent.
fn peel(
    edit: &crate::EditFunction<'_>,
    mut x: ValueId,
    mask: &UBig,
    selves: [ValueId; 2],
    walked: &mut Vec<NodeId>,
    budget: &mut usize,
) -> Option<(Masked, Option<ValueId>)> {
    let mut witness = None;
    for _ in 0..PEEL_STEPS {
        *budget = budget.checked_sub(1)?;
        if selves.contains(&x) {
            return Some((Masked::Itself, None));
        }
        if let Some(k) = edit.int_const(x) {
            return Some((Masked::Const(k & mask), None));
        }
        let node = edit.producer(x);
        let NodeKind::IntBinaryOp(op) = *edit.node_kind(node) else {
            break;
        };
        let Ok([a, b]) = edit.node_inputs_exact::<2>(node) else {
            break;
        };
        let (next, exact) = match op {
            IntBinaryOp::And => {
                let Some((y, k)) = var_and_ubig(edit.function(), node) else {
                    break;
                };
                // `k & !mask == 0` without a width: the mask keeps all of `k`.
                if &k & mask == *mask {
                    (y, k == *mask)
                } else if &k & mask == k {
                    return Some((Masked::Value(x), None));
                } else {
                    break;
                }
            }
            IntBinaryOp::Or | IntBinaryOp::Xor => {
                if &known_zero(edit, a) & mask == *mask {
                    (b, false)
                } else if &known_zero(edit, b) & mask == *mask {
                    (a, false)
                } else {
                    break;
                }
            }
            _ => break,
        };
        witness = exact.then_some(x);
        walked.push(node);
        x = next;
    }
    Some((Masked::Of(x), witness))
}

/// Bits of `v` its producer alone proves zero: the known-bits transfer read
/// with operand facts from the IR alone, so no map is built for one hop.
fn known_zero(edit: &crate::EditFunction<'_>, v: ValueId) -> UBig {
    let function = edit.function();
    let facts = node_known_bits(function, function.producer(v), &const_only(function));
    // The transfer errors only where a slot the signature pins to a value does
    // not hold one, which `validate` rejects; proving nothing is the sound
    // release answer, not a reason to stop folding.
    debug_assert!(facts.is_ok(), "known bits of a value off a validated graph");
    facts
        .ok()
        .flatten()
        .filter(|&(out, _)| out == v)
        .map_or(UBig::ZERO, |(_, kb)| kb.zeros)
}
