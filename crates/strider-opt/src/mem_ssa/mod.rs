//! Nearest-clobber queries over the memory graph.
//!
//! [`layout`] answers by climbing the memory dominator tree over an index built
//! once per graph state.  [`find_nearest_clobber`] answers the same question
//! with no index, by walking the paths back from the start; it is what a caller
//! without a layout, or a graph the layout does not cover, gets.
//!
//! The walk's MemPhi join rule: resolve each predecessor independently.  If all
//! arms agree (all clean, or all naming the same definition) the merge is
//! transparent and the shared result passes through.  If they disagree there is
//! no single live definition across the merge, so the `MemPhi` itself becomes
//! the clobber boundary.
//!
//! Results are memoized per memory-output rather than guarded by a visited
//! set: two phi arms can share a subchain, and a visited set would let only
//! the first arm resolve it and hand the second a spurious "clean", i.e. a
//! false disagreement.  A node still on the resolution path is marked
//! `InProgress`; re-encountering it is a back-edge, resolved as `Cycle` (top).
//! `Cycle` is kept distinct from a genuine `Clean` bottom-out.
//!
//! Dropping a `Cycle` is sound only under the ancestor it cycled to, so every
//! memo entry carries that ancestor and a reader outside it takes the
//! conservative answer instead.
//!
//! An entry that dropped no `Cycle` and took no conservative answer is carried
//! to the next query by [`WalkAnswers`], which prunes its subtree there.  Every
//! recalled answer is sound, but recall is not answer-preserving: it reports no
//! `Cycle`, so a back-edge arm an earlier query resolved on its own votes as a
//! real answer and can turn a transparent `MemPhi` into a clobber boundary.
//! An answer is therefore usually deeper than a cold per-call walk's and
//! sometimes shallower, and depends on which starts were queried before it.
//! "Deeper" is the CONSERVATIVE direction here -- a clobber boundary found
//! earlier rather than later -- so neither outcome is unsound; what varies is
//! how much a query folds, not whether its answer holds.

mod layout;

#[cfg(test)]
pub(crate) use layout::CLIMB_STEPS;
pub(crate) use layout::{Answers, ClobberProbe, MemLayout, Shape};

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

/// One location's carried walk answers by memory value.  Valid while no def's
/// verdict for the location changes, the same window as [`Answers`].  Every
/// answer is sound; none is guaranteed to be the per-call walk's.
///
/// Without it every query re-walks its own chain, so `L` loads over `S` defs
/// cost `L * S`.
#[derive(Default)]
pub(crate) struct WalkAnswers {
    nearest: FxHashMap<ValueId, NodeId>,
}

/// Nearest clobbering definition backward from `mem`'s memory output, the
/// `InitialMemory` node when every path is clean, or `mem` itself when every
/// path cycles without reaching either.  Read-only.
///
/// `def_clobbers` answers whether a def overlaps the location being analysed.
/// It is never asked about a `MemPhi` or `InitialMemory`; the walk handles
/// those structurally.  Answer `true` for a producer you cannot reason about:
/// `true` terminates the branch with that def as the nearest clobber, `false`
/// advances past it to its own memory input.
///
/// `answers` must hold the answers of this `def_clobbers`, i.e. of one location
/// under one set of options.
pub(crate) fn find_nearest_clobber(
    function: &Function,
    mem: NodeId,
    def_clobbers: &mut dyn FnMut(&Function, NodeId) -> bool,
    answers: &mut WalkAnswers,
) -> NodeId {
    MemSsaWalk {
        function,
        def_clobbers,
    }
    .nearest_clobber(mem, answers)
}

