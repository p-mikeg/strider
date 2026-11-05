use anyhow::Result;

use crate::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Binds [`strider_target::BuiltCallingConvention::entry_address`] to
    /// the entry address, so a TOC / GOT prologue folds, then each
    /// `entry_values` register to its value; at a loop-header entry, only
    /// the phi's entry-edge operand.
    pub(crate) fn bind_entry_registers(&mut self) -> Result<()> {
        let entry = self.entry_machine_addr();
        if let Some(vn) = self.builder.function().default_cc().entry_address {
            self.bind_entry_register(vn, entry)?;
        }
        for &(vn, value) in self.entry_values {
            self.bind_entry_register(vn, value)?;
        }
        Ok(())
    }

    fn bind_entry_register(&mut self, vn: rsleigh::Vn, value: u64) -> Result<()> {
        use strider_ir::{IRBuilderExt as _, IRViewer as _, VnTypeExt as _};
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
            let value = s.builder.build_int_const(u128::from(value), ty)?;
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

    /// Records each argument register's entry value as the carrier of its ABI
    /// position: integer carriers are containers; float carriers leave a gap
    /// for an untracked register and take their own slice of a shared
    /// container (`d0`/`d1` in `q0`).
    pub(crate) fn record_register_arg_carriers(&mut self) -> Result<()> {
        self.builder.record_register_arg_carriers();
        let float_slots = {
            let function = self.builder.function();
            function
                .default_cc()
                .float_arg_slots(function.all_vns(), |v| self.container_map.container_of(v))
        };
        // So a materialised slice carries a fingerprint.
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
