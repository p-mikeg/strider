//! Classifies the targets of `BranchIndirect` placeholders and seated `Switch`es.

use std::cell::LazyCell;

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

use strider_cfg::ResolvedTargets;

use crate::ReadOnlyMemory;

pub(crate) mod eval;
pub(crate) mod index_range;
mod return_target;
pub(crate) mod table;
#[cfg(test)]
mod tests;

pub use return_target::ReturnTargets;

/// Cap on enumerated table slots: a range narrowed only by a KnownBits stride
/// still spans billions of them.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

/// `SiteClassifier::classify` on a classifier built for `function` alone.
#[cfg(any(test, feature = "test-util"))]
pub fn classify_target(
    function: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
) -> Option<ResolvedTargets> {
    SiteClassifier::new(function, rom).classify(branch)
}

type LazyRanges<'f> =
    LazyCell<index_range::RangeMap<'f>, Box<dyn FnOnce() -> index_range::RangeMap<'f> + 'f>>;

/// One function's dispatch-site recogniser, with the memos its sites share.
///
/// # Soundness
///
/// Every recogniser fails closed on a partial proof, never under-approximating
/// the target set. The IR it reads was forwarded under the function's claims, so
/// a claim wrong for the input can still seat a wrong edge.
pub(crate) struct SiteClassifier<'f> {
    function: &'f strider_ir::Function,
    /// Built on the first table query: a constant or link-register site never
    /// reads it.
    ranges: LazyRanges<'f>,
    eval: eval::Evaluator<'f>,
    returns: ReturnTargets<'f>,
    /// Per value, the `Add(value, k)` siblings [`index_range::RangeMap::may_bound_sum`]
    /// admits, with each `k`.
    sums: rustc_hash::FxHashMap<ValueId, Vec<(ValueId, u128)>>,
}

impl<'f> SiteClassifier<'f> {
    /// The recogniser for `function`'s dispatch sites.  `rom` is the binary's
    /// read-only image, read only by the rodata jump-table shape; `None`
    /// disables it.
    pub(crate) fn new(
        function: &'f strider_ir::Function,
        rom: Option<&'f dyn ReadOnlyMemory>,
    ) -> Self {
        Self {
            function,
            ranges: LazyCell::new(Box::new(move || index_range::RangeMap::new(function))),
            eval: eval::Evaluator::new(function, rom),
            returns: ReturnTargets::new(function),
            sums: rustc_hash::FxHashMap::default(),
        }
    }

    pub(crate) fn function(&self) -> &'f strider_ir::Function {
        self.function
    }

    /// The target set of `site`, an `IndirectBranch` or a seated `Switch`, by
    /// the first recogniser to match its dispatch value: a literal `IntConst`
    /// address, an `InitialVar(lr)` return, a `Load(RAM)` return
    /// [`ReturnTargets::returns_to_caller`] follows back to the saved link
    /// register, then the table arm.  `None` defers the site.
    ///
    /// A `Switch` is re-derived from its retained selector and is never a return.
    #[must_use]
    pub(crate) fn classify(&mut self, site: NodeId) -> Option<ResolvedTargets> {
        let f = self.function();
        let switch = matches!(f.node_kind(site), NodeKind::Switch(_));
        // The ISA mode an interworking branch commits for its targets.
        let (target_value, mode_value) = if switch {
            (f.switch_selector(site), f.switch_isa_mode(site))
        } else {
            (
                f.indirect_branch_target(site),
                f.indirect_branch_isa_mode(site),
            )
        };
        self.single_const_target(target_value, mode_value)
            .or_else(|| {
                (!switch)
                    .then(|| self.link_register_return(site, target_value))
                    .flatten()
            })
            .or_else(|| self.classify_dispatch_value(site, target_value, mode_value))
    }

    /// The literal target of an `IntConst` dispatch value.
    fn single_const_target(
        &self,
        target_value: ValueId,
        mode_value: Option<ValueId>,
    ) -> Option<ResolvedTargets> {
        let function = self.function();
        let k = function.int_const_u128(target_value)?;
        // A committed mode that does not fold defers the branch.
        let isa_bit = match mode_value {
            Some(mv) => Some(function.int_const_u128(mv)? != 0),
            None => None,
        };
        Some(ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(
            u64::try_from(k).ok()?,
            isa_bit,
        )))
    }

    /// A return through the link register, directly or popped from the stack
    /// (ARM `ldr pc,[sp],#4`); [`ReturnTargets`] tells the latter from a
    /// `push X; pop pc` tail call.  `None` on ABIs with no link register.
    fn link_register_return(
        &mut self,
        branch: NodeId,
        target_value: ValueId,
    ) -> Option<ResolvedTargets> {
        let function = self.function();
        let lr = function.default_cc().link_register?;
        match *function.kind_of_value(strip_isa_mode_mask(function, target_value)) {
            NodeKind::InitialVar(id) if function.initial_vn(id) == lr => {
                Some(ResolvedTargets::LinkRegister)
            }
            NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => self
                .returns
                .returns_to_caller(branch, target_value)
                .then_some(ResolvedTargets::LinkRegister),
            _ => None,
        }
    }
}

/// The operand of an exact `target & ~1` interworking mask (`BXWritePC` /
/// `JXWritePC`, whose cleared bit the branch carries separately), else `value`.
fn strip_isa_mode_mask(function: &strider_ir::Function, value: ValueId) -> ValueId {
    let Some((operand, mask)) = crate::const_eval::var_and_const_of(
        function,
        function.producer(value),
        NodeKind::IntBinaryOp(strider_ir::node::IntBinaryOp::And),
    ) else {
        return value;
    };
    let want = function
        .value_type_opt(value)
        .and_then(|ty| ty.int_mask_u128())
        .map(|m| m ^ 1);
    if want == Some(mask) { operand } else { value }
}

/// Every live dispatch site of a function, and what was recovered for each.
#[derive(Debug)]
pub struct ClassifiedSites {
    /// Every reachable `IndirectBranch` placeholder and seated `Switch`.
    pub sites: Vec<NodeId>,
    /// The sites a target set was recovered for.
    pub resolved: rustc_hash::FxHashMap<NodeId, ResolvedTargets>,
}

impl ClassifiedSites {
    /// Every live dispatch site of `f`, none resolved.
    #[must_use]
    pub fn unresolved(f: &strider_ir::Function) -> Self {
        Self {
            sites: f
                .walk_kind(|k| matches!(k, NodeKind::IndirectBranch | NodeKind::Switch(_)))
                .collect(),
            resolved: rustc_hash::FxHashMap::default(),
        }
    }
}

/// Classifies every live `IndirectBranch` and re-derives every live `Switch` of
/// `f`, the optimized function.
///
/// # Errors
///
/// None; the `Result` is the orchestrator's classifier-hook shape.
pub fn classify_sites(
    f: &strider_ir::Function,
    rom: Option<&dyn ReadOnlyMemory>,
) -> crate::Result<ClassifiedSites> {
    let mut out = ClassifiedSites::unresolved(f);
    if out.sites.is_empty() {
        return Ok(out);
    }
    let mut classifier = SiteClassifier::new(f, rom);
    for &node in &out.sites {
        if let Some(targets) = classifier.classify(node) {
            out.resolved.insert(node, targets);
        }
    }
    Ok(out)
}
