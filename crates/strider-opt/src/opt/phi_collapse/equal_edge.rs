//! Below the edge on which `If(Equal(Add(phi, c), y))` holds, the `Add` is
//! `y` and `phi` is `y - c`: everything the `If` reads is defined at a point
//! dominating it, so a path to a point the edge dominates crosses the edge
//! after each such definition last runs, and `phi` and `y` are unchanged
//! since.
//!
//! A read moves when its point is dominated by the edge: a control node reads
//! at itself, a phi arm at its predecessor edge, and a floating node at each
//! point its value is read. `y` must be computed from entry values and
//! constants: rooted at this phi, `y - c` reads it, and moving that read
//! closes a data cycle; through a load or another phi, a moved read could
//! trade a stack address the memory analysis follows for one it cannot.
//!
//! No hull a run has computed grows as reads move: hulls are of nodes reading
//! a phi, through their operands, and the nodes gaining reads, `y` (entry
//! computed when the run starts), its cone and `y - c`, read none.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, UseId, ValueId};
use strider_ir::{
    CtrlKey, DominatorTree, Function, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp,
    control_edge_dominator_tree,
};

use crate::Result;
use crate::const_eval::var_and_const_of;
use crate::opt::if_cond_inversion::bool_not_operand;

#[cfg(test)]
thread_local! {
    /// Reads of a tested value examined.
    pub(super) static READS_SCANNED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub(super) fn fold_equal_edges(edit: &mut crate::EditFunction<'_>) -> Result<()> {
    let mut ifs: Vec<NodeId> = edit
        .live_of_kind(|k| matches!(k, NodeKind::If))
        .filter(|&branch| equality(edit.function(), branch).is_some())
        .collect();
    if ifs.is_empty() {
        return Ok(());
    }
    let mut points = Points::new(edit.function());
    // Outer `If`s first, so one nested on the same phi has had its `Equal`
    // read moved and no longer tests it.
    ifs.sort_by_key(|&branch| points.tree.span(CtrlKey::Node(branch)));
    let mut reads: FxHashMap<ValueId, Reads> = FxHashMap::default();
    for branch in ifs {
        let Some(eq) = equality(edit.function(), branch) else {
            continue;
        };
        let Some(edge) = points.tree.span(CtrlKey::Edge(eq.edge)) else {
            continue;
        };
        let mut below = |edit: &crate::EditFunction<'_>, value| -> Result<SmallVec<[UseId; 4]>> {
            match reads.entry(value) {
                std::collections::hash_map::Entry::Occupied(at) => Ok(at.into_mut().take(edge)),
                std::collections::hash_map::Entry::Vacant(at) => {
                    Ok(at.insert(points.reads(edit.function(), value)?).take(edge))
                }
            }
        };
        let side = below(edit, eq.side)?;
        move_reads(edit, &side, eq.other, eq.cond);
        let moved = below(edit, eq.phi)?;
        if moved.is_empty() {
            continue;
        }
        let ty = edit
            .value_type_opt(eq.phi)
            .expect("an Equal operand is typed");
        let value = if eq.delta == 0 {
            eq.base
        } else {
            let delta = edit.build_int_const(eq.delta, ty)?;
            edit.absorb_fingerprint(delta, eq.cond);
            edit.build_int_binary_operation(eq.base, delta, IntBinaryOp::Add, ty)?
        };
        move_reads(edit, &moved, value, eq.cond);
    }
    Ok(())
}

fn move_reads(edit: &mut crate::EditFunction<'_>, reads: &[UseId], value: ValueId, cond: ValueId) {
    if reads.is_empty() {
        return;
    }
    for &read in reads {
        edit.update_input(read, value);
    }
    edit.absorb_fingerprint(value, cond);
}

/// A tested value's reads with their hulls, by the hulls' `pre`; a moved
/// read is taken out.
struct Reads(Vec<(u32, u32, Option<UseId>)>);

impl Reads {
    /// Takes every read whose hull lies within `edge`, a span.
    fn take(&mut self, (lo, hi): (u32, u32)) -> SmallVec<[UseId; 4]> {
        let from = self.0.partition_point(|&(enter, ..)| enter < lo);
        let mut below = SmallVec::new();
        for (enter, exit, read) in &mut self.0[from..] {
            #[cfg(test)]
            READS_SCANNED.with(|c| c.set(c.get() + 1));
            if *enter > hi {
                break;
            }
            if *exit <= hi {
                below.extend(read.take());
            }
        }
        below
    }
}

/// `side == other` on `edge`, `side` being `phi` plus constants, and so
/// `phi == base + delta`.
struct Equality {
    side: ValueId,
    other: ValueId,
    phi: ValueId,
    base: ValueId,
    delta: u128,
    edge: ValueId,
    /// The `Equal`.
    cond: ValueId,
}

