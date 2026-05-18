//! Egg-based `KnownBits` rewriter — Phase 3 Task 3.4.
//!
//! Built alongside the imperative [`crate::opt::KnownBits`] — NOT a
//! replacement.  The parity test
//! `crates/strider-analyze/tests/known_bits_egg_parity.rs` proves
//! both produce structurally identical IR for every supported shape.
//!
//! # Design — first use of `egg::Analysis::Data`
//!
//! Per-eclass metadata = a [`BitLattice`] (`ones` + `zeros` masks +
//! the output type's full-width mask).  On union the analysis takes
//! the **bitwise OR** of `ones` and of `zeros`: a bit known in either
//! representative survives into the merged class.  Contradictions
//! (`new_ones & new_zeros != 0`) drop ALL knowledge for that bit in
//! the merged class — sound, and the only ever-soundness-preserving
//! choice without aborting.
//!
//! [`Analysis::make`] is the transfer function: it inspects the
//! e-node's payload + reads its children's [`BitLattice`]s out of the
//! egraph, and computes the new lattice value.  Mirrors v1's
//! `node_known_bits` table verbatim.
//!
//! # Rewrite step
//!
//! After `egraph.rebuild()`, we walk every class; for any class whose
//! lattice says "every bit of `type_mask` is determined", we synthesise
//! an `IntConst(ones, ty)` e-node and `egraph.union(...)` it with the
//! class.  This makes the const visible to consumers in further sweeps
//! (though in practice one sweep suffices because [`BitAnalysis::make`]
//! already propagates the lattice down the use chain).
//!
//! # Reflection back to the strider graph
//!
//! Walk every value [`NodeOutputId`] registered in the adapter.  If its
//! e-class data has a fully-determined lattice, materialise an
//! `IntConst` (or `BoolConst` for cmp results — but cmp outputs are
//! `Bool` and lie outside the tracker; this case doesn't fire) in the
//! strider graph and `replace_all_uses` to rewire every consumer.
//!
//! # Lattice width constraint
//!
//! v1 limits tracking to types that fit in `u64`.  This port keeps
//! the same constraint: types wider than U64 (U80, U128, U256, U512)
//! have `BitLattice::unknown()` analysis data and never fold.  The
//! existing `EGraphAdapter` already models U256/U512 constants as
//! opaque leaves (the spike noted "Phase 3 will extend with an
//! `IntConstWide` variant if needed" — not yet needed for KnownBits).

use std::collections::HashMap;

use egg::{Analysis, DidMerge, EGraph, Id};
use strider_ir::{ExtendOp, IntBinaryOp, IntUnaryOp};
use strider_ir::egraph_adapter::StriderLang;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};

use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

// ── Lattice ──────────────────────────────────────────────────────────────────

/// Per-eclass bit-lattice metadata.
///
/// Two masks (`ones` / `zeros`) plus the output type's full-width mask
/// (`type_mask`).  `ones & zeros == 0` is the soundness invariant —
/// preserved by every constructor here and by [`BitAnalysis::merge`]
/// (contradictions collapse to "unknown" rather than panic, because a
/// contradiction in the analysis is recoverable: drop the conflicting
/// bit's info).
///
/// When `type_mask == 0` the lattice is **unsupported** (Bool, float,
/// or width > 64) and never folds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BitLattice {
    pub ones: u64,
    pub zeros: u64,
    pub type_mask: u64,
}

impl BitLattice {
    /// All-unknown lattice for a tracked integer type.  Bits are
    /// individually unknown but the width-mask is set so callers can
    /// distinguish "tracked type, no info yet" from "untracked type".
    fn unknown_for(type_mask: u64) -> Self {
        Self {
            ones: 0,
            zeros: 0,
            type_mask,
        }
    }

    /// Untracked-type lattice: every operation involving this value
    /// degrades to "unknown" and the lattice never folds.
    fn untracked() -> Self {
        Self::default()
    }

