//! What the entry region binds before any instruction is lifted.

use anyhow::Result;

use crate::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Binds [`strider_target::BuiltCallingConvention::entry_address`] to
    /// the entry address, so the prologue that turns it into the TOC / GOT
    /// pointer folds to a constant.
    ///
    /// Bound whatever address the analysis starts at, although the ABI
    /// guarantees it only at a symbol's global entry: a function entered below
    /// that never reads the register before writing it, the prologue that reads
    /// it being above the entry.
    ///
    /// An entry that is also a loop header holds the register's phi, and the
    /// binding is that phi's entry-edge operand: the back edge carries the
    /// loop's own value.
    pub(crate) fn bind_entry_address_register(&mut self) -> Result<()> {
        use strider_ir::{IRBuilderExt as _, IRViewer as _, VnTypeExt as _};
        let Some(vn) = self.builder.function().default_cc().entry_address else {
            return Ok(());
        };
        let container = self.container_map.container_of(&vn);
        // Untracked means the function never names it, so nothing reads it.
        if self.builder.function().vn_id_of(&container).is_none() {
            return Ok(());
        }
        let current = self.builder.read_variable(&container)?;
        let function = self.builder.function();
        let phi = function.producer(current);
        let phi = matches!(function.node_kind(phi), strider_ir::node::NodeKind::Phi).then_some(phi);
        // Input 0 is the phi token; input 1 the entry edge's `InitialVar`.
        if let Some(phi) = phi {
            let initial = function.graph().nth_input(phi, 1).ok_or_else(|| {
                anyhow::anyhow!("the entry phi of {container:?} has no entry-edge operand")
            })?;
            self.builder.write_variable(&container, initial)?;
        }
        let addr = self.entry_machine_addr();
        let ty = vn.int_type()?;
        self.with_lift_addr(Some(addr), |s| {
            let value = s.builder.build_int_const(u128::from(addr), ty)?;
            s.write_vn(&vn, value)
        })?;
        if let Some(phi) = phi {
            let bound = self.builder.read_variable(&container)?;
            let graph = self.builder.function_mut().graph_mut();
            let entry_edge = graph.node_input_id_at(phi, 1)?;
            graph.update_input(entry_edge, bound);
            self.builder.write_variable(&container, current)?;
        }
        Ok(())
    }

    /// Each arg-passing register's largest-container `InitialVar` output is the
    /// carrier for its positional index: a narrow ABI alias (`edi`) routes
    /// through its tracked container (`rdi`).
    ///
    /// Integer and float registers are numbered in separate index spaces, so
    /// the j-th float parameter is float carrier `j`, indexed by ABI position:
    /// a register the function never names leaves a gap instead of shifting
    /// the ones after it down. Float argument registers sharing a container
    /// (AAPCS-VFP `d0`/`d1` inside `q0`) each carry their own slice of it.
    /// The integer half,
    /// [`strider_ir::FunctionBuilder::record_register_arg_carriers`], records
    /// each register's container outright, where the float loop below records
    /// the register itself when several share a container.
    /// No shipped convention names two integer argument registers inside one
    /// container, so the two cannot disagree today; a convention that did would
    /// make one argument out of two here while the caller side still passes
    /// each as its own slice.
    pub(crate) fn record_register_arg_carriers(&mut self) -> Result<()> {
        self.builder.record_register_arg_carriers();
        let float_slots = {
            let function = self.builder.function();
            function
                .default_cc()
                .float_arg_slots(function.all_vns(), |v| self.container_map.container_of(v))
        };
        // The entry machine address, so a materialised slice carries a
        // fingerprint like every other non-exempt node.
        let entry_addr = Some(self.entry_machine_addr());
        for (j, carrier) in float_slots.into_iter().enumerate() {
            let Some(carrier) = carrier else { continue };
            // A slice out of a shared container dedups with the function's own
            // first read of the register unless the entry is a loop header.
            let value = self.with_lift_addr(entry_addr, |s| s.read_initial_reg_vn(&carrier))?;
            if let Some(value) = value {
                self.builder
                    .function_mut()
                    .side_tables_mut()
                    .register_float_arg_value(j as u32, value);
            }
        }
        Ok(())
    }
}
