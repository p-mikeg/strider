//! These build a function eagerly: a value `Phi` for every tracked varnode
//! with current values pre-seeded, so a hand-written test graph resolves each
//! variable read without the pruned-SSA dominance walk the lifter runs.
//!
//! They cannot live in the `strider-ir-test-utils` dev-dep: under `cargo
//! test` it links a separate compilation of `strider-ir`, so its
//! `FunctionBuilder` is a different type from the unit tests'.

use super::FunctionBuilder;
use crate::error::Result;
use crate::region::RegionId;

impl FunctionBuilder {
    /// Creates a region carrying a phi for every tracked varnode.
    pub fn create_region_all(&mut self) -> Result<RegionId> {
        let vns: Vec<_> = self.function().vn_ids().collect();
        self.create_region(&vns)
    }

    /// Wires `region_id` as the entry WITHOUT seeding the current-value map
    /// with the `InitialVar`s, so an entry read resolves to the region's phi.
    pub fn set_entry_region_all(&mut self, region_id: RegionId) -> Result<()> {
        let initial_variables = self.wire_entry_and_build_initial_vars(region_id)?;
        self.link_region_variables(region_id, &initial_variables)
    }
}
