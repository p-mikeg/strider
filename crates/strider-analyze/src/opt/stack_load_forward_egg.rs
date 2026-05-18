//! Egg-based `StackLoadForward` rewriter — Phase 3 Task 3.5b.
//!
//! Built alongside the imperative [`crate::opt::StackLoadForward`] —
//! NOT a replacement.  The parity test
//! `crates/strider-analyze/tests/stack_load_forward_egg_parity.rs`
//! proves both produce structurally identical IR for the supported
//! shapes.
//!
//! # Design — egraph-informed-but-imperative
//!
//! The egraph (a value-slice over the strider graph; see
//! `crates/strider-ir/src/egraph_adapter/`) does NOT contain memory
//! chains, `Store` / `StackStore` / `MemPhi` / `Load` nodes — those
//! are control / memory edges that the adapter discards by
//! construction.  The egraph is therefore informative for **address
//! classification** (the load's addr e-class carries a
//! [`crate::opt::stack_store_detect_egg::StackOffset`] lattice value
//! computed by [`StackOffsetAnalysis`]) but useless for the
//! actual memory-chain walk and forwarding rewrite.
//!
//! So this pass:
//! 1. Builds an `EGraphAdapter<StackOffsetAnalysis>` to classify
//!    addresses.
//! 2. For each reachable `Load`, looks up its addr e-class.  If
//!    `SpRelative(K)`, drives a memory-chain walker (the same shape
//!    as v1's `probe`) to find a dominating `StackStore { offset: K }`
//!    whose data slot we can forward.  All chain-walking is imperative
//!    on the strider graph.
//! 3. Synthesises forwarding IR (existing-data slot, Truncate /
//!    ShiftRight narrowing, or ValuePhi over a MemPhi resolution)
//!    identically to v1.

use strider_ir::egraph_adapter::EGraphAdapter;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use target::Endianness;

use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};
use crate::opt::sp_expr::{
    AliasStep, SpExpr, SpExprMemo, decompose_sp, ranges_disjoint,
    step_through_stack_store_phi, step_through_store,
};
use crate::opt::stack_store_detect_egg::{StackOffset, StackOffsetAnalysis};

/// Store-to-load forwarding for SP-relative stack slots, egg-informed.
pub struct StackLoadForwardEgg {
    /// Varnode for the stack pointer register.
    pub stack_ptr_vn: rsleigh::Vn,
    /// Target endianness — controls how a narrow load from a wider
    /// store is synthesised.
    pub endianness: Endianness,
}

impl StackLoadForwardEgg {
    /// Construct a fresh pass for the given stack-pointer varnode and
    /// target endianness.
    #[must_use]
    pub fn new(stack_ptr_vn: rsleigh::Vn, endianness: Endianness) -> Self {
        Self {
            stack_ptr_vn,
            endianness,
        }
    }
}