    /// Build a lattice from a known constant value.  Bits set in `val`
    /// land in `ones`; the complement of `val & type_mask` lands in
    /// `zeros`.
    fn from_const(val: u128, ty: NodeOutputType) -> Self {
        let Some(type_mask) = u64_type_mask(ty) else {
            return Self::untracked();
        };
        let masked = (val & ty.bit_mask_u128()) as u64;
        Self {
            ones: masked & type_mask,
            zeros: !masked & type_mask,
            type_mask,
        }
    }

    /// Is every bit of `type_mask` determined?  Folding fires when
    /// this returns true.
    fn all_known(self) -> bool {
        self.type_mask != 0 && (self.ones | self.zeros) & self.type_mask == self.type_mask
    }
}

/// Returns the type-width mask in `u64` for a tracked integer type, or
/// `None` for Bool / floats / U80 / U128 / U256 / U512.
fn u64_type_mask(ty: NodeOutputType) -> Option<u64> {
    if !ty.is_integer() || !ty.fits_u64() {
        return None;
    }
    u64::try_from(ty.bit_mask_u128()).ok()
}

// ── Analysis ─────────────────────────────────────────────────────────────────

/// `egg::Analysis` impl computing [`BitLattice`] for each e-class.
#[derive(Default, Clone, Copy)]
pub struct BitAnalysis;

impl Analysis<StriderLang> for BitAnalysis {
    type Data = BitLattice;

