//! Cross-arch verification that `FlagCmpCanonicalize` folds every
//! conditional-branch flavour down to a single direct `IntCmpOp` on the
//! original operands, on real lifted code rather than synthetic flag trees.
//!
//! The `cmp_branches` fixture has one `if (a <cmp> b)` branch per comparison
//! (see `fixtures/cases/cmp_branches.c`), wrapped in `memory` asm barriers so
//! the compiler emits a real branch rather than a conditional select.
//!
//! AArch64 emits the canonical flag tree the rules were first written for;
//! ARM/Thumb lift the branch with inverted sense (an outer `BoolNeg`), so by
//! the time the pass runs, ConstantFold has decomposed the sub-terms into
//! direct comparisons, which the "decomposed-form" rules recognise. x86 / x64
//! (EFLAGS) reach the canonical form directly. This test pins that every
//! flag-register arch ends at the same single-`IntCmpOp` shape.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::*;
use strider_ir::IRViewer;

use strider_ir::IntCmpOp;
use strider_ir::node::NodeKind;

/// Each comparison branch and the `IntCmpOp` its condition must canonicalise
/// to.  Signed comparisons fold to `Sless`, unsigned to `Less`, equality to
/// `Equal`.  `br_neg` (`x < 0`) is a sign test whose exact variant is
/// arch-dependent, so it only has to reach *some* direct `IntCmpOp`.
const CASES: &[(&str, Option<IntCmpOp>)] = &[
    ("br_eq", Some(IntCmpOp::Equal)),
    ("br_ne", Some(IntCmpOp::Equal)),
    ("br_slt", Some(IntCmpOp::Sless)),
    ("br_sge", Some(IntCmpOp::Sless)),
    ("br_sgt", Some(IntCmpOp::Sless)),
    ("br_sle", Some(IntCmpOp::Sless)),
    ("br_ugt", Some(IntCmpOp::Less)),
    ("br_ule", Some(IntCmpOp::Less)),
    ("br_ult", Some(IntCmpOp::Less)),
    ("br_uge", Some(IntCmpOp::Less)),
    ("br_neg", None),
];

fn assert_branches_canonicalize(arch: Arch) {
    for &(fn_name, expected) in CASES {
        let function = analyze(arch, "cmp_branches", fn_name);
        let mut if_count = 0;
        for nid in function.graph().all_node_ids() {
            if !function.graph().has_node(nid) || !matches!(function.node_kind(nid), NodeKind::If) {
                continue;
            }
            if_count += 1;
            let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
            assert!(
                inputs.len() >= 2,
                "{} {fn_name}: If node {nid:?} has no condition input",
                arch.name()
            );
            let cond = function.producer(inputs[1]);
            match (function.node_kind(cond), expected) {
                (NodeKind::IntCmpOp(op), Some(exp)) => assert_eq!(
                    *op,
                    exp,
                    "{} {fn_name}: branch condition canonicalised to {op:?}, expected {exp:?}",
                    arch.name()
                ),
                (NodeKind::IntCmpOp(_), None) => {}
                (other, _) => panic!(
                    "{} {fn_name}: branch condition is not a direct IntCmpOp \
                     (flag tree survived canonicalisation): {other:?}",
                    arch.name()
                ),
            }
        }
        assert!(
            if_count >= 1,
            "{} {fn_name}: expected at least one If node",
            arch.name()
        );
    }
}

#[test]
fn flag_cmp_canonicalizes_branches_aarch64() {
    assert_branches_canonicalize(Arch::Aarch64);
}

#[test]
fn flag_cmp_canonicalizes_branches_arm() {
    assert_branches_canonicalize(Arch::Arm);
}

#[test]
fn flag_cmp_canonicalizes_branches_arm_thumb() {
    assert_branches_canonicalize(Arch::ArmThumb);
}

#[test]
fn flag_cmp_canonicalizes_branches_x86() {
    assert_branches_canonicalize(Arch::X86);
}

#[test]
fn flag_cmp_canonicalizes_branches_x64() {
    assert_branches_canonicalize(Arch::X64);
}
