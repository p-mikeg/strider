//! Cross-architecture IR structural-shape baseline.
//!
//! Lifts a single representative function from `fixtures/cases/control.c`
//! for every supported architecture, distils each post-optimization IR
//! graph into a **structural fingerprint** (register-name-/ID-/address-
//! independent), and snapshots the per-arch fingerprint map.
//!
//! The structural fingerprint is invariant to register names, specific
//! `IntConst` values, asm-fingerprint addresses, and node IDs — only the
//! *shape* of the graph (node-kind histogram + edge-kind totals + reachable
//! node count + region count + per-phi-kind counts) is captured.  This lets
//! the same source on x86 and arm64 collapse to a comparable fingerprint
//! whose drift between arches is itself information worth pinning: any
//! later work that breaks the "same source ↦ structurally equivalent IR
//! across arches" invariant will move at least one arch's bucket.
//!
//! Function selected: `sum_to_n` — exercises a loop + arithmetic + return
//! path, while being simple enough to compare cleanly across arches.
//!
//! Lift failures (panics from `common::analyze`) are captured as a sentinel
//! `LIFT_FAILED` fingerprint rather than silently dropped — drift in the
//! failure set is itself part of the contract.
//!

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use common::ALL_ARCHES;
use strider_ir::node::{NodeKind, NodeOutputKind};
use std::collections::BTreeMap;

/// Function selected for the cross-arch comparison.  See module doc.
const FN_NAME: &str = "sum_to_n";
const CASE: &str = "control";

/// Structural shape of one lifted IR graph, register-/address-/value-
/// agnostic.  See module doc for what is and isn't captured.
#[derive(Debug, serde::Serialize)]
struct Fingerprint {
    /// `LIFT_FAILED:<msg>` on lift panic, else `OK`.
    status: String,
    reachable_nodes: usize,
    regions: usize,
    edges_control: usize,
    edges_memory: usize,
    edges_value: usize,
    /// Phi counts broken out by kind so register-renaming inside `VarPhi(Vn)`
    /// or `Phi` doesn't blur the histogram.
    var_phis: usize,
    mem_phis: usize,
    value_phis: usize,
    stack_store_phis: usize,
    /// `NodeKind` variant name → count.  Variant payload is elided (no
    /// register names, no constants, no user-op ids) so the bucket is
    /// arch-independent.
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
            stack_store_phis: 0,
            kind_histogram: BTreeMap::new(),
        }
    }
}

/// Canonical (payload-elided) name for a `NodeKind` variant.  Two nodes
/// with the same variant but different payload (e.g. `IntConst(0)` vs
/// `IntConst(42)`, or `InitialVar(rax)` vs `InitialVar(x0)`) collapse to
/// the same bucket.  Operator-bearing variants keep the operator name
/// (e.g. `IntBinaryOp(Add)`) since that's a *structural* property of the
/// source program, not a register-renaming artefact.
///
/// Delegates to [`NodeKind::as_static_str`] for the common case;
/// overrides only for `Phi`, which splits into `VarPhi` / `ValuePhi`
/// based on the per-node side-table `Graph::phi_var_tag` (the
/// `as_static_str` method has no graph context).
fn kind_bucket(g: &strider_ir::Function, nid: strider_ir::node::NodeId) -> String {
    let k = g.node_kind(nid);
    match k {
        NodeKind::Phi if g.phi_var_tag(nid).is_some() => "VarPhi".to_string(),
        NodeKind::Phi => "ValuePhi".to_string(),
        _ => k.as_static_str().to_string(),
    }
}

/// Compute the structural fingerprint of `g`.
///
/// Walks every reachable node via `Graph::preorder` (same
/// reachability scope used by the validator's local-typing check), accumulating:
///   * histogram of payload-elided `NodeKind` buckets,
///   * total edge counts by kind (Control / Memory / Value), where an
///     "edge" is one input slot — counted by the kind of that input's
///     producer-output,
///   * region count (one per `Region` reachable node — matches
///     how `strider_lift::cfg::Cfg` regions are projected into the IR),
///   * per-kind phi counts (broken out for sensitivity to kind drift).
fn structural_fingerprint(g: &strider_ir::Function) -> Fingerprint {
    let mut kind_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut reachable_nodes = 0usize;
    let mut regions = 0usize;
    let mut var_phis = 0usize;
    let mut mem_phis = 0usize;
    let mut value_phis = 0usize;
    let mut stack_store_phis = 0usize;
    let mut edges_control = 0usize;
    let mut edges_memory = 0usize;
    let mut edges_value = 0usize;

    for nid in g.preorder() {
        reachable_nodes += 1;
        let kind = g.node_kind(nid);
        *kind_histogram.entry(kind_bucket(g, nid)).or_insert(0) += 1;
        match kind {
            NodeKind::Region => regions += 1,
            NodeKind::Phi if g.phi_var_tag(nid).is_some() => var_phis += 1,
            NodeKind::Phi => value_phis += 1,
            NodeKind::MemPhi => mem_phis += 1,
            NodeKind::StackStorePhi { .. } => stack_store_phis += 1,
            _ => {}
        }
        // Count incoming edges by producer-output kind.
        for input in g.node_inputs(nid) {
            match g.output_kind(input) {
                NodeOutputKind::Control => edges_control += 1,
                NodeOutputKind::Memory => edges_memory += 1,
                NodeOutputKind::OutputType(_) => edges_value += 1,
                // PhiToken edges are an internal phi-bookkeeping detail and
                // not part of the user-visible data/control shape; we omit
                // them.
                NodeOutputKind::PhiToken => {}
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
        stack_store_phis,
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
            // Missing binary — fixture not built for this arch.  Distinct
            // from a lift failure; record sentinel so a future build of
            // the missing arch shows up as drift.
            map.insert(arch.name(), Fingerprint::lift_failed("MISSING_BINARY"));
            continue;
        }
        let arch_copy = arch;
        let result = std::panic::catch_unwind(move || {
            common::analyze(arch_copy, CASE, FN_NAME)
        });
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