    fn make(egraph: &mut EGraph<StriderLang, Self>, enode: &StriderLang) -> Self::Data {
        use StriderLang as L;
        // Helper: read child analysis data.
        let child = |egraph: &EGraph<StriderLang, Self>, id: Id| egraph[id].data;
        match enode {
            // ── Leaves ────────────────────────────────────────────────────
            L::Opaque(_) => BitLattice::untracked(),
            L::IntConst(v, ty) => BitLattice::from_const(*v, *ty),
            L::BoolConst(_) | L::FloatConst(..) => BitLattice::untracked(),

            // ── IntBinaryOp ───────────────────────────────────────────────
            L::IntBin(op, ty, [a, b]) => {
                let Some(type_mask) = u64_type_mask(*ty) else {
                    return BitLattice::untracked();
                };
                let l = child(egraph, *a);
                let r = child(egraph, *b);
                match op {
                    IntBinaryOp::And => BitLattice {
                        ones: (l.ones & r.ones) & type_mask,
                        zeros: (l.zeros | r.zeros) & type_mask,
                        type_mask,
                    },
                    IntBinaryOp::Or => BitLattice {
                        ones: (l.ones | r.ones) & type_mask,
                        zeros: (l.zeros & r.zeros) & type_mask,
                        type_mask,
                    },
                    IntBinaryOp::Xor => BitLattice {
                        // bit known 1 if exactly one input is known 1.
                        ones: ((l.ones & r.zeros) | (l.zeros & r.ones)) & type_mask,
                        // bit known 0 if both inputs agree (both 0 or both 1).
                        zeros: ((l.ones & r.ones) | (l.zeros & r.zeros)) & type_mask,
                        type_mask,
                    },
                    IntBinaryOp::ShiftLeft => shift_lattice(l, r, type_mask, ty.bit_width() as u64, true),
                    IntBinaryOp::ShiftRight => shift_lattice(l, r, type_mask, ty.bit_width() as u64, false),
                    _ => BitLattice::unknown_for(type_mask),
                }
            }

            // ── IntUnaryOp ────────────────────────────────────────────────
            L::IntUn(op, ty, [a]) => {
                let Some(type_mask) = u64_type_mask(*ty) else {
                    return BitLattice::untracked();
                };
                let l = child(egraph, *a);
                match op {
                    IntUnaryOp::BitNot => BitLattice {
                        ones: l.zeros & type_mask,
                        zeros: l.ones & type_mask,
                        type_mask,
                    },
                    IntUnaryOp::Neg => BitLattice::unknown_for(type_mask),
                }
            }

            // ── Truncate ──────────────────────────────────────────────────
            L::Truncate(ty, [a]) => {
                let Some(type_mask) = u64_type_mask(*ty) else {
                    return BitLattice::untracked();
                };
                let l = child(egraph, *a);
                BitLattice {
                    ones: l.ones & type_mask,
                    zeros: l.zeros & type_mask,
                    type_mask,
                }
            }

            // ── Extend ────────────────────────────────────────────────────
            L::Extend(op, ty, [a]) => {
                let Some(type_mask) = u64_type_mask(*ty) else {
                    return BitLattice::untracked();
                };
                let l = child(egraph, *a);
                // For Extend we need the input's type-mask, which we read
                // from the child's analysis data.  If the child is untracked
                // (e.g. U128) we can't reason about the extension at all.
                let input_mask = l.type_mask;
                if input_mask == 0 {
                    // Child is untracked.  Cannot reason about extend.
                    return BitLattice::unknown_for(type_mask);
                }
                match op {
                    ExtendOp::ZeroExtend => BitLattice {
                        ones: l.ones & input_mask,
                        // Upper bits become 0.
                        zeros: (l.zeros & input_mask) | (type_mask ^ input_mask),
                        type_mask,
                    },
                    ExtendOp::SignExtend => {
                        let sign_bit = (input_mask >> 1) + 1;
                        let upper_mask = type_mask & !input_mask;
                        let lower_ones = l.ones & input_mask;
                        let lower_zeros = l.zeros & input_mask;
                        if l.ones & sign_bit != 0 {
                            // Sign bit known 1 → upper bits all known 1.
                            BitLattice {
                                ones: lower_ones | upper_mask,
                                zeros: lower_zeros,
                                type_mask,
                            }
                        } else if l.zeros & sign_bit != 0 {
                            // Sign bit known 0 → upper bits all known 0.
                            BitLattice {
                                ones: lower_ones,
                                zeros: lower_zeros | upper_mask,
                                type_mask,
                            }
                        } else {
                            BitLattice {
                                ones: lower_ones,
                                zeros: lower_zeros,
                                type_mask,
                            }
                        }
                    }
                }
            }

            // ── Popcount / Lzcount ────────────────────────────────────────
            L::Popcount(ty, [a]) | L::Lzcount(ty, [a]) => {
                let Some(type_mask) = u64_type_mask(*ty) else {
                    return BitLattice::untracked();
                };
                // Need the input's width to bound the result.  Read it
                // from the child analysis data's type_mask (which holds
                // the input's full-width mask when tracked).
                let l = child(egraph, *a);
                let input_mask = l.type_mask;
                if input_mask == 0 {
                    return BitLattice::unknown_for(type_mask);
                }
                let max_val = (input_mask.count_ones() as u64).max(1);
                let bits_needed = if max_val == 0 {
                    1
                } else {
                    64u32 - max_val.leading_zeros()
                } as u64;
                let result_mask = if bits_needed >= 64 {
                    u64::MAX
                } else {
                    (1u64 << bits_needed) - 1
                };
                let upper_zeros = type_mask & !result_mask;
                BitLattice {
                    ones: 0,
                    zeros: upper_zeros,
                    type_mask,
                }
            }

            // ── Untracked variants (cast/cmp/bool/float) ──────────────────
            L::IntCmp(..)
            | L::CastToInt(..)
            | L::CastToBool(..)
            | L::CastToFloat(..)
            | L::BoolBin(..)
            | L::BoolUn(..)
            | L::FloatBin(..)
            | L::FloatUn(..)
            | L::FloatCmp(..)
            | L::IntToFloat(..)
            | L::FloatToInt(..)
            | L::FloatToFloat(..)
            | L::IntBitsToFloat(..)
            | L::FloatBitsToInt(..) => BitLattice::untracked(),
        }
    }