/// Repoints a `Load`'s memory input onto `clobber`'s memory output, skipping
/// the proven-disjoint defs in between.  A non-`Load` handle is left alone.
///
/// Sound only for a `Load`: it is a pure consumer with no memory output, so
/// moving its single incoming memory edge is invisible to every other node.
/// Idempotent, and monotone across fixed-point iterations (verdicts only ever
/// move MayAlias to Disjoint).  Returns whether the edge moved.
pub(crate) fn narrow_load_to(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    clobber: NodeId,
) -> bool {
    let rewire = {
        let function = edit.function();
        if matches!(function.node_kind(load), NodeKind::Load(_)) {
            let target_mem = function
                .memory_output_of(clobber)
                .expect("a clobber node has a memory output");
            let mem_use = function
                .node_input_id_at(load, 0)
                .expect("a Load has a memory input at slot 0");
            let cur_mem = function.node_inputs(load)[0];
            (cur_mem != target_mem).then_some((mem_use, target_mem))
        } else {
            None
        }
    };
    if let Some((mem_use, target_mem)) = rewire {
        edit.update_input(mem_use, target_mem);
    }
    rewire.is_some()
}

/// The resolution of one memory node for the probed slot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// A def that clobbers the slot reaches here.
    Clobber(ValueId),
    /// Every path bottoms out clean at `InitialMemory`: the slot is
    /// uninitialised here.  A real value in a join.
    Clean,
    /// A back-edge to an ancestor still on the resolution path.  The loop found
    /// no clobber for the slot before cycling, so this arm carries no value and
    /// is a don't-care (top) in a MemPhi join.  Distinct from `Clean`, and it must
    /// propagate up through intervening non-clobbering nodes so a loop-header
    /// MemPhi can recognise its own back-edge rather than mistake the cycle for
    /// a genuine `Clean`.
    Cycle,
}

/// One node on the resolution path, ordered by when the walk entered it.
/// Deeper is larger.
#[derive(Clone, Copy)]
struct Dep {
    value: ValueId,
    order: u32,
}

/// A [`Reach`] and the assumption it rests on.
#[derive(Clone, Copy)]
struct Resolved {
    reach: Reach,
    /// The deepest ancestor whose `Cycle` this result dropped, still open when
    /// it was computed.  `None` makes the entry unconditional.
    dep: Option<Dep>,
    /// Whether the result took, or forwards, the conservative answer a re-read
    /// of a closed `dep` gives.  Sound, but tied to this walk's order, so no
    /// later query recalls it.
    approximate: bool,
}

/// Keeps `dep` at the deepest of the ancestors seen so far.
fn deepen(dep: &mut Option<Dep>, candidate: Dep) {
    match *dep {
        Some(d) if d.order >= candidate.order => {}
        _ => *dep = Some(candidate),
    }
}

/// Transparent iff every non-`Cycle` arm names the same definition.
///
/// A `Cycle` arm carries no value: the path around the loop reached an ancestor
/// still being resolved, and that ancestor's own join is where the value it
/// carries is accounted for.  Dropping it is therefore sound only under that
/// ancestor, which is what [`Resolved::dep`] records.
fn join_phi_results(phi_value: ValueId, preds: &[Reach]) -> Reach {
    let mut non_cycle = preds.iter().copied().filter(|r| !matches!(r, Reach::Cycle));
    let Some(first) = non_cycle.next() else {
        // Every arm cycled (a degenerate all-back-edge merge), or the MemPhi has
        // zero arms (a control-dead Region `CfgDetach` culls before any consumer
        // walks it).  No information.
        return Reach::Cycle;
    };
    if non_cycle.all(|r| r == first) {
        first
    } else {
        Reach::Clobber(phi_value)
    }
}

/// An absent key means "not yet entered on this walk".
#[derive(Clone, Copy)]
enum Resolve {
    /// On the current resolution path, at the given entry order;
    /// re-encountering it is a cycle.
    InProgress(u32),
    Done(Resolved),
}

enum Frame {
    /// First visit: classify, then push an `Exit` continuation and successors.
    /// Carries the DFS parent, the deepest ancestor a dropped cycle can name
    /// once the node discharges its own.
    Enter(ValueId, Option<Dep>),
    /// Successors resolved; combine and memoize.
    Exit(ValueId, Option<Dep>),
}

struct MemSsaWalk<'f, 'w> {
    function: &'f Function,
    def_clobbers: &'w mut dyn FnMut(&Function, NodeId) -> bool,
}

