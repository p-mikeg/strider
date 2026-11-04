mod claims;
mod feasible;
mod stack;

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, ValueId};
use strider_ir::{CtrlKey, DominatorTree, Function};

/// Verifies that a function's return sites jump to the address it was entered
/// with: the entry link register, or on a stack-push ISA the entry-SP slot a
/// `call` pushed it into.  Saved return addresses and saved stack pointers are
/// followed back through the stores that saved them.
///
/// A RAM store not placed on the entry stack, and any callee or user-op, leave
/// the slot intact under
/// [`AssumptionOptions::return_slots_survive_untracked_writes`](crate::AssumptionOptions::return_slots_survive_untracked_writes).
/// Without it, such a store is located the way a load is and clobbers the slot
/// unless a held claim puts it off the stack (`stack_global_disjoint`, a listed
/// allocator).
pub struct ReturnTargets<'f> {
    function: &'f Function,
    /// Every claim a query has decided, shared across sites.
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
    /// The verifier for `function`'s return sites under its claims.
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
    /// A claim holds unless one it rests on fails, so a cycle of claims (a
    /// loop rewriting a slot with its own value) holds.  Linear in the claims
    /// reached.
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
        // The offset from the entry SP each value is claimed at, per path;
        // first wins. Per query, so a failed probe's offset does not refuse
        // the right one at the next site.
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