    fn merge(&mut self, a: &mut Self::Data, b: Self::Data) -> DidMerge {
        // Contract: a bit known in EITHER class survives the merge
        // (bitwise OR).  If a contradiction emerges (ones & zeros != 0
        // post-merge), drop the conflicting bit from both — the
        // analysis cannot prove anything sound about it.
        //
        // type_mask resolution: if exactly one side is tracked
        // (type_mask != 0), inherit it.  If both are tracked they MUST
        // agree (different widths in the same e-class is a graph
        // shape that violates the egraph's per-op type discrimination
        // in `StriderLang::matches` — but be defensive).
        let new_type_mask = if a.type_mask == 0 {
            b.type_mask
        } else if b.type_mask == 0 || a.type_mask == b.type_mask {
            a.type_mask
        } else {
            // Disagreement: drop knowledge.
            0
        };
        let new_ones = a.ones | b.ones;
        let new_zeros = a.zeros | b.zeros;
        let conflict = new_ones & new_zeros;
        let cleaned_ones = new_ones & !conflict;
        let cleaned_zeros = new_zeros & !conflict;

        let prev_a = *a;
        a.ones = cleaned_ones;
        a.zeros = cleaned_zeros;
        a.type_mask = new_type_mask;

        let a_changed = *a != prev_a;
        // For `b`'s did_change side, conservatively report whatever
        // bits `b` didn't already have.
        let b_changed = *a != b;
        DidMerge(a_changed, b_changed)
    }
}

/// Compute the lattice for a shift op.  `is_left = true` for `ShiftLeft`,
/// `false` for `ShiftRight`.  Mirrors v1's `node_known_bits` arms.
fn shift_lattice(
    lhs: BitLattice,
    rhs: BitLattice,
    type_mask: u64,
    bit_width: u64,
    is_left: bool,
) -> BitLattice {
    // Shift amount must be fully known (matches v1 semantics).
    if rhs.type_mask == 0 || (rhs.ones | rhs.zeros) & rhs.type_mask != rhs.type_mask {
        return BitLattice::unknown_for(type_mask);
    }
    // Sleigh: shift >= bit_width returns 0.
    if rhs.ones >= bit_width {
        return BitLattice {
            ones: 0,
            zeros: type_mask,
            type_mask,
        };
    }
    let shift = rhs.ones as u32;
    if is_left {
        let lower_mask = (1u64 << shift).wrapping_sub(1) & type_mask;
        let shifted_ones = (lhs.ones << shift) & type_mask;
        let shifted_zeros = ((lhs.zeros << shift) & type_mask) | lower_mask;
        BitLattice {
            ones: shifted_ones,
            zeros: shifted_zeros & !shifted_ones,
            type_mask,
        }
    } else {
        let upper_mask = !(type_mask >> shift) & type_mask;
        let shifted_ones = (lhs.ones & type_mask) >> shift;
        let shifted_zeros = ((lhs.zeros & type_mask) >> shift) | upper_mask;
        BitLattice {
            ones: shifted_ones,
            zeros: shifted_zeros & !shifted_ones,
            type_mask,
        }
    }
}

// ── KnownBitsEgg optimizer ──────────────────────────────────────────────────

/// Egg-based KnownBits.  Stateless.
pub struct KnownBitsEgg;

impl KnownBitsEgg {
    /// Construct a fresh `KnownBitsEgg`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for KnownBitsEgg {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerRaw for KnownBitsEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Step 1: build a fresh egraph WITH our analysis attached.
        // We can't reuse `EGraphAdapter::from_graph` because it's
        // hard-coded to `EGraph<StriderLang, ()>`; we reimplement the
        // walk locally so the analysis Data flows into every class as
        // it's built.
        let (egraph, output_to_eclass) = build_egraph_with_analysis(graph, entry);

