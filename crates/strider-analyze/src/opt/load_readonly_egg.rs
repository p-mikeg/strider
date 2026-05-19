//! Egg-based `LoadReadOnly` rewriter — Phase 3 Task 3.6.
//!
//! Built alongside the imperative [`crate::opt::LoadReadOnly`] — NOT a
//! replacement.  The parity test
//! `crates/strider-analyze/tests/load_readonly_egg_parity.rs` proves
//! both produce structurally identical IR for the supported shapes.
//!
//! # Design — egraph-informed-but-imperative
//!
//! `Load` is opaque in the egraph (the adapter discards memory-chain
//! nodes by construction), so this pass can't be expressed as a pure
//! egg rewrite.  Instead it uses the egraph as a **constant-address
//! oracle**: for each reachable `Load`, look up the address input's
//! e-class and ask "does this e-class contain a literal `IntConst`
//! e-node?".  If yes, query the caller-supplied [`ReadOnlyMemory`]
//! and materialise a fresh `IntConst` in the strider graph via
//! `replace_all_uses`.
//!
//! No `egg::Analysis::Data` is required: the existence of an
//! `IntConst(K, ty)` e-node in the e-class is itself sufficient.
//! This is strictly stronger than v1's "is the input strider node an
//! `IntConst`?" check, because two equivalent addr e-classes (e.g. an
//! `Add` that the egraph's congruence closure has merged with a
//! constant) will fold here even when v1 would have missed them.
//! Parity is preserved for the v1-supported shapes; the egg variant
//! is a superset.
//!
//! # Memory-space and endianness contract
//!
//! Mirrors v1 verbatim — see [`crate::opt::LoadReadOnly`] for the full
//! contract on `ReadOnlyMemory::read` (forwards the load's
//! [`rsleigh::VnSpace`] verbatim; rom impls must return `None` for
//! foreign spaces; endianness handled by the rom impl).

use strider_ir::egraph_adapter::{EGraphAdapter, StriderLang};
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};
use strider_ir::ReadOnlyMemory;

use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

/// Egg-informed `LoadReadOnly`.  Wraps an arbitrary
/// [`ReadOnlyMemory`] image (typically an ELF's `.rodata` / `.text`).
pub struct LoadReadOnlyEgg<M>(pub M);

impl<M: ReadOnlyMemory> LoadReadOnlyEgg<M> {
    /// Wrap a ROM image for use as an optimizer pass.
    pub fn new(rom: M) -> Self {
        Self(rom)
    }
}

impl<M: ReadOnlyMemory + 'static> OptimizerRaw for LoadReadOnlyEgg<M> {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // Step 1: collect all reachable Load nodes up front; the rewrite
        // loop below mutates the graph and we can't hold a live walk
        // iterator across mutations.
        let loads: Vec<NodeId> = strider_ir::walk::walk_graph(graph, entry)
            .filter(|&n| matches!(graph.node_kind(n), NodeKind::Load(_)))
            .collect();
        if loads.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // Step 2: build the egraph (no analysis — we only consult the
        // structural shape of each addr e-class for an `IntConst`
        // e-node).
        let adapter = EGraphAdapter::from_graph(graph, entry);

        // Step 3: classify each load, then apply rewrites.  We snapshot
        // pending rewrites first so the mutation phase doesn't disturb
        // the still-borrowed `adapter`.
        struct Pending {
            load: NodeId,
            value: u64,
            ty: NodeOutputType,
        }
        let mut pending: Vec<Pending> = Vec::new();
        for load_id in loads {
            let kind = *graph.node_kind(load_id);
            let NodeKind::Load(space) = kind else {
                continue;
            };
            // Load inputs: [memory_token, addr].
            let inputs = graph.node_inputs(load_id);
            if inputs.len() < 2 {
                continue;
            }
            let addr_out = inputs[1];
            // Address must be in the egraph (it's a value output;
            // adapter registers every reachable value output).
            let Some(&eclass) = adapter.output_to_eclass.get(&addr_out) else {
                continue;
            };
            let canon = adapter.egraph.find(eclass);
            // Scan the e-class for an `IntConst(K, _)` e-node; the
            // declared output type isn't required to match the load's
            // size — only the numeric value matters, since ROM reads
            // are byte-addressed.
            let Some(addr) = const_addr_in_eclass(&adapter, canon) else {
                continue;
            };

            // Output type: Load's single value output carries the load width.
            let [data_out] = graph.node_outputs_exact::<1>(load_id)?;
            let Some(ty) = graph.output_kind(data_out).as_value() else {
                continue;
            };
            let size = ty.byte_size();
            // `ReadOnlyMemory::read` returns `Option<u64>` — bail on
            // wider loads (U80 / U128 / U256 / U512) rather than asking
            // the impl to truncate silently.
            if size > 8 {
                continue;
            }
            let Some(loaded) = self.0.read(space, addr, size) else {
                continue;
            };
            // Mask the loaded value to the load's output width.
            let Some(masked) = ty
                .get_unsigned_int(u128::from(loaded))
                .and_then(|v| u64::try_from(v).ok())
            else {
                continue;
            };
            pending.push(Pending {
                load: load_id,
                value: masked,
                ty,
            });
        }

        if pending.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        let mut any = false;
        for p in pending {
            let [data_out] = graph.node_outputs_exact::<1>(p.load)?;
            let new_out = graph.make_int_const(p.value, p.ty)?;
            let new_producer = graph.get_node_from_output(new_out);
            graph.extend_asm_fingerprint_from(new_producer, p.load);
            if graph.replace_all_uses(data_out, new_out)? {
                any = true;
            }
        }
        Ok(if any {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

/// Return the constant address encoded in any `IntConst` e-node of the
/// given e-class, or `None` if no such e-node exists.
///
/// Multiple `IntConst` e-nodes in one e-class with different values is
/// a soundness contradiction (the e-graph's congruence closure should
/// never merge two distinct-value constants) — if we ever see it, take
/// the first; the ROM lookup will either match one of them or none.
fn const_addr_in_eclass<A: egg::Analysis<StriderLang>>(
    adapter: &EGraphAdapter<A>,
    canon: egg::Id,
) -> Option<u64> {
    for enode in adapter.egraph[canon].nodes.iter() {
        if let StriderLang::IntConst(v, _ty) = enode {
            // Address is interpreted as an unsigned `u64`; truncate
            // wider constants (the masking happens implicitly via
            // `u64::try_from`).
            return u64::try_from(*v).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! White-box smoke tests — full parity in
    //! `crates/strider-analyze/tests/load_readonly_egg_parity.rs`.
    use super::*;
    use strider_ir::test_utils::make_empty_fn;

    struct SmokeRom;
    impl ReadOnlyMemory for SmokeRom {
        fn read(&self, _space: rsleigh::VnSpace, addr: u64, _size: usize) -> Option<u64> {
            if addr == 0x1000 { Some(42) } else { None }
        }
    }

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
    fn smoke_load_const_in_rom() {
        let mut fg = make_empty_fn(|b| {
            let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
            b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
        })
        .expect("build fixture");
        let res = LoadReadOnlyEgg::new(SmokeRom)
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize must not error");
        assert!(res.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(42));
    }

    #[test]
    fn smoke_load_const_not_in_rom() {
        let mut fg = make_empty_fn(|b| {
            let addr = b.build_int_const(0xDEADu64, NodeOutputType::U64)?;
            b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
        })
        .expect("build fixture");
        let res = LoadReadOnlyEgg::new(SmokeRom)
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize must not error");
        assert!(!res.changed());
    }
}
