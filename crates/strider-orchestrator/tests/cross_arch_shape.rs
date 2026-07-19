//! Cross-architecture IR structural-shape baseline.
//!
//! Lifts `sum_to_n` from `fixtures/cases/control.c` for every supported
//! architecture, distils each post-optimization graph into a structural
//! fingerprint (register-name-/ID-/address-independent: node-kind
//! histogram + edge-kind totals + reachable node count + region count +
//! per-phi-kind counts), and snapshots the per-arch map. This lets x86 and
//! arm64 IR for the same source collapse to a comparable fingerprint;
//! drift in that "same source -> structurally equivalent IR" invariant
//! moves at least one arch's bucket.
//!
//! Lift failures (panics from `common::analyze`) are captured as a
//! sentinel `LIFT_FAILED` fingerprint rather than silently dropped: drift
//! in the failure set is itself part of the contract.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use common::ALL_ARCHES;
use std::collections::BTreeMap;
use strider_ir::node::{NodeKind, ValueKind};
use strider_ir::{IRViewer, IRWalker};

const FN_NAME: &str = "sum_to_n";
const CASE: &str = "control";

/// Payload-elided name for a `NodeKind` variant: scalar payload irrelevant
/// to structural shape (an `IntConst`'s value, an `InitialVar`'s varnode)
/// is dropped, but operator payload that reflects the source program
/// (`IntBinaryOp(Add)`) is kept.
fn node_kind_name(k: &NodeKind) -> &'static str {
    use strider_ir::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    };
    match k {
        NodeKind::Entry => "Entry",
        NodeKind::InitialMemory => "InitialMemory",
        NodeKind::InitialVar(_) => "InitialVar",
        NodeKind::Region => "Region",
        NodeKind::MemPhi => "MemPhi",
        NodeKind::Phi => "Phi",
        NodeKind::If => "If",
        NodeKind::Call => "Call",
        NodeKind::Return => "Return",
        NodeKind::IndirectBranch => "IndirectBranch",
        NodeKind::Unreachable => "Unreachable",
        NodeKind::Switch => "Switch",
        NodeKind::Load(_) => "Load",
        NodeKind::Store(_) => "Store",
        NodeKind::IntConst(_) => "IntConst",
        NodeKind::IntUnaryOp(op) => match op {
            IntUnaryOp::Neg => "IntUnaryOp(Neg)",
        },
        NodeKind::IntBinaryOp(op) => match op {
            IntBinaryOp::Add => "IntBinaryOp(Add)",
            IntBinaryOp::Mul => "IntBinaryOp(Mul)",
            IntBinaryOp::Div => "IntBinaryOp(Div)",
            IntBinaryOp::Sdiv => "IntBinaryOp(Sdiv)",
            IntBinaryOp::Rem => "IntBinaryOp(Rem)",
            IntBinaryOp::Srem => "IntBinaryOp(Srem)",
            IntBinaryOp::And => "IntBinaryOp(And)",
            IntBinaryOp::Or => "IntBinaryOp(Or)",
            IntBinaryOp::Xor => "IntBinaryOp(Xor)",
            IntBinaryOp::ShiftLeft => "IntBinaryOp(ShiftLeft)",
            IntBinaryOp::ShiftRight => "IntBinaryOp(ShiftRight)",
            IntBinaryOp::SShiftRight => "IntBinaryOp(SShiftRight)",
        },
        NodeKind::IntCmpOp(op) => match op {
            IntCmpOp::Equal => "IntCmpOp(Equal)",
            IntCmpOp::Less => "IntCmpOp(Less)",
            IntCmpOp::Sless => "IntCmpOp(Sless)",
            IntCmpOp::Carry => "IntCmpOp(Carry)",
            IntCmpOp::Scarry => "IntCmpOp(Scarry)",
            IntCmpOp::Sborrow => "IntCmpOp(Sborrow)",
        },
        NodeKind::Truncate => "Truncate",
        NodeKind::Popcount => "Popcount",
        NodeKind::Lzcount => "Lzcount",
        NodeKind::Extend(op) => match op {
            ExtendOp::ZeroExtend => "Extend(ZeroExtend)",
            ExtendOp::SignExtend => "Extend(SignExtend)",
        },
        NodeKind::FloatConst(_) => "FloatConst",
        NodeKind::FloatBinaryOp(op) => match op {
            FloatBinaryOp::Add => "FloatBinaryOp(Add)",
            FloatBinaryOp::Mul => "FloatBinaryOp(Mul)",
            FloatBinaryOp::Div => "FloatBinaryOp(Div)",
        },
        NodeKind::FloatUnaryOp(op) => match op {
            FloatUnaryOp::Neg => "FloatUnaryOp(Neg)",
            FloatUnaryOp::Abs => "FloatUnaryOp(Abs)",
            FloatUnaryOp::Sqrt => "FloatUnaryOp(Sqrt)",
            FloatUnaryOp::Ceil => "FloatUnaryOp(Ceil)",
            FloatUnaryOp::Floor => "FloatUnaryOp(Floor)",
            FloatUnaryOp::Round => "FloatUnaryOp(Round)",
        },
        NodeKind::FloatCmpOp(op) => match op {
            FloatCmpOp::Equal => "FloatCmpOp(Equal)",
            FloatCmpOp::Less => "FloatCmpOp(Less)",
        },
        NodeKind::IntToFloat => "IntToFloat",
        NodeKind::FloatToInt => "FloatToInt",
        NodeKind::FloatToFloat => "FloatToFloat",
        NodeKind::IntBitsToFloat => "IntBitsToFloat",
        NodeKind::FloatBitsToInt => "FloatBitsToInt",
        NodeKind::CallOther { .. } => "CallOther",
        NodeKind::SegmentOp { .. } => "SegmentOp",
        NodeKind::CPoolRef => "CPoolRef",
        NodeKind::New => "New",
    }
}

