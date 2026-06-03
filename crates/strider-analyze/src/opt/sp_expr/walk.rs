//! Step-through walkers used by SP-aware memory-chain analyses to
//! decide whether a single memory-side-effecting node aliases a query
//! byte range.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::Function;

use crate::opt::AliasMode;

use super::decompose::{decompose_sp, SpExpr, SpExprMemo};
use super::ranges::{ranges_disjoint, store_value_byte_size};

/// Outcome of inspecting a memory-chain node for the byte range
/// `[query_off, query_off + query_size)`: either the node may alias and
/// further walking must terminate, or it is provably non-aliasing and the
/// caller may step past it.
///
/// The non-aliasing arm carries no prior-memory output: the caller
/// ([`crate::opt::memory_ssa::walk_memory_ssa`]) advances the cursor via
/// its own memory-token traversal, so this verdict only conveys whether
/// the store aliases.
pub(crate) enum AliasStep {
    /// The node is provably non-aliasing with the query range — the
    /// caller may step past it.
    PassThrough,
    /// The node may alias the query range (overlapping byte ranges, an
    /// SP-rooted Phi address, or malformed inputs).  Caller must terminate.
    MayAlias,
}

/// Decides whether walking past `node` (a raw `NodeKind::Store`) is safe
/// for the SP-rooted query slot `query_base + [query_off, query_off +
/// query_size)`.  `query_base` is the query load's own SP terminal base
/// (from [`decompose_sp`]); see [`AliasMode`] for the soundness/coverage
/// trade-off the `mode` parameter controls.
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_through_store(
    function: &Function,
    node: NodeId,
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    query_base: ValueId,
    query_off: i64,
    query_size: i64,
    mode: AliasMode,
) -> AliasStep {
    // Store inputs: [MEM, ADDR, DATA] — exactly 3 once the kind is
    // established by the caller (validated structural invariant).
    let inputs = function.graph().node_inputs_exact::<3>(node)
        .expect("Store node has 3 inputs (validated)");
    match decompose_sp(function, inputs[1], stack_vn, sp_memo) {
        None => match mode {
            // Strict: cannot prove disjoint from an SP-rooted query
            // without a memory-layout assumption.  Bail.
            AliasMode::Strict => AliasStep::MayAlias,
            // Permissive: stack region and constant-address region are
            // assumed disjoint.  A Store whose address is a literal
            // `IntConst` therefore cannot alias the SP-rooted query;
            // step through.  Anchor addresses (anything else) still
            // bail — closing that gap requires escape analysis.
            AliasMode::AssumeStackGlobalDisjoint => {
                let store_addr_node = function.producer(inputs[1]);
                if matches!(function.node_kind(store_addr_node), NodeKind::IntConst(_)) {
                    AliasStep::PassThrough
                } else {
                    AliasStep::MayAlias
                }
            }
        },
        Some(SpExpr { base: store_base, offset: store_off }) => {
            // Two SP terminals are comparable by offset alone only when
            // they share the same base.  Distinct SP bases — e.g. the
            // entry SP vs an alignment-masked `sp & mask` — differ by an
            // unknown, caller-dependent delta, so a byte-range check on
            // their offsets is meaningless.  Treat a base mismatch as
            // may-alias rather than wrongly proving disjointness.
            if store_base != query_base {
                return AliasStep::MayAlias;
            }
            let store_size = store_value_byte_size(function.graph(), inputs[2]);
            if ranges_disjoint(store_off, store_size, query_off, query_size) {
                AliasStep::PassThrough
            } else {
                AliasStep::MayAlias
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};
    use strider_ir::IntBinaryOp;

    /// The `InitialVar(sp)` output — the canonical entry-SP terminal base
    /// that `decompose_sp` returns for any clean `sp + k` address.
    fn entry_sp_value(f: &Function, sp: rsleigh::Vn) -> ValueId {
        let node = f
            .graph()
            .all_node_ids()
            .find(|&n| matches!(*f.node_kind(n), NodeKind::InitialVar(vn) if vn == sp))
            .expect("InitialVar(sp) exists");
        f.node_outputs_exact::<1>(node).expect("InitialVar has 1 output")[0]
    }

    fn only_store(f: &Function) -> NodeId {
        f.graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("one store")
    }

    /// Regression for the two-terminal base bug: a `Store` whose address is
    /// an *alignment-masked* SP base (`(sp & mask) + 8`) must NOT be proven
    /// disjoint from a query slot rooted at the *entry* SP just because
    /// their offsets don't overlap.  The two bases differ by the runtime
    /// alignment delta `sp mod align`, so the offset comparison is
    /// meaningless and the verdict must be may-alias.
    ///
    /// Before the fix, `step_through_store` discarded the store's base and
    /// compared offsets only: `ranges_disjoint(8, 4, 0, 4) == true` wrongly
    /// stepped through a store that, for a small alignment delta, overlaps
    /// the query bytes.
    #[test]
    fn different_base_terminal_store_may_alias() {
        let sp = stack_vn_x86();
        let f = make_sp_fn(sp, |b, sp_val| {
            // aligned = sp & 0xFFFF_FFF8  (a distinct SP base)
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
            let aligned =
                b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
            // store at aligned + 8
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            Ok(())
        })
        .unwrap();

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let mut memo = SpExprMemo::default();
        let verdict = step_through_store(
            &f, store, sp, &mut memo, query_base, 0, 4,
            AliasMode::AssumeStackGlobalDisjoint,
        );
        assert!(
            matches!(verdict, AliasStep::MayAlias),
            "store at an alignment-masked base must may-alias an entry-SP query \
             (different bases are not offset-comparable)"
        );
    }

    /// Sanity: same base, non-overlapping offsets still passes through.
    #[test]
    fn same_base_disjoint_offsets_passes_through() {
        let sp = stack_vn_x86();
        let f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            Ok(())
        })
        .unwrap();

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let mut memo = SpExprMemo::default();
        // store at sp+8 (size 4) vs query at sp+0 (size 4): disjoint.
        let verdict = step_through_store(
            &f, store, sp, &mut memo, query_base, 0, 4,
            AliasMode::AssumeStackGlobalDisjoint,
        );
        assert!(matches!(verdict, AliasStep::PassThrough));
    }

    /// Sanity: same base, overlapping offsets may-alias.
    #[test]
    fn same_base_overlapping_offsets_may_alias() {
        let sp = stack_vn_x86();
        let f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            Ok(())
        })
        .unwrap();

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let mut memo = SpExprMemo::default();
        // store at sp+8 (size 4) vs query at sp+8 (size 4): overlap.
        let verdict = step_through_store(
            &f, store, sp, &mut memo, query_base, 8, 4,
            AliasMode::AssumeStackGlobalDisjoint,
        );
        assert!(matches!(verdict, AliasStep::MayAlias));
    }
}