impl OptimizerRaw for StackLoadForwardEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Collect Load nodes first; we'll mutate the graph during forwarding.
        let loads: Vec<NodeId> = strider_ir::walk::walk_graph(graph, entry)
            .filter(|&n| matches!(graph.node_kind(n), NodeKind::Load(_)))
            .collect();

        if loads.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // Build the egraph + analysis once, before any mutation.  The
        // egraph only informs the load-address classification step;
        // mutations below don't invalidate the cached e-class data
        // because we only consult it for the load addrs we collected
        // up front.
        let sp_vn = self.stack_ptr_vn;
        let graph_ref: &strider_ir::Graph = graph;
        let adapter: EGraphAdapter<StackOffsetAnalysis> =
            EGraphAdapter::from_graph_with_analysis_and_visit(
                graph_ref,
                entry,
                StackOffsetAnalysis,
                |egraph, _oid, kind, id| {
                    if let NodeKind::InitialVar(vn) = kind {
                        if *vn == sp_vn {
                            egraph.set_analysis_data(id, StackOffset::Sp);
                        }
                    }
                },
            );

        let mut memo: SpExprMemo = Default::default();
        let mut any_changed = false;
        for load in loads {
            // Skip loads that have been detached by a prior forwarding
            // iteration (their inputs were removed; node_inputs returns
            // an empty slice).
            if graph.node_inputs(load).is_empty() {
                continue;
            }
            let changed = try_forward_load_egg(
                graph,
                load,
                sp_vn,
                self.endianness,
                &adapter,
                &mut memo,
            )?;
            if changed {
                any_changed = true;
            }
        }

        Ok(if any_changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

/// Classify the load's addr via the egraph: returns `Some(K)` if
/// SP-relative at offset K, else None.
fn classify_addr_via_egraph(
    addr: NodeOutputId,
    adapter: &EGraphAdapter<StackOffsetAnalysis>,
) -> Option<i64> {
    let &eclass = adapter.output_to_eclass.get(&addr)?;
    let canon = adapter.egraph.find(eclass);
    match &adapter.egraph[canon].data {
        StackOffset::Sp => Some(0),
        StackOffset::SpRelative(off) => Some(*off),
        _ => None,
    }
}

fn try_forward_load_egg(
    graph: &mut strider_ir::Graph,
    load: NodeId,
    sp_vn: rsleigh::Vn,
    endianness: Endianness,
    adapter: &EGraphAdapter<StackOffsetAnalysis>,
    memo: &mut SpExprMemo,
) -> crate::opt::Result<bool> {
    let [mem, addr] = graph.node_inputs_exact::<2>(load)?;
    let [load_out] = graph.node_outputs_exact::<1>(load)?;
    let Some(load_ty) = graph.output_kind(load_out).as_value() else {
        return Ok(false);
    };

    // Address classification: egraph first, fall back to decompose_sp
    // for shapes the egraph analysis doesn't cover (e.g. VarPhi(sp),
    // which is opaque in the egraph).
    let offset = if let Some(off) = classify_addr_via_egraph(addr, adapter) {
        off
    } else {
        let mut visiting: entity_utils::DenseEntitySet<NodeId> = entity_utils::DenseEntitySet::new();
        let Some(SpExpr::Terminal { base: _, offset }) =
            decompose_sp(graph, addr, sp_vn, memo, &mut visiting)
        else {
            return Ok(false);
        };
        offset
    };

    let load_size = load_ty.byte_size() as i64;
    let mut visited: entity_utils::DenseEntitySet<NodeOutputId> = entity_utils::DenseEntitySet::new();
    let Some(shape) = probe(graph, mem, offset, load_size, load_ty, sp_vn, memo, &mut visited)
    else {
        return Ok(false);
    };
    let forwarded = realize(graph, shape, load_ty, endianness, load)?;
    let forwarded_node = graph.get_node_from_output(forwarded);
    graph.extend_asm_fingerprint_from(forwarded_node, load);
    let changed = graph.replace_all_uses(load_out, forwarded)?;
    if changed {
        graph.detach_node_inputs(load);
    }
    Ok(changed)
}

/// Description of how to materialize a forwarded value.  See v1's
/// [`crate::opt::stack_load_forward`] for the rationale behind splitting
/// the probe (read-only) and realize (mutating) phases — the egg port
/// mirrors v1's split exactly.
enum ResolveShape {
    Existing(NodeOutputId),
    Narrow {
        data: NodeOutputId,
        data_ty: NodeOutputType,
    },
    Phi {
        phi_token: NodeOutputId,
        preds: Vec<ResolveShape>,
    },
}

#[allow(clippy::too_many_arguments)]
fn probe(
    graph: &strider_ir::Graph,
    initial_mem: NodeOutputId,
    offset: i64,
    load_size: i64,
    load_ty: NodeOutputType,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visited: &mut entity_utils::DenseEntitySet<NodeOutputId>,
) -> Option<ResolveShape> {
    struct PhiFrame {
        phi_node: NodeId,
        phi_token: NodeOutputId,
        total_preds: usize,
        done_count: usize,
        collected: Vec<ResolveShape>,
    }

    let mut work: Vec<PhiFrame> = Vec::new();
    let mut mem = initial_mem;

    'outer: loop {
        let inner_result: Option<ResolveShape> = loop {
            let node = graph.get_node_from_output(mem);
            match *graph.node_kind(node) {
                NodeKind::StackStore { offset: k, space: _ } => {
                    let inputs = graph.node_inputs(node);
                    if inputs.len() < 3 {
                        break None;
                    }
                    let data = inputs[2];
                    let Some(data_ty) = graph.output_kind(data).as_value() else {
                        break None;
                    };
                    let store_size = data_ty.byte_size() as i64;
                    if k == offset {
                        if data_ty == load_ty {
                            break Some(ResolveShape::Existing(data));
                        } else if data_ty.is_integer()
                            && load_ty.is_integer()
                            && load_ty.byte_size() < data_ty.byte_size()
                        {
                            break Some(ResolveShape::Narrow { data, data_ty });
                        } else {
                            break None;
                        }
                    } else if ranges_disjoint(k, store_size, offset, load_size) {
                        mem = inputs[0];
                        continue;
                    } else {
                        break None;
                    }
                }
                NodeKind::Store(_) => {
                    match step_through_store(graph, node, sp_vn, memo, offset, load_size) {
                        AliasStep::MayAlias => break None,
                        AliasStep::PassThrough { prev_mem } => {
                            mem = prev_mem;
                            continue;
                        }
                    }
                }
                NodeKind::StackStorePhi { .. } => {
                    match step_through_stack_store_phi(graph, node, offset, load_size) {
                        AliasStep::MayAlias => break None,
                        AliasStep::PassThrough { prev_mem } => {
                            mem = prev_mem;
                            continue;
                        }
                    }
                }
                NodeKind::MemPhi => {
                    if !visited.insert(mem) {
                        break None;
                    }
                    let inputs = graph.node_inputs(node);
                    if inputs.len() < 2 {
                        break None;
                    }
                    let phi_token = inputs[0];
                    let total_preds = inputs.len() - 1;
                    let first_pred = inputs[1];
                    work.push(PhiFrame {
                        phi_node: node,
                        phi_token,
                        total_preds,
                        done_count: 0,
                        collected: Vec::with_capacity(total_preds),
                    });
                    mem = first_pred;
                    continue;
                }
                _ => break None,
            }
        };

        let mut last_result = inner_result;
        loop {
            let Some(top) = work.last_mut() else {
                return last_result;
            };
            let Some(shape) = last_result else {
                work.pop();
                last_result = None;
                continue;
            };
            top.collected.push(shape);
            top.done_count += 1;
            if top.done_count >= top.total_preds {
                let Some(frame) = work.pop() else {
                    last_result = None;
                    return last_result;
                };
                last_result = Some(ResolveShape::Phi {
                    phi_token: frame.phi_token,
                    preds: frame.collected,
                });
                continue;
            }
            let next_slot = top.done_count + 1;
            let phi_inputs = graph.node_inputs(top.phi_node);
            let next_mem = phi_inputs[next_slot];
            mem = next_mem;
            continue 'outer;
        }
    }
}

