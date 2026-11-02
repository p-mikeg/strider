//! Classifies the placeholder targets the lifter inserts at `BranchIndirect`
//! sites.
//!
//! [`SiteClassifier::classify`] tries an ordered list of sound recognisers, first match
//! wins: a literal `IntConst` address, an `InitialVar(lr)` return, a
//! `Load(RAM)` return that [`ReturnTargets::returns_to_caller`] follows back
//! to the saved link register, then `table::classify_dispatch_value` for
//! rodata jump tables and on-stack label arrays.  No match leaves the target
//! unresolved.
//!
//! Known limits of the field-index rule, seen only in crafted or hand-written
//! asm, with no compiler shape found: a range a read guard narrows is trusted
//! as the table's bound, though an unread test (a mask, a carry, a test
//! looping back) or a guard a later truncate loses may be tighter; and a test
//! reading the cell through a phi reads no bits of it. Each seats cells the
//! code rules out.

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

/// `classify` on a classifier built for `function` alone; `classify_sites` is
/// the production entry, which shares one across the function's sites.
#[cfg(any(test, feature = "test-util"))]
pub fn classify_target(
    function: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
) -> Option<ResolvedTargets> {
    SiteClassifier::new(function, rom).classify(branch)
}

/// One function's dispatch-site recogniser.
///
/// Holds the ranges, which hold the function too, so no caller can pair a
/// function with another one's ranges, nor ask about a site with ranges
/// scoped to a different one. The memos every site's queries share live here
/// as well: the abstract evaluator's memory index, and the claims the return
/// verifier has settled.
///
/// # Soundness
///
/// Every recogniser must identify the runtime target set UNAMBIGUOUSLY and
/// fails closed on a partial proof, never under-approximating.  `Load(sp)` for
/// a `pop pc`-style return, which a `push X; pop pc` tail call shares the shape
/// of, is classified only where [`ReturnTargets`] follows the slot back to a
/// store of the entry link register.  That walk honours a call-boundary claim
/// of its own (`return_slots_survive_untracked_writes`), and it is the
/// orchestrator that keeps the answer honest: the seated `Return` carries a
/// return site, so every round re-checks the seat against its own IR. A
/// verdict that rests on that claim is not reported; analysing again with the
/// claim cleared is how a caller audits one.
///
/// Of the function's claims ([`strider_ir::Function::assumptions`]) the table
/// walk's own memory queries honour `stack_global_disjoint` and the allocation
/// identity `noalias_allocators` gives `decompose`, and no call-boundary
/// relaxation (`Policy::strict`).  That keeps the relaxations out of
/// the walk's own steps, not off the edge: every recogniser
/// reads the IR the pipeline left, whose `LoadForward` forwards across a call
/// under `escape_analysis`, `callee_preserves_stack_args` or a listed
/// allocator.  A dispatch value forwarded that way arrives here folded, so a
/// claim that is wrong for the input seats a wrong edge.
pub(crate) struct SiteClassifier<'f> {
    ranges: index_range::RangeMap<'f>,
    eval: eval::Evaluator<'f>,
    returns: ReturnTargets<'f>,
    /// Per value, the `Add(value, k)` siblings [`index_range::RangeMap::may_bound_sum`]
    /// admits, with each `k`.
    sums: rustc_hash::FxHashMap<ValueId, Vec<(ValueId, u128)>>,
}

impl<'f> SiteClassifier<'f> {
    /// The recogniser for `function`'s dispatch sites, with nothing computed
    /// for any of them yet.  `rom` is the binary's read-only image, consulted
    /// only by the rodata jump-table shape; `None` disables it.
    pub(crate) fn new(
        function: &'f strider_ir::Function,
        rom: Option<&'f dyn ReadOnlyMemory>,
    ) -> Self {
        Self {
            ranges: index_range::RangeMap::new(function),
            eval: eval::Evaluator::new(function, rom),
            returns: ReturnTargets::new(function),
            sums: rustc_hash::FxHashMap::default(),
        }
    }

