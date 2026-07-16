//! Test-only [`FunctionBuilder`] conveniences, quarantined out of the
//! production builder source.
//!
//! These build an ad-hoc IR function *eagerly* — a value `Phi` for every
//! tracked varnode, current values pre-seeded — so a hand-written test graph
//! resolves every variable read WITHOUT running the pruned-SSA
//! dominance/inheritance walk the lifter performs.  Production lifting uses the
//! single pruned flow ([`FunctionBuilder::create_region`] +
//! [`FunctionBuilder::set_entry_region`]); this whole module is `#[cfg]`-gated
//! out of production.
//!
//! They live inside `strider-ir` rather than the `strider-ir-test-utils`
//! dev-dep crate because `strider-ir`'s OWN `#[cfg(test)]` modules call them:
//! under `cargo test` the dev-dep links a *separate, non-test* compilation of
//! `strider-ir`, so an extension trait `impl … for FunctionBuilder` defined in
//! test-utils targets a different `FunctionBuilder` than the unit tests build
//! and would not resolve (the same wall `edit::test_fixtures` documents).

use super::FunctionBuilder;
use crate::error::Result;
use crate::region::RegionId;

impl FunctionBuilder {
    /// Create a region carrying a value `Phi` for EVERY tracked varnode (with
    /// its current value seeded), so an ad-hoc test function's regions resolve
    /// every variable read WITHOUT the pruned-SSA dominance/inheritance walk.
    /// Production passes an explicit Cytron IDF-placed set to
    /// [`FunctionBuilder::create_region`]; this all-variables form is the
    /// test-only counterpart.
    ///
    /// # Errors
    ///
    /// Propagates [`FunctionBuilder::create_region`]'s errors.
    pub fn create_region_all(&mut self) -> Result<RegionId> {
        let vns: Vec<_> = self.function().vn_ids().collect();
        self.create_region(&vns)
    }

    /// Entry setup pairing with [`Self::create_region_all`]: like
    /// [`FunctionBuilder::set_entry_region`], but does NOT seed the
    /// current-value map with the `InitialVar`s.  The eager builder placed a
    /// value `Phi` for every variable at the entry region (seeding the phis as
    /// current values), so an entry read resolves to its phi — whose operand
    /// this wires to the `InitialVar` — WITHOUT the pruned dominance/inheritance
    /// walk.
    ///
    /// # Errors
    ///
    /// Same as [`FunctionBuilder::set_entry_region`].
    pub fn set_entry_region_all(&mut self, region_id: RegionId) -> Result<()> {
        let initial_variables = self.wire_entry_and_build_initial_vars(region_id)?;
        self.link_region_variables(region_id, &initial_variables)
    }

    /// Record register-passed argument carriers on the arg table, mirroring what
    /// the LIFTER does in prod right after [`FunctionBuilder::set_entry_region`].
    ///
    /// Each arg-passing register resolves to its largest tracked container (via
    /// [`vn_container::largest_container_in`] over `all_vns` — the same
    /// containment rule the lifter's `container_of` map applies), and that
    /// container's `InitialVar` value is registered as the carrier for the
    /// argument's positional index.  Direct-builder tests (no lifter) call this
    /// after `set_entry_region` to reproduce the prod arg table.
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
