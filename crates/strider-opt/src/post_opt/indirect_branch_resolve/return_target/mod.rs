//! Whether a return instruction jumps back to the address the function was
//! entered with.
//!
//! Here: the claim types and the fixpoint over them.  [`claims`] holds the
//! rule per claim, [`feasible`] which phi arm control can have taken, and
//! [`stack`] the stack-memory engine the rules probe.

mod claims;
mod feasible;
mod stack;

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, ValueId};
use strider_ir::{CtrlKey, DominatorTree, Function};

/// Answers, for the address each Sleigh `RETURN` op of one function jumps to,
/// whether it is the one the function was entered with: the entry link
/// register, or on a stack-push ISA the entry-SP slot a `call` pushed it into.
///
/// Read off the optimised IR, so a return address saved to the stack and
/// restored is followed back to the store that saved it, and a stack pointer
/// saved in a realigned frame back to the store that saved it. `false` when
/// that cannot be shown, and the site is then an indirect branch.
///
/// A RAM store the address spine does not place on the entry stack, and any
/// callee or user-op, rest on one claim,
/// [`AssumptionOptions::return_slots_survive_untracked_writes`](crate::AssumptionOptions::return_slots_survive_untracked_writes):
/// code that breaks it smashes its own stack. Without it, such a store's
/// address is located the way a load's is, and a store no location is reached
/// for is taken to clobber the slot, unless a claim the function holds puts it
/// off the stack: a constant address under `stack_global_disjoint`, or a
/// listed allocator's memory.
pub struct ReturnTargets<'f> {
    function: &'f Function,
    /// Every claim a query has decided. Return sites mostly share their
    /// claims, and each is explored once.
    settled: FxHashMap<Claim, bool>,
    doms: Option<DominatorTree<CtrlKey>>,
    feasible: FxHashMap<(CtrlKey, NodeId), Option<usize>>,
    guesses: FxHashMap<(Path, ValueId), Option<i128>>,
    stored: FxHashMap<(ValueId, StackAddr, i128), Option<ValueId>>,
    guards: FxHashMap<ValueId, Vec<(NodeId, bool)>>,
}

/// Where a claim is observed: `Some(point)` each time control passes `point`,
/// of the values current there, `None` everywhere.
type Path = Option<CtrlKey>;

/// One fact a return target's verification rests on.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Claim {
    /// `value` holds `held` along `path`.
    Holds {
        path: Path,
        value: ValueId,
        held: Held,
    },
    /// In memory state `mem`, the `size` bytes at `at` hold `holds`.
    Slot {
        mem: ValueId,
        at: StackAddr,
        size: i128,
        holds: Held,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Held {
    /// The address the function was entered with.
    ReturnAddress,
    /// The entry SP plus this offset.
    EntrySp(i128),
}

/// `offset` bytes off the entry SP (`base: None`) or off an alignment-masked
/// SP value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct StackAddr {
    base: Option<ValueId>,
    offset: i128,
}

enum StoreEffect {
    Writes(ValueId),
    Clobbers,
    Passes,
}

impl<'f> ReturnTargets<'f> {
    /// The verifier for `function`'s return sites under its claims, with no
    /// claim decided yet; each query reuses the claims earlier ones settled.
    #[must_use]
    pub fn new(function: &'f Function) -> Self {
        Self {
            function,
            settled: FxHashMap::default(),
            doms: None,
            feasible: FxHashMap::default(),
            guesses: FxHashMap::default(),
            stored: FxHashMap::default(),
            guards: FxHashMap::default(),
        }
    }

    /// Whether `target`, the address `ret` jumps to, is the address the
    /// function was entered with.
    ///
    /// A claim holds unless one it rests on fails outright, so a cycle of
    /// claims (a loop rewriting a slot with its own value, a phi of itself)
    /// brings in no value of its own. Linear in the claims reached.
    pub fn returns_to_caller(&mut self, ret: NodeId, target: ValueId) -> bool {
        let root = Claim::Holds {
            path: Some(CtrlKey::Node(ret)),
            value: target,
            held: Held::ReturnAddress,
        };
        let mut index: FxHashMap<Claim, usize> = FxHashMap::default();
        index.insert(root, 0);
        let mut claims = vec![root];
        // Per claim, the claims resting on it.
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new()];
        let mut failed: Vec<usize> = Vec::new();
        let mut premises = Vec::new();
        // The one offset from the entry SP a value is claimed at, per path.
        // Per query: an offset a failed probe wrote would otherwise refuse the
        // right one at the next site. Still first-wins WITHIN the query, so of
        // two contradictory claims on one key the later one fails whichever it
        // is, and `settled` carries that `false` to every later query.
        let mut claimed: FxHashMap<(Path, ValueId), i128> = FxHashMap::default();
        let mut next = 0;
        while let Some(&claim) = claims.get(next) {
            premises.clear();
            if !self.premises(claim, &mut claimed, &mut premises) {
                failed.push(next);
            }
            for &premise in &premises {
                match self.settled.get(&premise) {
                    Some(true) => {}
                    Some(false) => failed.push(next),
                    None => {
                        let at = *index.entry(premise).or_insert_with(|| {
                            claims.push(premise);
                            dependents.push(Vec::new());
                            claims.len() - 1
                        });
                        dependents[at].push(next);
                    }
                }
            }
            next += 1;
        }
        let mut holds = vec![true; claims.len()];
        while let Some(at) = failed.pop() {
            if std::mem::replace(&mut holds[at], false) {
                failed.extend_from_slice(&dependents[at]);
            }
        }
        self.settled.extend(claims.into_iter().zip(holds));
        self.settled[&root]
    }
}
