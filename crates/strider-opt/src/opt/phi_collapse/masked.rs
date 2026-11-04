//! Folds `And(phi, C)` to `v` when every arm of the phi, masked by `C`, is
//! `v`, an arm reaching the `And` or the phi itself agreeing with any `v`.
//!
//! Each arm is peeled through a bounded chain of `And` / `Or` / `Xor` with a
//! constant or known-zero operand, never through a phi, at most
//! [`PEEL_VISITS_PER_ARM`] steps per arm of each phi per run.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRBuilderExt, IRViewer, IntBinaryOp};

use crate::Result;
use crate::const_eval::{var_and_const, var_and_const_of};
use crate::opt::known_bits::{KnownBitsFacts, const_only, node_known_bits};

/// Nodes one arm of a masked phi is peeled through.
pub(super) const PEEL_STEPS: usize = 8;

/// Peel steps per arm of a phi in one run.
pub(super) const PEEL_VISITS_PER_ARM: usize = 2 * PEEL_STEPS;

/// Folds every masked use of each phi in `phis`.
pub(super) fn fold_masked_uses(edit: &mut crate::EditFunction<'_>, phis: &[NodeId]) -> Result<()> {
    let mut walked = Vec::new();
    for &phi in phis {
        if !edit.is_live(phi) || !matches!(edit.node_kind(phi), NodeKind::Phi) {
            continue;
        }
        let mut budget = (edit.node_inputs(phi).len() - 1).saturating_mul(PEEL_VISITS_PER_ARM);
        let phi_out = edit.node_outputs(phi)[0];
        let masks: Vec<(ValueId, u128)> = edit
            .value_uses(phi_out)
            .filter_map(|(user, _)| masked_use(edit, user))
            .collect();
        for (and_out, mask) in masks {
            if budget == 0 {
                break;
            }
            walked.clear();
            let Some(value) = masked_phi_value(edit, phi, and_out, mask, &mut walked, &mut budget)?
            else {
                continue;
            };
            let into = edit.producer(value);
            for &n in &walked {
                edit.absorb_node_fingerprint(into, n);
            }
            edit.absorb_node_fingerprint(into, phi);
            edit.replace_value(and_out, value);
        }
    }
    Ok(())
}

/// `user`'s output and mask when it is `And(phi_out, C)`.
fn masked_use(edit: &crate::EditFunction<'_>, user: NodeId) -> Option<(ValueId, u128)> {
    let (_, mask) = var_and_const_of(edit, user, NodeKind::IntBinaryOp(IntBinaryOp::And))?;
    let (out, ty) = edit.single_value_output(user).ok()?;
    ty.int_mask_u128()?;
    Some((out, mask))
}

/// `And(arm, C)` after peeling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Masked {
    /// The arm reads the `And` being folded, or its phi.
    Itself,
    Const(u128),
    /// An existing value equal to `And(arm, C)`.
    Value(ValueId),
    /// `And(x, C)`.
    Of(ValueId),
}

/// The value every arm of `phi` masked by `mask` agrees on, built if needed;
/// `None` when two arms disagree, every arm is [`Masked::Itself`], or the
/// peel `budget` runs out.
///
/// Every non-`Itself` arm, and what it peels to, is available on its edge; an
/// `Itself` arm's edge is dominated by the phi's `Region`, so the agreed value
/// dominates the `Region`.
fn masked_phi_value(
    edit: &mut crate::EditFunction<'_>,
    phi: NodeId,
    and_out: ValueId,
    mask: u128,
    walked: &mut Vec<NodeId>,
    budget: &mut usize,
) -> Result<Option<ValueId>> {
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
        if agreed.is_some_and(|a| a != masked) {
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
            None => Some(edit.build_const_binop(mask, x, IntBinaryOp::And, ty)?),
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
    mask: u128,
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
        if let Some(k) = edit.int_const_u128(x) {
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
                let Some((y, k)) = var_and_const(edit.function(), node) else {
                    break;
                };
                if k & mask == mask {
                    (y, k == mask)
                } else if k & !mask == 0 {
                    return Some((Masked::Value(x), None));
                } else {
                    break;
                }
            }
            IntBinaryOp::Or | IntBinaryOp::Xor => {
                // The proof the dropped operand adds nothing under the mask.
                let mut proof = Vec::new();
                let other = if known_zero(edit, a, &mut proof) & mask == mask {
                    b
                } else {
                    proof.clear();
                    if known_zero(edit, b, &mut proof) & mask == mask {
                        a
                    } else {
                        break;
                    }
                };
                walked.extend(proof);
                (other, false)
            }
            _ => break,
        };
        witness = exact.then_some(x);
        walked.push(node);
        x = next;
    }
    Some((Masked::Of(x), witness))
}

/// Operand levels [`known_zero`] reads below a value, enough for a flag pack
/// `Or(ShiftLeft(ZeroExtend(flag), k), ...)`.
const KNOWN_ZERO_DEPTH: u32 = 4;

/// Bits of `v` proven zero from at most [`KNOWN_ZERO_DEPTH`] levels of its
/// operand cone; every node read is pushed onto `read`.
fn known_zero(edit: &crate::EditFunction<'_>, v: ValueId, read: &mut Vec<NodeId>) -> u128 {
    known_bits_near(edit.function(), v, KNOWN_ZERO_DEPTH, read).zeros
}

fn known_bits_near(
    function: &strider_ir::Function,
    v: ValueId,
    depth: u32,
    read: &mut Vec<NodeId>,
) -> KnownBitsFacts {
    let node = function.producer(v);
    read.push(node);
    let mut operand = |u| match depth {
        0 => {
            if function.int_const_u128(u).is_some() {
                read.push(function.producer(u));
            }
            const_only(function)(u)
        }
        _ => known_bits_near(function, u, depth - 1, read),
    };
    node_known_bits(function, node, &mut operand)
        .filter(|&(out, _)| out == v)
        .map_or_else(KnownBitsFacts::default, |(_, kb)| kb)
}
