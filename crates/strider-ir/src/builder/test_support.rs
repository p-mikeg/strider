//! Eager builders: a `Phi` for every tracked varnode, so a hand-written test
//! graph resolves reads without the lifter's pruned-SSA walk. Not in
//! `strider-ir-test-utils`, whose `FunctionBuilder` is a different type under
//! `cargo test`.

use super::FunctionBuilder;
use crate::Result;
use crate::region::RegionId;

impl FunctionBuilder {
    /// Creates a region carrying a phi for every tracked varnode.
    pub fn create_region_all(&mut self) -> Result<RegionId> {
        let vns: Vec<_> = self.function().vn_ids().collect();
        self.create_region(&vns)
    }

    /// Wires `region_id` as the entry without seeding the `InitialVar`s, so
    /// an entry read resolves to the region's phi.
    pub fn set_entry_region_all(&mut self, region_id: RegionId) -> Result<()> {
        let initial_variables = self.wire_entry_and_build_initial_vars(region_id)?;
        self.link_region_variables(region_id, &initial_variables)
    }
}