/// Structural shape of one lifted IR graph, register-/address-/value-
/// agnostic. See module doc for what is and isn't captured.
#[derive(Debug, serde::Serialize)]
struct Fingerprint {
    /// `LIFT_FAILED:<msg>` on lift panic, else `OK`.
    status: String,
    reachable_nodes: usize,
    regions: usize,
    edges_control: usize,
    edges_memory: usize,
    edges_value: usize,
    /// Broken out per phi kind so register-renaming inside `VarPhi(Vn)` /
    /// `Phi` doesn't blur the histogram.
    var_phis: usize,
    mem_phis: usize,
    value_phis: usize,
    kind_histogram: BTreeMap<String, usize>,
}

impl Fingerprint {
    fn lift_failed(msg: &str) -> Self {
        Self {
            status: format!("LIFT_FAILED:{}", msg),
            reachable_nodes: 0,
            regions: 0,
            edges_control: 0,
            edges_memory: 0,
            edges_value: 0,
            var_phis: 0,
            mem_phis: 0,
            value_phis: 0,
            kind_histogram: BTreeMap::new(),
        }
    }
}

/// `Phi` splits into `VarPhi` / `ValuePhi` via the per-value side-table
/// `value_vn` (`Function::get_vn_for_value` on the Phi's output), which
/// [`node_kind_name`] can't see without graph context; every other kind
/// delegates to it.
fn kind_bucket(function: &strider_ir::Function, nid: strider_ir::node::NodeId) -> String {
    let k = function.node_kind(nid);
    match k {
        NodeKind::Phi
            if function
                .get_vn_for_value(function.node_outputs(nid)[0])
                .is_some() =>
        {
            "VarPhi".to_string()
        }
        NodeKind::Phi => "ValuePhi".to_string(),
        _ => node_kind_name(k).to_string(),
    }
}

/// Walks every reachable node (same reachability scope as the validator's
/// local-typing check) accumulating the histogram, edge-kind totals
/// (counted by the kind of each input's producer-output), region count,
/// and per-kind phi counts.
fn structural_fingerprint(function: &strider_ir::Function) -> Fingerprint {
    let mut kind_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut reachable_nodes = 0usize;
    let mut regions = 0usize;
    let mut var_phis = 0usize;
    let mut mem_phis = 0usize;
    let mut value_phis = 0usize;
    let mut edges_control = 0usize;
    let mut edges_memory = 0usize;
    let mut edges_value = 0usize;

    for nid in function.walk() {
        reachable_nodes += 1;
        let kind = function.node_kind(nid);
        *kind_histogram
            .entry(kind_bucket(function, nid))
            .or_insert(0) += 1;
        match kind {
            NodeKind::Region => regions += 1,
            NodeKind::Phi
                if function
                    .get_vn_for_value(function.node_outputs(nid)[0])
                    .is_some() =>
            {
                var_phis += 1
            }
            NodeKind::Phi => value_phis += 1,
            NodeKind::MemPhi => mem_phis += 1,
            _ => {}
        }
        for input in function.node_inputs(nid) {
            match function.value_kind(input) {
                ValueKind::Control => edges_control += 1,
                ValueKind::Memory => edges_memory += 1,
                ValueKind::Typed(_) => edges_value += 1,
                // PhiToken edges are internal phi bookkeeping, not part of
                // the user-visible data/control shape.
                ValueKind::PhiToken => {}
            }
        }
    }

    Fingerprint {
        status: "OK".to_string(),
        reachable_nodes,
        regions,
        edges_control,
        edges_memory,
        edges_value,
        var_phis,
        mem_phis,
        value_phis,
        kind_histogram,
    }
}

#[test]
fn control_c_sum_to_n_cross_arch_shape() {
    // Confine snapshot files to a single directory under tests/snapshots/.
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    let _guard = settings.bind_to_scope();

    let mut map: BTreeMap<&'static str, Fingerprint> = BTreeMap::new();

    for &arch in ALL_ARCHES {
        let path = common::binary_path(arch, CASE);
        if !path.exists() {
            // Missing binary (fixture not built for this arch) is distinct
            // from a lift failure; record a sentinel so a future build of
            // the missing arch shows up as drift.
            map.insert(arch.name(), Fingerprint::lift_failed("MISSING_BINARY"));
            continue;
        }
        let arch_copy = arch;
        let result = std::panic::catch_unwind(move || common::analyze(arch_copy, CASE, FN_NAME));
        let fp = match result {
            Ok(g) => structural_fingerprint(&g),
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<String>() {
                    s.as_str()
                } else if let Some(s) = payload.downcast_ref::<&'static str>() {
                    *s
                } else {
                    "<non-string panic payload>"
                };
                Fingerprint::lift_failed(msg)
            }
        };
        map.insert(arch.name(), fp);
    }

    insta::assert_yaml_snapshot!("control_c_sum_to_n_cross_arch", map);
}
