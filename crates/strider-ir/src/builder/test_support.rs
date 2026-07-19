//! Test-only [`FunctionBuilder`] conveniences, `#[cfg]`-gated out of prod.
//!
//! These build a function eagerly: a value `Phi` for every tracked varnode
//! with current values pre-seeded, so a hand-written test graph resolves each
//! variable read without the pruned-SSA dominance walk the lifter runs.
//!
//! They live here rather than in the `strider-ir-test-utils` dev-dep because
//! `strider-ir`'s own `#[cfg(test)]` modules call them. Under `cargo test` the
//! dev-dep links a separate, non-test compilation of `strider-ir`, so an
//! extension trait defined there would target a different `FunctionBuilder`
//! than the unit tests build and fail to resolve.

use super::FunctionBuilder;
use crate::error::Result;
use crate::region::RegionId;

impl FunctionBuilder {
    /// The test-only counterpart to [`FunctionBuilder::create_region`], which
    /// production calls with an explicit IDF-placed set.
    pub fn create_region_all(&mut self) -> Result<RegionId> {
        let vns: Vec<_> = self.function().vn_ids().collect();
        self.create_region(&vns)
    }

    /// Pairs with [`Self::create_region_all`]. Unlike
    /// [`FunctionBuilder::set_entry_region`] it does NOT seed the
    /// current-value map with the `InitialVar`s: the eager builder already
    /// seeded the entry region's phis as current values, so an entry read
    /// resolves to its phi, whose operand this wires to the `InitialVar`.
    pub fn set_entry_region_all(&mut self, region_id: RegionId) -> Result<()> {
        let initial_variables = self.wire_entry_and_build_initial_vars(region_id)?;
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Mirrors what the lifter does right after
    /// [`FunctionBuilder::set_entry_region`], for tests that build directly.
    ///
    /// Resolves each arg-passing register to its largest tracked container
    /// under the same containment rule the lifter's `container_of` applies,
    /// then registers that container's `InitialVar` as the carrier for the
    /// argument's positional index.
    pub fn record_register_arg_carriers(&mut self) {
        let arg_regs: Vec<rsleigh::Vn> = self.function.default_cc().arg_passing_regs.clone();
        for (i, reg) in arg_regs.iter().enumerate() {
            let container = vn_container::largest_container_in(self.function.all_vns(), reg);
            if let Some(value) = self.function.initial_var_value(&container) {
                self.function_mut()
                    .side_tables_mut()
                    .register_arg_value(i as u32, value);
            }
        }
    }
}
