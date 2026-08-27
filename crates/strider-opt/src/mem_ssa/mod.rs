//! MemPhi join rule: resolve each predecessor independently.  If all arms
//! agree (all clean, or all naming the same definition) the merge is
//! transparent and the shared result passes through.  If they disagree there
//! is no single live definition across the merge, so the `MemPhi` itself
//! becomes the clobber boundary.
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

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

pub(crate) trait MemorySSAWalker {
    /// Does `def` overlap the location being analysed?
    ///
    /// `def` is never a `MemPhi` or `InitialMemory`; the walk handles those
    /// structurally.  Return `true` for producers you cannot reason about.
    ///
    /// `true` terminates the branch with `def` as the nearest clobber;
    /// `false` advances past `def` to its own memory input.
    fn def_clobbers(&mut self, function: &Function, def: NodeId) -> bool;

    /// Nearest clobbering definition backward from `mem`'s memory output, the
    /// `InitialMemory` node when every path is clean, or `mem` itself when
    /// every path cycles without reaching either.  Read-only.
    fn find_nearest_clobber(&mut self, function: &Function, mem: NodeId) -> NodeId
    where
        Self: Sized,
    {
        MemSsaWalk::new(function, self).nearest_clobber(mem)
    }
}

/// Repoints a `Load`'s memory input onto `clobber`'s memory output, skipping
/// the proven-disjoint defs in between.  A non-`Load` handle is left alone.
///
/// Sound only for a `Load`: it is a pure consumer with no memory output, so
/// moving its single incoming memory edge is invisible to every other node.
/// Idempotent, and monotone across fixed-point iterations (verdicts only ever
/// move MayAlias to Disjoint).
pub(crate) fn narrow_load_to(edit: &mut crate::EditFunction<'_>, load: NodeId, clobber: NodeId) {
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

struct MemSsaWalk<'f, 'w, W: MemorySSAWalker> {
    function: &'f Function,
    walker: &'w mut W,
}

impl<'f, 'w, W: MemorySSAWalker> MemSsaWalk<'f, 'w, W> {
    fn new(function: &'f Function, walker: &'w mut W) -> Self {
        Self { function, walker }
    }

    fn nearest_clobber(&mut self, mem: NodeId) -> NodeId {
        let start_mem = self
            .function
            .memory_output_of(mem)
            .expect("memory-chain start node has a memory output");
        let mut initial_memory: Option<NodeId> = None;
        match self.walk_from(start_mem, &mut initial_memory) {
            Some(clobber_value) => self.function.producer(clobber_value),
            // No clobber and no clean bottom either: every path cycled, so
            // nothing is proven and the start is the only def-free answer.
            None => initial_memory.unwrap_or(mem),
        }
    }

    /// The memo is per-call, so every query re-walks from its own cursor.
    fn walk_from(
        &mut self,
        start_mem: ValueId,
        initial_memory: &mut Option<NodeId>,
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
                    let node = self.function.producer(cur);
                    let node_kind = self.function.node_kind(node);
                    let is_phi = matches!(node_kind, NodeKind::MemPhi);
                    let is_initial = matches!(node_kind, NodeKind::InitialMemory);
                    if is_initial {
                        // Remember the clean root so the caller can name it.
                        *initial_memory = Some(node);
                    }
                    if !is_phi && !is_initial && self.walker.def_clobbers(self.function, node) {
                        memo.insert(
                            cur,
                            Resolve::Done(Resolved {
                                reach: Reach::Clobber(cur),
                                dep: None,
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
                                None => r.reach,
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
                                    Reach::Clobber(s)
                                }
                                Some(d) => {
                                    deepen(&mut dep, d);
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
                    memo.insert(cur, Resolve::Done(Resolved { reach, dep }));
                }
            }
        }

        match memo.get(&start_mem).copied() {
            Some(Resolve::Done(Resolved {
                reach: Reach::Clobber(v),
                ..
            })) => Some(v),
            // Clean or Cycle: no clobber reached, so the caller names
            // `InitialMemory`.
            _ => None,
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
