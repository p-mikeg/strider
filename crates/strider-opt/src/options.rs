use strider_ir::AssumptionOptions;

/// Per-run knobs.
#[derive(Debug, Clone)]
pub struct OptOptions {
    /// Run the indirect-branch classifier.  Off leaves every unseeded site an
    /// `IndirectBranch` placeholder, so a caller can hand its own answers in
    /// through `CfgOptions::known_targets` instead, or see the raw dispatch
    /// shape when a resolution looks wrong.  Caller-supplied targets still
    /// seat: they are read by the CFG builder, not the classifier.
    pub resolve_indirect_branches: bool,
    /// Claims about the program under analysis.
    pub assumptions: AssumptionOptions,
}

impl Default for OptOptions {
    fn default() -> Self {
        Self {
            // The only non-`Default` field: resolution is on unless a caller
            // turns it off.
            resolve_indirect_branches: true,
            assumptions: AssumptionOptions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OptOptions;

    /// The pipeline's own defaults, which are NOT the sound floor.  A test
    /// reaches for the floor through `OptCtx::at_floor` instead.
    #[test]
    fn default_leaves_the_pipeline_claims_on() {
        let o = OptOptions::default();
        assert!(o.resolve_indirect_branches);
        assert!(o.assumptions.stack_global_disjoint);
        assert!(o.assumptions.assume_incoming_args_survive_calls);
        assert!(o.assumptions.return_slots_survive_untracked_writes);
        assert!(!o.assumptions.escape_analysis);
    }
}