impl MemSsaWalk<'_, '_> {
    fn nearest_clobber(&mut self, mem: NodeId, answers: &mut WalkAnswers) -> NodeId {
        let start_mem = self
            .function
            .memory_output_of(mem)
            .expect("memory-chain start node has a memory output");
        if let Some(&answer) = answers.nearest.get(&start_mem) {
            return answer;
        }
        let mut initial_memory: Option<NodeId> = None;
        match self.walk_from(start_mem, &mut initial_memory, answers) {
            Some(clobber_value) => self.function.producer(clobber_value),
            // No clobber and no clean bottom either: every path cycled, so
            // nothing is proven and the start is the only def-free answer.
            None => initial_memory.unwrap_or(mem),
        }
    }

    /// Walks back from `start_mem`, publishing every start-independent result
    /// into `answers` so the next query stops at it instead of re-walking.
    fn walk_from(
        &mut self,
        start_mem: ValueId,
        initial_memory: &mut Option<NodeId>,
        answers: &mut WalkAnswers,
    ) -> Option<ValueId> {
        // A hash map, not a dense entity-keyed `SecondaryMap`: one walk
        // touches only O(chain) of the function's ValueIds, so a per-walk
        // O(function) init would dominate.
        let mut memo: FxHashMap<ValueId, Resolve> = FxHashMap::default();
        let mut work: Vec<Frame> = vec![Frame::Enter(start_mem, None)];
        let mut next_order: u32 = 0;

        while let Some(frame) = work.pop() {
            match frame {
                Frame::Enter(cur, parent) => {
                    // Already seen: `Done` reuses the memoised result (DAG
                    // fan-in); `InProgress` stays as-is so the consuming
                    // `combine` reads `Cycle`, i.e. the cycle adds no clobber.
                    if memo.contains_key(&cur) {
                        continue;
                    }
                    // A recalled answer holds whatever the start was, so it ends
                    // the branch here.
                    if let Some(reach) = self.recall(answers, cur, initial_memory) {
                        memo.insert(
                            cur,
                            Resolve::Done(Resolved {
                                reach,
                                dep: None,
                                approximate: false,
                            }),
                        );
                        continue;
                    }
                    let node = self.function.producer(cur);
                    let node_kind = self.function.node_kind(node);
                    let is_phi = matches!(node_kind, NodeKind::MemPhi);
                    let is_initial = matches!(node_kind, NodeKind::InitialMemory);
                    if is_initial {
                        // Remember the clean root so the caller can name it.
                        *initial_memory = Some(node);
                    }
                    if !is_phi && !is_initial && (self.def_clobbers)(self.function, node) {
                        memo.insert(
                            cur,
                            Resolve::Done(Resolved {
                                reach: Reach::Clobber(cur),
                                dep: None,
                                approximate: false,
                            }),
                        );
                        continue;
                    }
                    let cur_dep = Dep {
                        value: cur,
                        order: next_order,
                    };
                    next_order += 1;
                    memo.insert(cur, Resolve::InProgress(cur_dep.order));
                    work.push(Frame::Exit(cur, parent));
                    for succ in self.successors(cur) {
                        work.push(Frame::Enter(succ, Some(cur_dep)));
                    }
                }
                Frame::Exit(cur, parent) => {
                    let mut dep: Option<Dep> = None;
                    let mut approximate = false;
                    let succ_results: SmallVec<[Reach; 4]> = self
                        .successors(cur)
                        .into_iter()
                        .map(|s| match memo.get(&s).copied() {
                            // Still open: a back-edge to an ancestor on this
                            // path, which contributes no value.
                            Some(Resolve::InProgress(order)) => {
                                deepen(&mut dep, Dep { value: s, order });
                                Reach::Cycle
                            }
                            Some(Resolve::Done(r)) => match r.dep {
                                None => {
                                    approximate |= r.approximate;
                                    r.reach
                                }
                                // Tagging each entry with the ancestor it
                                // assumed away, instead of refusing to memoise
                                // it, is what keeps the walk linear: a reader
                                // outside that ancestor takes the conservative
                                // answer in O(1) where a re-walk would be
                                // exponential in the loop nesting.
                                Some(d)
                                    if !matches!(
                                        memo.get(&d.value),
                                        Some(Resolve::InProgress(_))
                                    ) =>
                                {
                                    approximate = true;
                                    Reach::Clobber(s)
                                }
                                Some(d) => {
                                    deepen(&mut dep, d);
                                    approximate |= r.approximate;
                                    r.reach
                                }
                            },
                            None => {
                                unreachable!("every successor is entered before this Exit frame")
                            }
                        })
                        .collect();
                    let reach = self.combine(cur, &succ_results);
                    // A cycle back to `cur` is discharged here: `cur` is the
                    // loop header, so its own join accounts for it.  What the
                    // arm assumed ABOVE `cur` is no longer recoverable, and the
                    // DFS parent is the tightest bound on it.
                    let dep = match dep {
                        Some(d) if d.value == cur => parent,
                        other => other,
                    };
                    memo.insert(
                        cur,
                        Resolve::Done(Resolved {
                            reach,
                            dep,
                            approximate,
                        }),
                    );
                }
            }
        }

        let root = match memo.get(&start_mem).copied() {
            Some(Resolve::Done(r)) => r,
            _ => unreachable!("the start resolves before the work list empties"),
        };
        self.publish(&memo, root, *initial_memory, answers);
        match root.reach {
            Reach::Clobber(v) => Some(v),
            // Clean or Cycle: no clobber reached, so the caller names
            // `InitialMemory`.
            Reach::Clean | Reach::Cycle => None,
        }
    }

