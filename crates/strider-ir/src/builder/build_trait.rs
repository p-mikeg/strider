//! The [`Builder`] creation trait: the single polymorphic node-creation
//! seam shared by the lift builder, the plain function, and the editing
//! context. Creation-only — liveness bookkeeping is the implementor's
//! concern, never part of the contract.

use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

/// A node-creation seam. Implementors decide their own fingerprint
/// attribution and bookkeeping policy; the trait itself only creates and
/// exposes read access to the function under construction/edit.
pub trait Builder {
    /// Create (or dedup to) a node with `kind`, `inputs`, `outputs`,
    /// applying this builder's attribution/bookkeeping policy.
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>;

    /// Read access to the underlying [`Function`].
    fn function(&self) -> &Function;
}

/// Plainest builder: structural creation only — no fingerprint, no
/// liveness. Used by template-instantiation contexts that need neither
/// (e.g. unit tests building a throwaway RHS).
impl Builder for Function {
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        self.graph_mut().create_node(kind, inputs, outputs)
    }

    fn function(&self) -> &Function {
        self
    }
}

use crate::builder::FunctionBuilder;

/// Lift-time builder: structural creation plus the ambient `lift_addr`
/// asm-fingerprint stamp (its existing inherent `create_node` policy).
impl Builder for FunctionBuilder {
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        FunctionBuilder::create_node(self, kind, inputs, outputs)
    }

    fn function(&self) -> &Function {
        FunctionBuilder::function(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{ValueKind, ValueType};

    /// Construct a minimal `FunctionBuilder` with no tracked variables.
    /// Mirrors the local `empty_builder` helper in `builder/tests.rs`:
    /// no registered variables, no stamped lift address.
    fn empty_builder() -> crate::error::Result<FunctionBuilder> {
        let cc = strider_target::BuiltCallingConvention {
            arg_passing_regs: vec![],
            callee_saved_regs: vec![],
            ret_val_regs: vec![],
            ret_val_regs_float: vec![],
            stack_vn: strider_target::BuiltCallingConvention::default().stack_vn,
            stack_arg_offsets: vec![],
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
        };
        FunctionBuilder::new(vec![], &cc, strider_target::Endianness::Little)
    }

    #[test]
    fn function_builder_trait_creates_node() {
        let mut fx = Function::default();
        let n = <Function as Builder>::create_node(
            &mut fx,
            NodeKind::IntConst(9),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(matches!(fx.node_kind(n), NodeKind::IntConst(9)));
    }

    #[test]
    fn function_builder_delegates_to_inherent_create_node() {
        // Verify that the FunctionBuilder Builder impl creates a node with
        // the expected kind. Fingerprint stamping is tested in the
        // integration test `builder_trait.rs` where test-utils are available.
        let mut b = empty_builder().unwrap();
        b.set_lift_addr(None); // no fingerprint for this structural test
        let n = <FunctionBuilder as Builder>::create_node(
            &mut b,
            NodeKind::IntConst(3),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(matches!(Builder::function(&b).node_kind(n), NodeKind::IntConst(3)));
    }
}
