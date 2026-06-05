//! The [`IRBuilder`] creation trait: the single polymorphic node-creation
//! seam shared by the lift builder, the plain function, and the editing
//! context. Creation-only — liveness bookkeeping is the implementor's
//! concern, never part of the contract.

use crate::builder::FunctionBuilder;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};

/// A node-creation seam. Implementors decide their own fingerprint
/// attribution and bookkeeping policy; the trait also exposes mutable access
/// to the function under construction/edit (an escape hatch for side-table
/// work — see [`IRBuilder::function_mut`] for the caveats).
///
/// [`create_node_attributed`](IRBuilder::create_node_attributed) is the
/// primary method: it creates the node and unions every contributor's
/// asm-fingerprint into it (on top of the implementor's own attribution
/// policy). [`create_node`](IRBuilder::create_node) is a provided default
/// that calls it with no contributors.
///
/// Every builder is also a viewer: [`crate::IRViewer`] is a supertrait, so
/// the full read vocabulary (`value_type`, `node_inputs_exact`,
/// `const_value`, …) is available on any builder.  Each concrete builder
/// (`FunctionBuilder`, `EditFunction`) carries its own explicit
/// [`crate::IRViewer`] impl returning the wrapped function field.
pub trait IRBuilder: crate::IRViewer {
    /// Mutable access to the function under construction/edit.
    ///
    /// The write-side counterpart to [`crate::IRViewer::function`]. NOTE: this
    /// is a structural escape hatch — mutating graph *structure* through it
    /// bypasses [`crate::EditFunction`]'s cached live/roots bookkeeping (same
    /// caveat as [`crate::EditFunction::function_mut`]). Default methods on
    /// [`crate::IRBuilderExt`] may use it only for side-table-local work
    /// (e.g. interning a wide const), never to add/remove nodes or edges.
    fn function_mut(&mut self) -> &mut crate::Function;

    /// Create (or dedup to) a node with `kind`, `inputs`, `outputs`,
    /// applying this builder's attribution/bookkeeping policy and unioning
    /// every `contributors` node's asm-fingerprint into the result.
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

    /// Create (or dedup to) a node with no extra contributor attribution —
    /// delegates to [`Self::create_node_attributed`] with an empty
    /// contributor list.
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        self.create_node_attributed(kind, inputs, outputs, &[])
    }
}

/// Lift-time builder: structural creation plus the ambient `lift_addr`
/// asm-fingerprint stamp (its existing inherent `create_node` policy),
/// then any extra contributor fingerprints unioned in.
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
        // Create via the existing inherent path so the ambient `lift_addr`
        // stamp is applied, then union each contributor's fingerprint.
        let node = FunctionBuilder::create_node(self, kind, inputs, outputs);
        for &c in contributors {
            self.function_mut().extend_asm_fingerprint_from(node, c);
        }
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IRViewer;
    use crate::node::{IntPayload, ValueType};

    /// Construct a minimal `FunctionBuilder` with no tracked variables.
    /// Mirrors the local `empty_builder` helper in `builder/tests.rs`:
    /// no registered variables, no stamped lift address.
    fn empty_builder() -> crate::error::Result<FunctionBuilder> {
        FunctionBuilder::new(vec![], &strider_target::BuiltCallingConvention::default(), strider_target::Endianness::Little)
    }

    #[test]
    fn function_builder_builder_trait_creates_node() {
        // Verify that the FunctionBuilder IRBuilder impl creates a node with
        // the expected kind. Fingerprint stamping is tested in the
        // integration test `builder_trait.rs` where test-utils are available.
        let mut b = empty_builder().unwrap();
        assert_eq!(b.lift_addr(), None);
        let n = <FunctionBuilder as IRBuilder>::create_node(
            &mut b,
            NodeKind::IntConst(IntPayload::Small(3)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(matches!(IRViewer::function(&b).node_kind(n), NodeKind::IntConst(IntPayload::Small(3))));
    }
}
