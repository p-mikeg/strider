use crate::builder::FunctionBuilder;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

pub trait IRBuilder: crate::IRViewer {
    /// Bypasses [`crate::EditFunction`]'s live/roots bookkeeping: use it for
    /// side-table work only. On an `EditFunction` each borrow counts as one of
    /// its [`control_edits`](crate::EditFunction::control_edits).
    fn function_mut(&mut self) -> &mut crate::Function;

    /// Masks to `ty`; no structural edit.
    fn intern_int_const(
        &mut self,
        val: impl Into<crate::UBig>,
        ty: crate::node::ValueType,
    ) -> crate::node::IntConstId {
        self.function_mut().intern_int_const(val, ty)
    }

    /// Unions each contributor's asm-fingerprint into the result.
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
        // On a dedup hit, the lift address joins the existing fingerprint.
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