fn realize(
    graph: &mut strider_ir::Graph,
    shape: ResolveShape,
    load_ty: NodeOutputType,
    endianness: Endianness,
    load: NodeId,
) -> crate::opt::Result<NodeOutputId> {
    match shape {
        ResolveShape::Existing(out) => Ok(out),
        ResolveShape::Narrow { data, data_ty } => {
            let shifted = match endianness {
                Endianness::Little => data,
                Endianness::Big => {
                    let shift_bits = ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
                    let shift_const_node = graph.create_node_attributed(
                        NodeKind::IntConst(u128::from(shift_bits) & data_ty.bit_mask_u128()),
                        [],
                        [NodeOutputKind::OutputType(data_ty)],
                        &[load],
                    );
                    let [shift_const] = graph.node_outputs_exact::<1>(shift_const_node)?;
                    let shr = graph.create_node_attributed(
                        NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::ShiftRight),
                        [data, shift_const],
                        [NodeOutputKind::OutputType(data_ty)],
                        &[load],
                    );
                    let [out] = graph.node_outputs_exact::<1>(shr)?;
                    out
                }
            };
            let trunc = graph.create_node_attributed(
                NodeKind::Truncate,
                [shifted],
                [NodeOutputKind::OutputType(load_ty)],
                &[load],
            );
            let [out] = graph.node_outputs_exact::<1>(trunc)?;
            Ok(out)
        }
        ResolveShape::Phi { phi_token, preds } => {
            let mut resolved: Vec<NodeOutputId> = Vec::with_capacity(preds.len());
            for p in preds {
                resolved.push(realize(graph, p, load_ty, endianness, load)?);
            }
            if let Some(&first) = resolved.first()
                && resolved.windows(2).all(|w| w[0] == w[1])
            {
                return Ok(first);
            }
            let value_phi = graph.create_node_attributed(
                NodeKind::ValuePhi,
                std::iter::once(phi_token).chain(resolved),
                [NodeOutputKind::OutputType(load_ty)],
                &[load],
            );
            let [out] = graph.node_outputs_exact::<1>(value_phi)?;
            Ok(out)
        }
    }
}
