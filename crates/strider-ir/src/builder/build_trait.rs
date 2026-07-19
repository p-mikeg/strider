//! The one polymorphic node-creation seam, shared by the lift builder, the
//! plain function and the editing context. Creation only: liveness
//! bookkeeping stays the implementor's concern, never part of the contract.

use crate::builder::FunctionBuilder;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

/// Implementors set their own fingerprint-attribution and bookkeeping policy.
///
/// [`create_node_attributed`](IRBuilder::create_node_attributed) is the real
/// method: it creates the node and unions every contributor's asm-fingerprint
/// in on top of the implementor's own policy.
pub trait IRBuilder: crate::IRViewer {
    /// A structural escape hatch: mutating graph structure through this
    /// bypasses [`crate::EditFunction`]'s cached live/roots bookkeeping.
    /// [`crate::IRBuilderExt`] default methods may use it only for
    /// side-table-local work such as interning a const, never to add or
    /// remove nodes and edges.
    fn function_mut(&mut self) -> &mut crate::Function;

    /// Creates or dedups to a node, applying this builder's attribution
    /// policy and unioning each contributor's asm-fingerprint into the result.
    fn create_node_attributed<I, O>(
        &mut self,
        kind: NodeKind,
        inputs: I,
        outputs: O,
        contributors: &[NodeId],
    ) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>;

    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        self.create_node_attributed(kind, inputs, outputs, &[])
    }
}

/// Applies the ambient `lift_addr` stamp first, then the contributors.
impl IRBuilder for FunctionBuilder {
    fn function_mut(&mut self) -> &mut crate::Function {
        &mut self.function
    }

    fn create_node_attributed<I, O>(
        &mut self,
        kind: NodeKind,
        inputs: I,
        outputs: O,
        contributors: &[NodeId],
    ) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        // Go through the inherent path so the `lift_addr` stamp is applied.
        let node = FunctionBuilder::create_node(self, kind, inputs, outputs);
        for &c in contributors {
            self.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint_from(node, c);
        }
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IRViewer;
    use crate::node::ValueType;
    use cranelift_entity::EntityRef;

    /// No tracked variables, no stamped lift address.
    fn empty_builder() -> crate::error::Result<FunctionBuilder> {
        FunctionBuilder::new(
            vec![],
            strider_target::BuiltCallingConvention::default(),
            strider_target::Endianness::Little,
        )
    }

    #[test]
    fn function_builder_builder_trait_creates_node() {
        // Fingerprint stamping is covered by the integration test, which has
        // test-utils available.
        let mut b = empty_builder().unwrap();
        assert_eq!(b.lift_addr, None);
        let const_id = crate::node::const_value::ConstId::new(3);
        let n = <FunctionBuilder as IRBuilder>::create_node(
            &mut b,
            NodeKind::IntConst(const_id),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(matches!(
            IRViewer::function(&b).node_kind(n),
            NodeKind::IntConst(id) if *id == const_id
        ));
    }
}