        // Step 2: walk the registered value outputs; for each whose
        // e-class is fully-determined, materialise an `IntConst` in
        // the strider graph and `replace_all_uses` to rewire every
        // consumer.
        reflect_known_constants(graph, &egraph, &output_to_eclass)
    }
}

/// Build an `EGraph<StriderLang, BitAnalysis>` from the value-slice
/// subgraph reachable from `entry`.  Mirrors
/// `EGraphAdapter::from_graph` but parameterised on `BitAnalysis`
/// instead of `()`.
fn build_egraph_with_analysis(
    g: &strider_ir::Graph,
    entry: NodeId,
) -> (EGraph<StriderLang, BitAnalysis>, HashMap<NodeOutputId, Id>) {
    let mut egraph: EGraph<StriderLang, BitAnalysis> = EGraph::default();
    let mut output_to_eclass: HashMap<NodeOutputId, Id> = HashMap::new();

    for node_id in strider_ir::walk::walk_graph(g, entry) {
        for oid in g.node_outputs(node_id) {
            if g.output_kind(oid).is_value() {
                add_value_output(g, &mut egraph, &mut output_to_eclass, oid);
            }
        }
    }
    egraph.rebuild();
    (egraph, output_to_eclass)
}

/// Recursive add — memoised on `output_to_eclass`.  Mirrors
/// `EGraphAdapter::add_value_output` but parameterised on the analysis.
fn add_value_output(
    g: &strider_ir::Graph,
    egraph: &mut EGraph<StriderLang, BitAnalysis>,
    output_to_eclass: &mut HashMap<NodeOutputId, Id>,
    oid: NodeOutputId,
) -> Id {
    if let Some(&id) = output_to_eclass.get(&oid) {
        return id;
    }
    let (node_id, _) = g.output_definition(oid);
    let kind = *g.node_kind(node_id);
    let out_kind = g.output_kind(oid);

    let (enode, opaque_type_mask) = if is_opaque(&kind) {
        // Opaque leaf: capture the strider-side output type so the
        // analysis data can carry a `type_mask` for downstream
        // transfer functions (e.g. Popcount / Lzcount / Extend) that
        // need to bound the input width without re-querying the
        // strider graph.
        let opaque_mask = out_kind.as_value().and_then(u64_type_mask).unwrap_or(0);
        (StriderLang::Opaque(oid.as_u32() as u64), Some(opaque_mask))
    } else {
        let ty = out_kind.as_value().expect("value output must carry type");
        (
            build_internal_enode(g, egraph, output_to_eclass, node_id, &kind, ty),
            None,
        )
    };
    let id = egraph.add(enode);
    if let Some(type_mask) = opaque_type_mask {
        // `make` returned `BitLattice::untracked()` for the opaque
        // (it has no payload to read type info from); patch in the
        // width-mask so transfer functions that consume this class
        // see "tracked, all-bits-unknown" rather than "untracked".
        egraph.set_analysis_data(id, BitLattice::unknown_for(type_mask));
    }
    output_to_eclass.insert(oid, id);
    id
}