    pub(crate) fn function(&self) -> &'f strider_ir::Function {
        self.ranges.function()
    }

    /// The recognisers, in order, against the dispatch value of `site`, an
    /// `IndirectBranch` placeholder or a seated `Switch`: a literal `IntConst`
    /// address, an `InitialVar(lr)` return, a `Load(RAM)` return
    /// [`ReturnTargets::returns_to_caller`] follows back to the saved link
    /// register, then the table arm.  `None` when the dispatch value matches
    /// no known sound shape, which defers the site.
    ///
    /// A seated `Switch` is re-derived from its retained selector, which reads
    /// as a wider table once a loop back-edge through its own arms closes, or
    /// folds to the one constant it jumps to; it carries the mode its
    /// instruction commits, so a widened arm is evaluated in its own mode. It
    /// was seated from a table, so it is no return.
    #[must_use]
    pub(crate) fn classify(&mut self, site: NodeId) -> Option<ResolvedTargets> {
        let f = self.function();
        let switch = matches!(f.node_kind(site), NodeKind::Switch(_));
        // The ISA mode the site's instruction committed for its target(s),
        // when it is an interworking branch; carried on the site as a live
        // input and evaluated per resolved target below.
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

    /// The literal target of an `IntConst` dispatch value.  A literal is
    /// always a deterministic function of the pcode, so it is the only
    /// possible target.
    fn single_const_target(
        &self,
        target_value: ValueId,
        mode_value: Option<ValueId>,
    ) -> Option<ResolvedTargets> {
        let function = self.function();
        // `int_const_u128` is `Some` only off an `IntConst`.
        let k = function.int_const_u128(target_value)?;
        // A committed interworking mode MUST fold to a constant here; if it
        // does not, fail closed (`?` defers the whole branch) rather than
        // decode the const target in the flowing mode. Mirrors the table path
        // (table.rs).
        let isa_bit = match mode_value {
            Some(mv) => Some(function.int_const_u128(mv)? != 0),
            None => None,
        };
        // Reject rather than truncate a const with its high 64 bits set.
        Some(ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(
            u64::try_from(k).ok()?,
            isa_bit,
        )))
    }

    /// `None` on stack-push ABIs (x86 / x86_64), which have no link register.
    ///
    /// A single-register pop into the PC (ARM `ldr pc,[sp],#4`) lifts to a
    /// `BranchIndirect` over a stack load, not a `Return`, and once a call
    /// sits between the push and the pop nothing folds that load back to the
    /// link register.  [`ReturnTargets`] is what separates it from the
    /// `push X; pop pc` tail call it looks like.
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

/// The value under an interworking mask, else `value` unchanged.
///
/// `BXWritePC` / `JXWritePC` emit `target & ~1`: the cleared bit is the ISA
/// mode, which the branch carries separately, so the mask hides the
/// `InitialVar(lr)` shape under it.  Only the exact low-bit mask is stripped;
/// any other constant is real arithmetic on the target.
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
    /// Every live dispatch site of `f`, none resolved: what a caller that does
    /// not classify reports.
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

/// Classifies every live `IndirectBranch` placeholder, and re-derives every
/// live `Switch` from its retained selector, under the claims `f` holds.
/// Reads `f` only.
///
/// A dispatch value only becomes classifiable once the optimizer has folded
/// it into a recognizable shape, so `f` is the converged function.
///
/// # Errors
///
/// None; the `Result` is the shape of the orchestrator's classifier hook.
pub fn classify_sites(
    f: &strider_ir::Function,
    rom: Option<&dyn ReadOnlyMemory>,
) -> crate::Result<ClassifiedSites> {
    let mut out = ClassifiedSites::unresolved(f);
    // Most functions have neither kind, so the setup below is skipped then.
    if out.sites.is_empty() {
        return Ok(out);
    }
    // Built once, since the graph does not change here: the ranges, the
    // evaluator's memory index, and the return verifier's settled claims all
    // carry from site to site.
    let mut classifier = SiteClassifier::new(f, rom);
    for &node in &out.sites {
        if let Some(targets) = classifier.classify(node) {
            out.resolved.insert(node, targets);
        }
    }
    Ok(out)
}
