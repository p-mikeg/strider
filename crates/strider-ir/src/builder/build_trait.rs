use crate::builder::FunctionBuilder;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

pub trait IRBuilder: crate::IRViewer {
    /// A structural escape hatch: mutating graph structure through this
    /// bypasses [`crate::EditFunction`]'s cached live/roots bookkeeping, so
    /// use it only for side-table-local work. On an `EditFunction` every
    /// borrow counts as one of its
    /// [`control_edits`](crate::EditFunction::control_edits).
    fn function_mut(&mut self) -> &mut crate::Function;

    /// Interns an integer constant, masked to `ty`; no structural edit.
    fn intern_int_const(
        &mut self,
        val: impl Into<crate::UBig>,
        ty: crate::node::ValueType,
    ) -> crate::node::IntConstId {
        self.function_mut().intern_int_const(val, ty)
    }

    /// Creates or dedups to a node, unioning each contributor's
    /// asm-fingerprint into the result.
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
        // Also stamps the current lift address; on a dedup-cache hit it is
        // unioned into the existing entry.
        let node = self
            .function
            .create_node_attributed(kind, inputs, outputs, contributors);
        if let Some(addr) = self.lift_addr {
            self.function
                .side_tables_mut()
                .extend_asm_fingerprint(node, &[addr]);
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
        let mut b = empty_builder().unwrap();
        assert_eq!(b.lift_addr, None);
        let const_id = crate::node::IntConstId::new(3);
        let n = <FunctionBuilder as IRBuilder>::create_node(
            &mut b,
            NodeKind::IntConst(const_id),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert!(matches!(
            IRViewer::function(&b).node_kind(n),
            NodeKind::IntConst(id) if *id == const_id
        ));
    }
}