/// Build the `StriderLang` variant for an internal (non-opaque) node.
fn build_internal_enode(
    g: &strider_ir::Graph,
    egraph: &mut EGraph<StriderLang, BitAnalysis>,
    output_to_eclass: &mut HashMap<NodeOutputId, Id>,
    node_id: NodeId,
    kind: &NodeKind,
    ty: NodeOutputType,
) -> StriderLang {
    use NodeKind as K;
    let inputs: Vec<NodeOutputId> = g.node_inputs(node_id).into_iter().collect();
    let child_ids: Vec<Id> = inputs
        .iter()
        .map(|&inp| add_value_output(g, egraph, output_to_eclass, inp))
        .collect();
    match kind {
        K::IntConst(v) => StriderLang::IntConst(*v, ty),
        K::BoolConst(b) => StriderLang::BoolConst(*b),
        K::FloatConst(bits) => StriderLang::FloatConst(*bits, ty),
        K::IntBinaryOp(op) => StriderLang::IntBin(*op, ty, take2(&child_ids, "IntBinaryOp")),
        K::IntUnaryOp(op) => StriderLang::IntUn(*op, ty, take1(&child_ids, "IntUnaryOp")),
        K::IntCmpOp(op) => StriderLang::IntCmp(*op, take2(&child_ids, "IntCmpOp")),
        K::CastToInt => StriderLang::CastToInt(ty, take1(&child_ids, "CastToInt")),
        K::Truncate => StriderLang::Truncate(ty, take1(&child_ids, "Truncate")),
        K::Popcount => StriderLang::Popcount(ty, take1(&child_ids, "Popcount")),
        K::Lzcount => StriderLang::Lzcount(ty, take1(&child_ids, "Lzcount")),
        K::Extend(op) => StriderLang::Extend(*op, ty, take1(&child_ids, "Extend")),
        K::BoolUnaryOp(op) => StriderLang::BoolUn(*op, take1(&child_ids, "BoolUnaryOp")),
        K::BoolBinaryOp(op) => StriderLang::BoolBin(*op, take2(&child_ids, "BoolBinaryOp")),
        K::CastToBool => StriderLang::CastToBool(take1(&child_ids, "CastToBool")),
        K::FloatBinaryOp(op) => {
            StriderLang::FloatBin(*op, ty, take2(&child_ids, "FloatBinaryOp"))
        }
        K::FloatUnaryOp(op) => StriderLang::FloatUn(*op, ty, take1(&child_ids, "FloatUnaryOp")),
        K::FloatCmpOp(op) => StriderLang::FloatCmp(*op, take2(&child_ids, "FloatCmpOp")),
        K::IntToFloat => StriderLang::IntToFloat(ty, take1(&child_ids, "IntToFloat")),
        K::FloatToInt => StriderLang::FloatToInt(ty, take1(&child_ids, "FloatToInt")),
        K::FloatToFloat => StriderLang::FloatToFloat(ty, take1(&child_ids, "FloatToFloat")),
        K::IntBitsToFloat => {
            StriderLang::IntBitsToFloat(ty, take1(&child_ids, "IntBitsToFloat"))
        }
        K::FloatBitsToInt => {
            StriderLang::FloatBitsToInt(ty, take1(&child_ids, "FloatBitsToInt"))
        }
        K::CastToFloat => StriderLang::CastToFloat(ty, take1(&child_ids, "CastToFloat")),
        other => panic!("build_internal_enode: unexpected kind {other:?}"),
    }
}

fn take1(v: &[Id], ctx: &str) -> [Id; 1] {
    assert_eq!(v.len(), 1, "{ctx}: expected 1 child, got {}", v.len());
    [v[0]]
}

fn take2(v: &[Id], ctx: &str) -> [Id; 2] {
    assert_eq!(v.len(), 2, "{ctx}: expected 2 children, got {}", v.len());
    [v[0], v[1]]
}

/// Mirror of `EGraphAdapter::is_opaque_value_kind` — duplicate here so
/// this module is self-contained.  Identical to the adapter's
/// classification.
fn is_opaque(kind: &NodeKind) -> bool {
    use NodeKind as K;
    matches!(
        kind,
        K::VarPhi(..)
            | K::MemPhi
            | K::ValuePhi
            | K::InitialVar(..)
            | K::InitialMemory
            | K::FunctionArg { .. }
            | K::Load(..)
            | K::Call
            | K::CallOther { .. }
            | K::SegmentOp { .. }
            | K::CPoolRef
            | K::New
            | K::Store(..)
            | K::StackStore { .. }
            | K::StackStorePhi { .. }
            | K::Entry
            | K::ControlState
            | K::If
            | K::Return
            | K::IndirectBranch
            | K::IntConstWide(..)
    )
}