    /// The answer `answers` holds for `cur`, as a [`Reach`].
    fn recall(
        &self,
        answers: &WalkAnswers,
        cur: ValueId,
        initial_memory: &mut Option<NodeId>,
    ) -> Option<Reach> {
        let &answer = answers.nearest.get(&cur)?;
        if matches!(self.function.node_kind(answer), NodeKind::InitialMemory) {
            *initial_memory = Some(answer);
            return Some(Reach::Clean);
        }
        Some(Reach::Clobber(
            self.function
                .memory_output_of(answer)
                .expect("a recalled clobber is a memory def"),
        ))
    }

    /// Carries one walk's carriable results into `answers`: the entries that
    /// dropped no `Cycle` and took no conservative answer, plus, when the start
    /// bottomed out `Clean`, every clean node in its cone, since a clobber
    /// anywhere in a cone reaches the start as one.  A `Cycle` start is
    /// excluded: it reached no clobber either, but only under the ancestor it
    /// cycled to.
    fn publish(
        &self,
        memo: &FxHashMap<ValueId, Resolve>,
        root: Resolved,
        initial_memory: Option<NodeId>,
        answers: &mut WalkAnswers,
    ) {
        let clean_cone = matches!(root.reach, Reach::Clean);
        for (&v, entry) in memo {
            let Resolve::Done(r) = entry else { continue };
            if r.approximate {
                continue;
            }
            let answer = match r.reach {
                Reach::Clobber(c) if r.dep.is_none() => self.function.producer(c),
                Reach::Clean if r.dep.is_none() || clean_cone => {
                    let Some(initial) = initial_memory else {
                        continue;
                    };
                    initial
                }
                _ => continue,
            };
            answers.nearest.insert(v, answer);
        }
    }

    fn successors(&self, cur: ValueId) -> SmallVec<[ValueId; 4]> {
        let node = self.function.producer(cur);
        match *self.function.node_kind(node) {
            NodeKind::MemPhi => self.function.phi_data_inputs(node).collect(),
            NodeKind::InitialMemory => SmallVec::new(),
            _ => self.function.memory_input_of(node).into_iter().collect(),
        }
    }

    /// The alias verdict short-circuits at enter-time, so a non-phi node
    /// only forwards.
    fn combine(&self, cur: ValueId, succ_results: &[Reach]) -> Reach {
        let node = self.function.producer(cur);
        match *self.function.node_kind(node) {
            NodeKind::MemPhi => join_phi_results(cur, succ_results),
            // A non-phi node forwards its single memory input's result,
            // propagating `Cycle` up so a loop-header MemPhi above recognises its
            // back-edge.  No successor means `InitialMemory`: clean.
            _ => succ_results.first().copied().unwrap_or(Reach::Clean),
        }
    }
}

#[cfg(test)]
mod tests;
