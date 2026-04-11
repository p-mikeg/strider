use ir::BuiltFunctionGraph;
use ir::node::NodeKind;

use crate::opt::{OptimizationResult, Optimizer};
use crate::utils::{int_const_val, make_int_const, replace_all_uses};

// ── ReadOnlyMemory trait ──────────────────────────────────────────────────────

/// Provides read access to a statically-known region of memory (e.g. a binary's
/// `.rodata` or `.text` section).
///
/// The optimizer uses this trait to resolve `Load` nodes whose address is a
/// compile-time constant into the corresponding constant values, eliminating
/// the load entirely.
pub trait ReadOnlyMemory: Send + Sync {
    /// Returns the value at `addr` in `space` as an unsigned integer of `size`
    /// bytes, or `None` if the address is not part of read-only memory or the
    /// read cannot be satisfied.
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64>;
}

// ── LoadReadOnly optimizer ────────────────────────────────────────────────────

/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// Wrap a concrete memory implementation and add this optimizer to the pipeline:
///
/// ```ignore
/// pipeline.add(LoadReadOnly(my_rom));
/// ```
pub struct LoadReadOnly<M>(pub M);

impl<M: ReadOnlyMemory + 'static> Optimizer for LoadReadOnly<M> {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let nodes: Vec<_> = function.preorder().collect();
        let mut result = OptimizationResult::NoChange;

        for node_id in nodes {
            let kind = *function.graph.node_kind(node_id);
            let NodeKind::Load(space) = kind else { continue };

            // Load inputs: [memory_token, addr].
            let inputs = function.graph.node_inputs(node_id);
            if inputs.len() < 2 {
                continue;
            }
            let addr_input = inputs[1];
            let Some(addr) = int_const_val(function, addr_input) else { continue };

            // Load output: the single value output carries the loaded data type.
            let [data_out] = function.graph.node_outputs_exact::<1>(node_id)?;
            let Some(ty) = function.graph.output_kind(data_out).as_value() else { continue };
            let size = ty.byte_size();

            let Some(loaded) = self.0.read(space, addr, size) else { continue };

            let Some(masked) = ty.get_unsigned_int(loaded) else { continue };
            let new_out = make_int_const(function, masked, ty)?;
            result |= replace_all_uses(function, data_out, new_out)?;
        }
        Ok(result)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ir::FunctionBuilder;
    use ir::node::{NodeKind, NodeOutputType};
    use crate::error::Result;

    // ── tiny ROM fixture ──────────────────────────────────────────────────────

    struct TestRom;

    impl ReadOnlyMemory for TestRom {
        fn read(&self, _space: rsleigh::VnSpace, addr: u64, _size: usize) -> Option<u64> {
            match addr {
                0x1000 => Some(42),
                0x2000 => Some(0xFF),
                _ => None,
            }
        }
    }

    // ── helper ────────────────────────────────────────────────────────────────

    fn make_fn<F>(f: F) -> Result<ir::BuiltFunctionGraph>
    where
        F: FnOnce(&mut FunctionBuilder) -> Result<ir::Value>,
    {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let val = f(&mut b)?;
        b.build_return(Some(val), &[])?;
        Ok(b.build())
    }

    fn return_kind(fg: &ir::BuiltFunctionGraph) -> NodeKind {
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .expect("no Return");
        let val = fg.graph.node_inputs(ret)[1];
        *fg.graph.node_kind(fg.graph.get_node_from_output(val))
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn load_from_rom_const_addr() -> Result<()> {
        let mut fg = make_fn(|b| {
            let addr = b.build_int_const(0x1000, NodeOutputType::U64);
            Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?)
        })?;
        assert!(LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
        assert_eq!(return_kind(&fg), NodeKind::IntConst(42));
        Ok(())
    }

    #[test]
    fn load_non_rom_addr_no_change() -> Result<()> {
        let mut fg = make_fn(|b| {
            let addr = b.build_int_const(0xDEAD, NodeOutputType::U64);
            Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?)
        })?;
        assert!(!LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
        // Load node should still be present.
        assert!(fg.all_node_ids().any(|n| matches!(fg.graph.node_kind(n), NodeKind::Load(_))));
        Ok(())
    }

    #[test]
    fn load_non_const_addr_no_change() -> Result<()> {
        let mut fg = make_fn(|b| {
            // addr = 0x1000 + 0 — a non-trivial expression that constant_fold
            // would simplify, but we don't run constant_fold here.
            let base = b.build_int_const(0x1000, NodeOutputType::U64);
            let off  = b.build_int_const(0, NodeOutputType::U64);
            let addr = b.build_int_binary_operation(base, off, ir::IntBinaryOp::Add, NodeOutputType::U64)?;
            Ok(b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?)
        })?;
        // addr is an Add node, not a const → LoadReadOnly must not fire.
        assert!(!LoadReadOnly(TestRom).optimize(&mut fg)?.changed());
        Ok(())
    }
}