/// Walk the strider graph and replace every value output whose
/// e-class lattice is fully-determined with an `IntConst` of the
/// folded value.  Returns `Changed` if any rewrite fired.
fn reflect_known_constants(
    graph: &mut strider_ir::Graph,
    egraph: &EGraph<StriderLang, BitAnalysis>,
    output_to_eclass: &HashMap<NodeOutputId, Id>,
) -> crate::opt::Result<OptimizationResult> {
    // Snapshot the pending rewrites first; the loop mutates the graph
    // and we don't want a live HashMap iterator while that happens.
    #[derive(Clone)]
    struct Pending {
        out: NodeOutputId,
        producer: NodeId,
        value: u128,
        ty: NodeOutputType,
    }
    let mut pending: Vec<Pending> = Vec::new();

    for (&oid, &eclass) in output_to_eclass {
        let producer = graph.get_node_from_output(oid);
        let producer_kind = *graph.node_kind(producer);
        // Skip nodes that are ALREADY constants — no rewrite needed.
        if matches!(
            producer_kind,
            NodeKind::IntConst(_) | NodeKind::BoolConst(_) | NodeKind::FloatConst(_)
        ) {
            continue;
        }
        let out_kind = graph.output_kind(oid);
        let Some(ty) = out_kind.as_value() else {
            continue;
        };
        // KnownBits tracks integer types only.
        if !ty.is_integer() {
            continue;
        }
        let canon = egraph.find(eclass);
        let lattice = egraph[canon].data;
        if lattice.type_mask == 0 || !lattice.all_known() {
            continue;
        }
        // Folded value = `lattice.ones` (every bit is either ones or zeros;
        // ones is the actual value).
        pending.push(Pending {
            out: oid,
            producer,
            value: lattice.ones as u128,
            ty,
        });
    }

    if pending.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }

    let mut any = false;
    for p in pending {
        let new_out = graph.make_int_const(p.value, p.ty)?;
        let new_producer = graph.get_node_from_output(new_out);
        graph.extend_asm_fingerprint_from(new_producer, p.producer);
        if graph.replace_all_uses(p.out, new_out)? {
            any = true;
        }
    }
    Ok(if any {
        OptimizationResult::Changed
    } else {
        OptimizationResult::NoChange
    })
}

#[cfg(test)]
mod tests {
    //! White-box smoke test — full parity test lives in
    //! `crates/strider-analyze/tests/known_bits_egg_parity.rs`.
    use super::*;
    use strider_ir::{IntBinaryOp, IntUnaryOp};
    use strider_ir::test_utils::make_empty_fn;

    fn return_kind(fg: &strider_ir::BuiltFunctionGraph) -> NodeKind {
        let ret = fg
            .graph
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .unwrap();
        let inputs = fg.graph.node_inputs(ret);
        let val_out = inputs[2];
        let producer = fg.graph.get_node_from_output(val_out);
        *fg.graph.node_kind(producer)
    }

    #[test]
    fn smoke_fold_const_and() {
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(0xFFu64, NodeOutputType::U8).unwrap();
            let c = b.build_int_const(0xF0u64, NodeOutputType::U8).unwrap();
            b.build_int_binary_operation(a, c, IntBinaryOp::And, NodeOutputType::U8)
        })
        .expect("build fixture");
        let _ = KnownBitsEgg::new()
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize must not error");
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0xF0));
    }

    #[test]
    fn smoke_bitnot_round_trip() {
        let mut fg = make_empty_fn(|b| {
            let v = b.build_int_const(0xAAu64, NodeOutputType::U8).unwrap();
            let n = b.build_int_unary_operation(v, IntUnaryOp::BitNot, NodeOutputType::U8)?;
            b.build_int_unary_operation(n, IntUnaryOp::BitNot, NodeOutputType::U8)
        })
        .expect("build fixture");
        let _ = KnownBitsEgg::new()
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize must not error");
        assert_eq!(return_kind(&fg), NodeKind::IntConst(0xAA));
    }
}