/// The equality `branch` establishes on one of its edges, between a phi and a
/// value [`entry_computed`].
fn equality(function: &Function, branch: NodeId) -> Option<Equality> {
    let mut cond = function.if_cond(branch);
    let mut holds_on = 0;
    while let Some(inner) = bool_not_operand(function, cond) {
        cond = inner;
        holds_on ^= 1;
    }
    let cmp = function.producer(cond);
    if *function.node_kind(cmp) != NodeKind::IntCmpOp(IntCmpOp::Equal) {
        return None;
    }
    let [a, b] = function.node_inputs_exact::<2>(cmp).ok()?;
    let mask = function.value_type_opt(a)?.int_mask_u128()?;
    // Through every constant `Add`, so a root is never one.
    let rooted = |mut v: ValueId| {
        let mut at = 0u128;
        while let Some((x, c)) = var_and_const_of(
            function,
            function.producer(v),
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
        ) {
            (v, at) = (x, at.wrapping_add(c));
        }
        (v, at)
    };
    let is_phi = |v: ValueId| matches!(function.node_kind(function.producer(v)), NodeKind::Phi);
    let (side, other) = match (rooted(a), rooted(b)) {
        (x, y) if is_phi(x.0) && entry_computed(function, b) => ((a, x), (b, y)),
        (y, x) if is_phi(x.0) && entry_computed(function, a) => ((b, x), (a, y)),
        _ => return None,
    };
    let (side, (phi, at_phi)) = side;
    let (other, (base, at_base)) = other;
    Some(Equality {
        side,
        other,
        phi,
        base,
        delta: at_base.wrapping_sub(at_phi) & mask,
        edge: function.node_outputs(branch)[holds_on],
        cond,
    })
}

/// Nodes [`entry_computed`] visits.
const ENTRY_COMPUTED_NODES: usize = 16;

/// Whether `value` is computed from entry values and constants by integer
/// arithmetic, within [`ENTRY_COMPUTED_NODES`] nodes.
fn entry_computed(function: &Function, value: ValueId) -> bool {
    let mut stack = vec![value];
    for _ in 0..ENTRY_COMPUTED_NODES {
        let Some(v) = stack.pop() else {
            return true;
        };
        let node = function.producer(v);
        let kind = function.node_kind(node);
        if matches!(
            kind,
            NodeKind::IntBinaryOp(_)
                | NodeKind::IntUnaryOp(_)
                | NodeKind::Extend(_)
                | NodeKind::Truncate
        ) {
            stack.extend(function.node_inputs(node));
        } else if !matches!(kind, NodeKind::InitialVar(_) | NodeKind::IntConst(_)) {
            return false;
        }
    }
    stack.is_empty()
}

/// Read points on the edge-split dominator tree, as `(pre, post)` hulls: a
/// span encloses every point of a hull iff it encloses the hull.
struct Points {
    tree: DominatorTree<CtrlKey>,
    /// Per floating node, the hull of the points its value is read at, a read
    /// outside the tree never running; `None` when no read is inside.
    floating: FxHashMap<NodeId, Option<(u32, u32)>>,
}

impl Points {
    fn new(function: &Function) -> Self {
        Self {
            tree: control_edge_dominator_tree(function),
            floating: FxHashMap::default(),
        }
    }

    /// Every read of `value` that has a hull, by its hull.
    fn reads(&mut self, function: &Function, value: ValueId) -> Result<Reads> {
        let mut reads = Vec::new();
        for (user, slot) in function.value_uses(value) {
            if let Some((enter, exit)) = self.hull(function, user, slot) {
                let read = function.node_input_id_at(user, slot as usize)?;
                reads.push((enter, exit, Some(read)));
            }
        }
        reads.sort_unstable_by_key(|&(enter, ..)| enter);
        Ok(Reads(reads))
    }

    /// The hull of the points `user` reads its input `slot` at.
    fn hull(&mut self, function: &Function, user: NodeId, slot: u32) -> Option<(u32, u32)> {
        if let Some(point) = pinned_point(function, user, slot) {
            return self.tree.span(point);
        }
        self.fill(function, user);
        self.floating[&user]
    }

    /// Computes the hull of `root`, a floating node, and of every floating
    /// node reading it, transitively.
    fn fill(&mut self, function: &Function, root: NodeId) {
        let mut stack = vec![(root, false)];
        while let Some((node, expanded)) = stack.pop() {
            let reads = function
                .node_outputs(node)
                .iter()
                .flat_map(|&out| function.value_uses(out));
            if expanded {
                let mut hull: Option<(u32, u32)> = None;
                for (user, slot) in reads {
                    let at = match pinned_point(function, user, slot) {
                        Some(point) => self.tree.span(point),
                        None => self.floating[&user],
                    };
                    if let Some((enter, exit)) = at {
                        hull = Some(
                            hull.map_or((enter, exit), |(lo, hi)| (lo.min(enter), hi.max(exit))),
                        );
                    }
                }
                self.floating.insert(node, hull);
                continue;
            }
            if self.floating.contains_key(&node) {
                continue;
            }
            // In progress: a data cycle back to it reads no point.
            self.floating.insert(node, None);
            stack.push((node, true));
            let next: SmallVec<[NodeId; 4]> = reads
                .map(|(user, _)| user)
                .filter(|&user| !is_pinned(function, user))
                .collect();
            stack.extend(next.into_iter().map(|user| (user, false)));
        }
    }
}

/// A phi, or a control node: every control node that reads a value also reads
/// control.
fn is_pinned(function: &Function, node: NodeId) -> bool {
    matches!(function.node_kind(node), NodeKind::Phi | NodeKind::MemPhi)
        || function
            .node_inputs(node)
            .into_iter()
            .any(|v| function.value_kind(v).is_control())
}

/// Where a pinned `user` reads its input `slot`; `None` for a floating one.
fn pinned_point(function: &Function, user: NodeId, slot: u32) -> Option<CtrlKey> {
    if matches!(function.node_kind(user), NodeKind::Phi | NodeKind::MemPhi) {
        let region = function.phi_region(user);
        return Some(CtrlKey::Edge(
            function.node_inputs(region)[slot as usize - 1],
        ));
    }
    is_pinned(function, user).then_some(CtrlKey::Node(user))
}
