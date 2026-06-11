//! [`EditFunction`] — the self-cleaning in-place editing context: a borrowed
//! `&mut Function` plus its persistent [`FunctionState`] bookkeeping (live
//! set, input-less `roots`, the maybe-dead `queue`, and per-node flags).
//!
//! Every mutation an editor performs routes through one of the curated verbs
//! below, which keep the cached live/roots state and the maybe-dead queue
//! accurate without a re-walk.  Asm-fingerprint propagation stays automatic:
//! there is no raw `extend_asm_fingerprint` here — fresh
//! nodes are stamped via [`EditFunction::create_node_attributed`]; composite
//! rewrites inline `extend_asm_fingerprint_from` directly.
//!
//! The rewrite *rules* (`rewrite_rule`, `GraphRewriter`, the template
//! interpreter) live in the downstream optimizer crate; this module owns only
//! the function-editing primitives they build on.

use entity_utils::{DenseEntitySet, Worklist};

use crate::function::state::{FunctionState, NodeFlags};

use crate::builder::IRBuilder;
use crate::IRViewer;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, UseId, ValueId, ValueKind};
use crate::{Function, Graph};

// ── EditFunction ─────────────────────────────────────────────────────

/// Edit context: a borrowed `&mut Function` plus its self-cleaning
/// [`FunctionState`] bookkeeping. Used by the optimizer's rewrite rules and
/// destructive passes.
///
/// The function's entry [`NodeId`] is derived on demand via
/// [`Self::entry`]; the wrapped function is required to be in its built
/// form (`function.entry()` is `Some(_)`), checked at construction time
/// by [`Self::new`].
pub struct EditFunction<'g> {
    pub(crate) function: &'g mut Function,
    state: FunctionState,
}

impl<'g> EditFunction<'g> {
    /// The sole constructor. Borrows a built [`Function`] and owns a freshly-
    /// populated [`FunctionState`]. Does NOT cull pre-existing dead nodes —
    /// call [`Self::cull_dead`] explicitly if you want that.
    ///
    /// # Errors
    ///
    /// Returns an error if `function` has not been built (no entry node).
    pub fn new(function: &'g mut Function) -> Result<Self> {
        function
            .entry()
            .ok_or_else(|| anyhow::anyhow!("EditFunction::new: entry node is not set"))?;
        let state = FunctionState::populate(function);
        Ok(Self { function, state })
    }

    /// Cull every pre-existing dead node: walk the **raw** forward def→use
    /// graph from the seeded `roots` (so dead consumers of still-live
    /// producers are reached) and `kill_node` everything not in
    /// `state.live_nodes`.  Idempotent on an already-clean graph (nothing
    /// outside the live set), since `populate` already excluded unreachable
    /// nodes from `live_nodes`.
    ///
    /// Explicit: [`Self::new`] no longer runs this — callers that need the
    /// initial cull invoke it themselves.  Callers grafting deliberate
    /// off-entry scaffolding (e.g. memory-SSA test shapes) simply skip it.
    pub fn cull_dead(&mut self) {
        use crate::walk::{PostOrder, RawDefUseSuccs};
        let order: Vec<NodeId> = PostOrder::new(
            RawDefUseSuccs::new(self.function.graph()),
            self.state.roots.iter(),
        )
        .collect();
        for node in order {
            if !self.state.live_nodes.contains(node) {
                self.kill_node(node);
            }
        }
    }

    // ── function access ──────────────────────────────────────────────
    //
    // The wrapped `&mut Function` is `pub(crate)` to this crate; downstream
    // crates reach it through these accessors.

    /// Shared access to the wrapped [`Function`].
    pub fn function(&self) -> &Function {
        self.function
    }

    /// Mutable access to the wrapped [`Function`].
    ///
    /// Bypasses the cached live/roots bookkeeping — callers that mutate the
    /// graph structure through this handle are responsible for any state the
    /// curated verbs would otherwise maintain.
    pub fn function_mut(&mut self) -> &mut Function {
        self.function
    }

    /// Pre-order graph walk starting at [`Self::entry`].
    pub fn walk(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self.function.graph(), self.entry())
    }

    /// Kind-filtered pre-order walk.
    pub fn walk_kind<'a, P>(&'a self, mut pred: P) -> impl Iterator<Item = NodeId> + 'a
    where
        P: FnMut(&NodeKind) -> bool + 'a,
    {
        let g: &Graph = self.function.graph();
        self.walk().filter(move |&n| pred(g.node_kind(n)))
    }

    /// Post-order over the cached live def→use graph, seeded from the
    /// O(1)-maintained `roots` (no `compute_full` re-walk): every node is
    /// yielded after all of its consumers.  Roots are visited in ascending
    /// `NodeId` order, which is STABLE across edits (deterministic), but
    /// differs from `GraphWalkInfo::compute_full`'s preorder-discovery order —
    /// see [`Self::reverse_postorder_filter`] for why that distinction matters.
    ///
    /// **Entry-global contract:** the cached roots are entry-global; this
    /// walk is valid only for the full entry-rooted graph.  A post-order seeded
    /// at a non-entry node must recompute roots from scratch (e.g. via a fresh
    /// [`walk_info(Some(seed))`](crate::IRWalker::walk_info) +
    /// [`reverse_postorder`](crate::IRWalker::reverse_postorder)) rather than
    /// reusing these.
    pub fn postorder(&self) -> Vec<NodeId> {
        use crate::walk::{DefUseSuccs, PostOrder};
        PostOrder::new(
            DefUseSuccs::new(self.function.graph(), &self.state.live_nodes),
            self.state.roots.iter(),
        )
        .collect()
    }

    /// Reverse-post-order (real RPO) from the cached state: the reverse of
    /// [`Self::postorder`], so every producer precedes its consumers.  Carries
    /// the same entry-global contract as [`Self::postorder`]: uses cached roots,
    /// so it covers only the entry-rooted walk.
    pub fn reverse_postorder(&self) -> Vec<NodeId> {
        let mut v = self.postorder();
        v.reverse();
        v
    }

    /// Entry-reachable nodes in **global reverse-post-order** (entry-first),
    /// filtered by a predicate over each node's kind.  The reachable SET
    /// matches [`Self::walk_kind`]; only the ORDER is canonicalised to RPO
    /// (every producer precedes its consumers), so worklist-seeding and
    /// node scans settle in fewer iterations.
    ///
    /// Derived from the cheap cached [`Self::reverse_postorder`] (no
    /// `compute_full` re-walk).  The cached `live_nodes`/`roots` are
    /// set-accurate, so the cached walk covers the same reachable SET as
    /// `compute_full`; only the root ITERATION order differs (cached =
    /// ascending `NodeId`, `compute_full` = preorder-discovery).
    pub fn reverse_postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.reverse_postorder()
            .into_iter()
            .filter(move |&n| pred(self.function.node_kind(n)))
    }

    /// Entry-reachable nodes in **global post-order** (consumers before
    /// operands; entry last), filtered by a predicate over each node's kind —
    /// the post-order counterpart of [`Self::reverse_postorder_filter`].
    ///
    /// Derived from the cheap cached [`Self::postorder`] (no `compute_full`
    /// re-walk).  The cached `live_nodes`/`roots` are set-accurate, so the
    /// cached walk covers the same reachable SET as `compute_full`; only the
    /// root ITERATION order differs (cached = ascending `NodeId`, `compute_full`
    /// = preorder-discovery).
    pub fn postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.postorder()
            .into_iter()
            .filter(move |&n| pred(self.function.node_kind(n)))
    }

    /// Read-only access to the wrapped structural [`Graph`].
    pub fn graph_ref(&self) -> &Graph {
        self.function.graph()
    }

    /// Function-entry `NodeId` anchor.
    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> NodeId {
        self.function.entry().expect(
            "EditFunction wraps a built Function with an entry node (new() invariant)",
        )
    }

    // ── self-cleaning core ───────────────────────────────────────────
    //
    // Dead-node cleanup: every edit that *might* orphan a producer enqueues
    // it (via `will_detach_value` → `enqueue_killed_def_node`); `clean`
    // drains the queue, killing any node that is genuinely dead and
    // recursively enqueuing ITS now-orphaned operands.  Side-effecting nodes
    // (`Store`, control flow, …) are never enqueued or culled.

    /// Whether `node` is currently live (entry-reachable, not culled).
    pub fn is_live(&self, node: NodeId) -> bool {
        self.state.live_nodes.contains(node)
    }

    /// Whether `node` is currently a cached root (live and input-less).
    ///
    /// Exposes the cached `roots` membership so downstream tests and passes
    /// can assert root-set invariants without re-walking the graph.
    pub fn is_root(&self, node: NodeId) -> bool {
        self.state.roots.contains(node)
    }

    /// A clone of the cached live-node set — a snapshot for downstream
    /// comparison against a fresh entry-reachable walk.
    pub fn live_snapshot(&self) -> DenseEntitySet<NodeId> {
        self.state.live_nodes.clone()
    }

    /// A clone of the cached `roots` set — a snapshot for downstream
    /// comparison against a fresh entry-reachable walk.
    pub fn roots_snapshot(&self) -> DenseEntitySet<NodeId> {
        self.state.roots.clone()
    }

    /// Account for a value losing a use: if the detach removes its **last**
    /// use, its producer may now be dead, so enqueue the producer for the
    /// next `clean` drain.  Call this with the displaced value BEFORE
    /// rewiring (while the about-to-be-removed use still counts).
    fn will_detach_value(&mut self, value: ValueId) {
        // `nth(1).is_none()` ⟺ at most one use remains — the one we're about
        // to detach.  (Zero uses → nothing to do, but enqueueing a producer
        // with no remaining uses is harmless: `is_node_dead` confirms it.)
        if self.function.graph().value_uses(value).nth(1).is_none() {
            let def = self.function.producer(value);
            self.enqueue_killed_def_node(def);
        }
    }

    /// Mirror of [`Self::will_detach_value`]: `value` is about to GAIN a use.
    /// If its producer sits outside the cached live set (it was unreachable
    /// when the state was populated), the attach resurrects it and its
    /// transitive input cone, so walk that cone marking nodes live (and
    /// input-less ones as roots).  The walk is backward-data only and stops
    /// at already-live nodes, so the cost is O(newly-live cone); the fast
    /// path is one set lookup.
    ///
    /// Assumes the consumer gaining the use is itself live; attaching onto a
    /// dead consumer resurrects the producer cone spuriously (harmless —
    /// `clean` / finalize-`compact` reclaim it — but imprecise).
    ///
    /// CONTROL-flow producers (`If` / `Region` / `Return` / `Call` / …) are
    /// exempt from resurrection here: a pass rewiring inside a
    /// not-yet-torn-down dead-control zone (e.g. collapsing a dead branch's
    /// leftover single-pred `Region`) must not drag an explicitly-killed,
    /// already-detached `If`/`Region` corpse back into the cached walks, and
    /// the same gate stops the walk at the (already-live) `Region` behind a
    /// `Phi`'s phi-token input.  The exemption is NARROWER than the cull-side
    /// exemption (`enqueue_killed_def_node` uses `has_side_effects`): a memory
    /// `Store` (side-effecting but not control flow) reached as a genuine data
    /// input of a resurrected pure node MUST be marked live + recursed,
    /// otherwise the cached live set would omit an in-use `Store` and
    /// `cull_dead` could kill it — corrupting the graph.
    fn will_attach_value(&mut self, value: ValueId) {
        let producer = self.function.producer(value);
        if self.state.live_nodes.contains(producer) {
            return;
        }
        let mut worklist: Worklist<NodeId> = Worklist::new();
        worklist.enqueue(producer);
        while let Some(node) = worklist.dequeue() {
            // Exempt only CONTROL corpses (If/Region/Return/Call/…): a pass
            // rewiring inside a not-yet-torn-down dead-control zone, or the
            // Region behind a Phi's phi-token, must not be dragged back into
            // the live walks. A memory Store (side-effecting but NOT control
            // flow) reached as a genuine data input of a resurrected pure
            // node MUST be marked live + recursed — otherwise the cached live
            // set omits an in-use Store and cull_dead corrupts the graph.
            if self.function.node_kind(node).has_control_flow() {
                continue;
            }
            // `insert` returns false when already present, doubling as the
            // seen-set check.
            if !self.state.live_nodes.insert(node) {
                continue;
            }
            // Snapshot inputs before touching `self.state` (same borrow
            // pattern as `kill_node`).
            let inputs: smallvec::SmallVec<[ValueId; 4]> =
                self.function.node_inputs(node).into_iter().collect();
            if inputs.is_empty() {
                self.state.roots.insert(node);
            }
            for input in inputs {
                let def = self.function.producer(input);
                if !self.state.live_nodes.contains(def) {
                    worklist.enqueue(def);
                }
            }
        }
    }

    /// Flag a node whose last output use was just removed as maybe-dead and
    /// enqueue it — unless it is side-effecting (those are never culled).
    fn enqueue_killed_def_node(&mut self, def: NodeId) {
        if self.function.node_kind(def).has_side_effects() {
            return;
        }
        self.state.flags[def].insert(NodeFlags::OUTPUT_KILLED);
        self.enqueue(def);
    }

    /// Enqueue a live, not-already-queued node for the maybe-dead drain.
    fn enqueue(&mut self, node: NodeId) {
        if self.state.live_nodes.contains(node)
            && !self.state.flags[node].contains(NodeFlags::ENQUEUED)
        {
            self.state.flags[node].insert(NodeFlags::ENQUEUED);
            self.state.queue.enqueue(node);
        }
    }

    /// Pop the next queued node, clearing its `ENQUEUED` flag.  Skips (and
    /// keeps draining past) any node that is no longer live.
    fn dequeue(&mut self) -> Option<NodeId> {
        while let Some(node) = self.state.queue.dequeue() {
            self.state.flags[node].remove(NodeFlags::ENQUEUED);
            if self.state.live_nodes.contains(node) {
                return Some(node);
            }
        }
        None
    }

    /// A node is dead iff it is non-side-effecting AND every one of its
    /// outputs has no remaining use.
    fn is_node_dead(&self, node: NodeId) -> bool {
        if self.function.node_kind(node).has_side_effects() {
            return false;
        }
        self.function
            .node_outputs(node)
            .iter()
            .all(|&out| self.function.graph().value_uses(out).next().is_none())
    }

    /// Remove `node` from the live graph: detach its inputs (enqueuing each
    /// operand whose last use this removes), evict it from the live set and
    /// `roots`, and clear its flags.  `detach_node_inputs` already evicts the
    /// dedup-cache entry, so there is no separate cache removal.
    ///
    /// Deadness of each operand is checked AFTER the detach rather than
    /// per-edge before it.  When `node` holds the same value in two or more
    /// input slots (e.g. `Add(k, k)`, `Xor(x, x)`), a per-edge before-detach
    /// `will_detach_value` check still sees all N uses on every edge and never
    /// triggers the last-use enqueue, yet `detach_node_inputs` drops all N
    /// edges at once — leaving the operand at zero uses but never enqueued.
    /// Checking post-detach collapses the repeated operand correctly: all its
    /// edges are gone, so it is seen as fully unused exactly once.
    ///
    /// This is `pub` so passes can EXPLICITLY remove a structural /
    /// side-effecting node (a folded `If`, a collapsed `Region`, an
    /// `IndirectBranch` placeholder) that the automatic [`Self::clean`]
    /// cascade — which only culls non-side-effecting nodes — never reaches.
    /// `kill_node` is unconditional for the node passed; the
    /// `has_side_effects` gate only governs the operand cascade it enqueues.
    pub fn kill_node(&mut self, node: NodeId) {
        // Snapshot inputs BEFORE detaching (detach clears them).
        let inputs: Vec<ValueId> = self.function.node_inputs(node).into_iter().collect();
        self.function.graph_mut().detach_node_inputs(node);
        self.mark_node_dead(node);
        // After the detach, any input value now at zero uses has a maybe-dead
        // producer.  `enqueue_killed_def_node` gates on `has_side_effects`.
        for value in inputs {
            if self.function.graph().value_uses(value).next().is_none() {
                let producer = self.function.producer(value);
                self.enqueue_killed_def_node(producer);
            }
        }
    }

    /// Drop `node` from the live set, `roots`, and clear its flags.
    ///
    /// The `roots` removal is unconditional and O(1): `DenseEntitySet::remove`
    /// of an absent node is a harmless no-op, so there is no scan and no
    /// input-less pre-check (the old `Vec` form's per-kill linear scan was the
    /// O(kills·roots) hot spot this avoids).
    fn mark_node_dead(&mut self, node: NodeId) {
        self.state.live_nodes.remove(node);
        self.state.roots.remove(node);
        self.state.flags[node] = NodeFlags::empty();
    }

    /// Drain the maybe-dead queue: kill every enqueued node that is actually
    /// dead, recursively enqueuing its freshly-orphaned operands.  Runs to a
    /// fixed point (the queue empties).
    pub fn clean(&mut self) {
        while let Some(node) = self.dequeue() {
            let was_output_killed =
                self.state.flags[node].contains(NodeFlags::OUTPUT_KILLED);
            self.state.flags[node].remove(NodeFlags::OUTPUT_KILLED);
            if was_output_killed && self.is_node_dead(node) {
                self.kill_node(node);
            }
        }
    }

    /// The cached live nodes whose kind satisfies `pred`, in `live_nodes`
    /// iteration order — no graph walk.
    pub fn live_of_kind<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.state
            .live_nodes
            .iter()
            .filter(move |&n| pred(self.function.node_kind(n)))
    }

    // ── mutation façade ──────────────────────────────────────────────
    //
    // Every mutation a pass performs routes through one of the curated
    // methods below, which delegate to the wrapped `&mut Function` and keep
    // the cached live/roots state accurate.
    //
    // Asm-fingerprint propagation stays automatic: there is no raw
    // `extend_asm_fingerprint` here.  Passes that
    // need to stamp a fresh node's history use [`Self::create_node_attributed`]
    // (contributor-attributed creation); composite rewrites inline
    // `extend_asm_fingerprint_from` directly at their use-redirection sites.

    /// Mark a freshly-returned node as live, and record it as a root iff it is
    /// input-less.  Called after every node-creation verb so the cached
    /// live/roots state stays accurate without a re-walk.
    ///
    /// Idempotent: a cacheable `create_node` may dedup back to a node that is
    /// already live (and possibly already a root) — `DenseEntitySet::insert` is
    /// itself idempotent, so no `contains` guard is needed.
    fn track_created(&mut self, node: NodeId) {
        self.state.live_nodes.insert(node);
        if self.function.graph().node_inputs(node).is_empty() {
            self.state.roots.insert(node);
        }
    }

    /// Create a node — delegates to [`Self::create_node_attributed`] with no
    /// extra contributors.
    pub fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        self.create_node_attributed(kind, inputs, output_kinds, &[])
    }

    /// Shared node-creation choke-point: create (or dedup to) the node,
    /// union every contributor's asm-fingerprint into it, then register it
    /// into the cached live/roots state. Every creation path — the inherent
    /// [`Self::create_node`] and the [`IRBuilder`] trait impl — routes
    /// through here, so "fresh node gets stamped + tracked" has one
    /// implementation. This is the fingerprint-aware creation path; passes
    /// use it instead of hand-stamping a fresh node.
    pub fn create_node_attributed(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        // Each input gains a use on the fresh node; resurrect any whose
        // producer was dead (per-input `contains` fast path keeps this cheap).
        // SmallVec inlines the common arity on this every-creation path.
        let inputs: smallvec::SmallVec<[ValueId; 4]> = inputs.into_iter().collect();
        for &input in &inputs {
            self.will_attach_value(input);
        }
        let node = self
            .function
            .create_node_attributed(kind, inputs, output_kinds, contributors);
        self.track_created(node);
        node
    }

    /// Redirect an input slot to a new producer output — delegates to
    /// [`Graph::update_input`].
    ///
    /// Maintains the cached state on both sides of the rewire: the value being
    /// displaced off this slot loses a use, so its producer is enqueued (via
    /// `will_detach_value`) when this was its last use; the new value gains a
    /// use, so its producer (and input cone) is resurrected into the live set
    /// (via `will_attach_value`) if it was dead.
    pub fn update_input(&mut self, input_id: UseId, output_id: ValueId) {
        let displaced = self.function.graph().value_of_use(input_id);
        // No-op self-redirect: nothing is displaced and nothing is attached.
        if displaced != output_id {
            self.will_detach_value(displaced);
            self.will_attach_value(output_id);
        }
        self.function.graph_mut().update_input(input_id, output_id);
    }

    /// Append an input to a (non-cacheable) node — delegates to
    /// [`Graph::add_node_input`].
    ///
    /// Maintains `roots`: if `node` was input-less before this call, it gains
    /// an input and is no longer a root.  Maintains the live set: the appended
    /// value gains a use, so its producer (and input cone) is resurrected (via
    /// `will_attach_value`) if it was dead.
    ///
    /// # Errors
    /// Never — always `Ok(())`; the `Result` keeps the edit-verb surface
    /// uniform.
    pub fn add_node_input(
        &mut self,
        node: NodeId,
        output_id: ValueId,
    ) -> crate::error::Result<()> {
        let was_input_less = self.function.graph().node_inputs(node).is_empty();
        self.will_attach_value(output_id);
        self.function.graph_mut().add_node_input(node, output_id);
        if was_input_less {
            self.state.roots.remove(node);
        }
        Ok(())
    }

    /// Remove the input at `index` from a (non-cacheable) node —
    /// delegates to [`Graph::remove_node_input`].
    ///
    /// Maintains the maybe-dead queue: the value at `index` loses a use, so its
    /// producer is enqueued (via `will_detach_value`) when this was its
    /// last use.
    ///
    /// # Errors
    /// Never — always `Ok(())`; the `Result` keeps the edit-verb surface
    /// uniform.
    pub fn remove_node_input(
        &mut self,
        node: NodeId,
        index: u32,
    ) -> crate::error::Result<()> {
        // Snapshot the displaced value BEFORE removal so its remaining-use
        // count still includes the edge we're about to drop.
        let displaced = self
            .function
            .node_inputs(node)
            .into_iter()
            .nth(index as usize);
        if let Some(value) = displaced {
            self.will_detach_value(value);
        }
        self.function.graph_mut().remove_node_input(node, index);
        Ok(())
    }

    /// Redirect every use of `old` to `new` — delegates to
    /// [`Graph::replace_all_uses`].
    ///
    /// A generic use-redirection primitive (no fingerprint work). Higher-level
    /// composites pair this with `extend_asm_fingerprint_from` for full
    /// fingerprint absorption.
    ///
    /// Returns `true` iff at least one use was redirected.
    ///
    /// # Errors
    /// Never — always `Ok`; the `Result` keeps the edit-verb surface
    /// uniform.
    pub fn replace_all_uses(
        &mut self,
        old: ValueId,
        new: ValueId,
    ) -> crate::error::Result<bool> {
        // The redirect attaches `new` wherever `old` was used; with no uses
        // to move there is nothing to attach (and nothing to resurrect).
        if old != new && self.function.graph().value_uses(old).next().is_some() {
            self.will_attach_value(new);
        }
        Ok(self.function.graph_mut().replace_all_uses(old, new))
    }

    /// Register an argument-carrier value under a CC argument index —
    /// delegates to [`Function::register_arg_value`].
    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.function.register_arg_value(index, value);
    }

    // ── composite rewrites ───────────────────────────────────────────
    //
    // These compose the generic primitives above into the higher-level
    // operations passes need (value replacement with fingerprint absorption,
    // single-input redirection, region-predecessor removal).

    /// The single value-replacement primitive: redirect every use of `old`
    /// to `new`, after **absorbing** `old`'s producer asm-fingerprint into
    /// `new`'s producer (superset-only union).
    ///
    /// This is the one place that pairs fingerprint absorption with
    /// use-replacement — passes call this instead of hand-writing the
    /// absorb + redirect pair, so the superset-only fingerprint contract has
    /// one implementation for value rewrites.
    ///
    /// Returns `true` iff at least one use was redirected.
    ///
    /// # Errors
    /// Propagates [`Self::replace_all_uses`]'s error arm unchanged.
    pub fn replace_value(&mut self, old: ValueId, new: ValueId) -> Result<bool> {
        let into = self.function.producer(new);
        let from = self.function.producer(old);
        self.function.extend_asm_fingerprint_from(into, from);
        // Snapshot old's producer before the redirect; afterwards every use of
        // `old` has moved to `new`, so its producer is a cull candidate.
        let old_producer = self.function.producer(old);
        let changed = self.replace_all_uses(old, new)?;
        // `replace_all_uses` bypasses `update_input`'s per-edge hook, so enqueue
        // the now-orphaned producer here (side-effect-guarded inside).
        self.enqueue_killed_def_node(old_producer);
        Ok(changed)
    }

    /// Redirect a single input slot from its current producer to `new`,
    /// absorbing the displaced producer's asm-fingerprint into `new`'s
    /// producer **iff** the redirect leaves the displaced producer with
    /// no remaining uses.
    ///
    /// The companion to [`Self::replace_value`] for the single-slot case:
    /// where `replace_value` redirects *every* use of a value,
    /// `redirect_input` rewires exactly one input edge. When the displaced
    /// producer becomes dead as a result, its contributing-asm history would
    /// otherwise be lost, so it is folded into the surviving consumer's new
    /// producer (superset-only union). When the displaced producer keeps other
    /// live uses, no absorption happens — those uses still explain its value via
    /// its own fingerprint, and contaminating `new`'s producer would violate the
    /// "fingerprint names the asm insns that contribute to this value" contract.
    pub fn redirect_input(&mut self, input_id: UseId, new: ValueId) {
        let old_value = self.graph_ref().value_of_use(input_id);
        // `input_id` itself is one use of `old_value`, so "exactly one use"
        // means this edge is the only one — bounded at 2 to avoid scanning a
        // long use-list.
        let only_use = self.graph_ref().value_uses(old_value).take(2).count() == 1;
        self.update_input(input_id, new);
        if only_use {
            // `old_value` is the displaced producer's output; absorb its
            // fingerprint into `new`'s producer (superset-only union).
            let into = self.function.producer(new);
            let from = self.function.producer(old_value);
            self.function.extend_asm_fingerprint_from(into, from);
        }
    }

    /// Removes a batch of predecessor slots from a `Region` and the matching
    /// value slots from every `Phi`/`MemPhi` that consumes the Region's
    /// phi-token output — the single structural primitive for dropping dead
    /// control edges into a join.
    ///
    /// A `Region` produces `[control, phi_token]`; a `Phi`/`MemPhi` over it has
    /// inputs `[phi_token, val_pred0, val_pred1, …]`, so the value for Region
    /// predecessor `i` lives at phi input `i + 1`. Region/Phi nodes are exempt
    /// from the asm-fingerprint non-empty check, so no fingerprint work is needed.
    ///
    /// The caller passes ALL dead predecessor indices for the region at once;
    /// this method removes them highest-index-first internally so earlier
    /// removals never invalidate a later (lower) index — the caller does not
    /// need to pre-sort or remove one-by-one. Duplicate indices are deduped,
    /// and out-of-range indices are skipped per-node via bounds checks.
    ///
    /// # Errors
    /// Propagates [`Self::remove_node_input`]'s error arm.
    pub fn remove_region_predecessors(
        &mut self,
        region: NodeId,
        pred_indices: &[u32],
    ) -> Result<()> {
        debug_assert!(
            matches!(self.node_kind(region), NodeKind::Region),
            "remove_region_predecessors: node is not a Region",
        );
        if pred_indices.is_empty() {
            return Ok(());
        }
        // Highest-index-first, deduped: removing a higher slot never shifts a
        // lower one, so every remaining index stays valid across the batch.
        let mut indices: Vec<u32> = pred_indices.to_vec();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();

        // Collect the phi-token consumers once (the set of Phi/MemPhi nodes
        // doesn't change as we remove their value inputs).
        let phi_nodes: Vec<NodeId> = {
            let outputs = self.node_outputs(region);
            if outputs.len() >= 2 {
                let phi_value = outputs[1]; // ValueId: Copy
                self.graph_ref()
                    .value_uses(phi_value)
                    .map(|(n, _)| n)
                    .collect()
            } else {
                Vec::new()
            }
        };

        for pred_index in indices {
            let phi_input_idx = pred_index + 1;
            for &phi in &phi_nodes {
                if phi_input_idx < self.node_inputs(phi).len() as u32 {
                    self.remove_node_input(phi, phi_input_idx)?;
                }
            }
            if pred_index < self.node_inputs(region).len() as u32 {
                self.remove_node_input(region, pred_index)?;
            }
        }
        Ok(())
    }
}

/// Editing-context builder: contributor-attributed structural creation plus
/// the cached live/roots bookkeeping — all routed through
/// [`EditFunction::create_node_attributed`].
///
/// The inherent [`EditFunction::create_node_attributed`] (same name, same
/// body) takes precedence for direct `ctx.create_node_attributed(...)` calls
/// in passes, so this trait impl is reached only through the generic
/// [`IRBuilder`] bound (e.g. the template interpreter's
/// `instantiate<B: IRBuilder>`).
impl IRBuilder for EditFunction<'_> {
    fn function_mut(&mut self) -> &mut crate::Function {
        self.function
    }

    fn create_node_attributed<I, O>(
        &mut self,
        kind: NodeKind,
        inputs: I,
        outputs: O,
        contributors: &[NodeId],
    ) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        EditFunction::create_node_attributed(self, kind, inputs, outputs, contributors)
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    //! Local, dev-dep-free fixture builders for the `edit` unit tests.
    //!
    //! strider-ir's own `#[cfg(test)]` modules cannot use
    //! `strider_ir_test_utils`' type-returning builders (`RegisterSet`,
    //! `make_empty_fn`): under `cargo test` the dev-dep links a *separate*
    //! compilation of strider-ir, so a helper returning `strider_ir::Function`
    //! would mismatch the unit-test crate's own `Function`.  These helpers call
    //! the local [`FunctionBuilder`] directly and stamp `SENTINEL_LIFT_ADDR` so
    //! every emitted node carries the non-empty asm-fingerprint the always-on
    //! validator check requires.

    use crate::FunctionBuilder;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// A trivial-convention [`FunctionBuilder`] with a single entry region,
    /// lift-addr pre-stamped — the local analogue of
    /// `RegisterSet::new().build_fn_single_region()`.
    #[allow(clippy::expect_used)]
    pub(crate) fn single_region_builder() -> FunctionBuilder {
        let cc = strider_target::BuiltCallingConvention {
            arg_passing_regs: Vec::new(),
            callee_saved_regs: Vec::new(),
            ret_val_regs: Vec::new(),
            ret_val_regs_float: Vec::new(),
            stack_vn: strider_target::BuiltCallingConvention::default().stack_vn,
            stack_args: None,
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
        };
        let mut b = FunctionBuilder::new(Vec::new(), &cc, strider_target::Endianness::Little)
            .expect("FunctionBuilder::new");
        let region = b.create_region().expect("create_region");
        b.set_entry_region(region).expect("set_entry_region");
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::test_fixtures::single_region_builder;
    use super::EditFunction;
    use crate::IRViewer;
    use crate::builder::IRBuilderExt;
    use crate::node::{IntPayload, NodeKind, ValueKind, ValueType};
    use crate::IntBinaryOp;
    use cranelift_entity::EntityRef;
    use std::collections::BTreeSet;

    /// Creating a node marks it live; killing it removes it from the live set.
    #[test]
    fn create_then_kill_tracks_liveness() {
        let mut b = single_region_builder();
        let root = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let mut ctx = EditFunction::new(&mut function).unwrap();

        let node = ctx.create_node(
            NodeKind::IntConst(IntPayload::Small(42)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(ctx.is_live(node), "freshly created node is live");
        assert!(ctx.is_root(node), "input-less fresh const is a root");

        ctx.kill_node(node);
        assert!(!ctx.is_live(node), "killed node is no longer live");
        assert!(!ctx.is_root(node), "killed node dropped from roots");
    }

    /// After `replace_value` + `clean`, the cached live set must equal a fresh
    /// entry-reachable walk's live set (the core self-cleaning invariant).
    #[test]
    fn replace_value_then_clean_keeps_live_eq_reachable() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        let add = b
            .build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let add_node = function.producer(add);

        let mut ctx = EditFunction::new(&mut function).unwrap();
        ctx.cull_dead();

        // Locate the Add via the cached live-kind iterator.
        let located = ctx
            .live_of_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
            .next()
            .expect("the Add must be live");
        assert_eq!(located, add_node, "live_of_kind located the Add");

        // Replace the Add's output with c1, then drain the maybe-dead queue.
        ctx.replace_value(add, c1).unwrap();
        ctx.clean();

        // The cached live set must equal a fresh entry-reachable walk.
        let entry = ctx.entry();
        let info = crate::walk::GraphWalkInfo::compute_full(ctx.function().graph(), entry);
        let fresh: BTreeSet<usize> = info.live_nodes.iter().map(|n| n.index()).collect();
        let cached: BTreeSet<usize> =
            ctx.live_snapshot().iter().map(|n| n.index()).collect();
        assert_eq!(
            cached, fresh,
            "cached live_nodes must equal the entry-reachable set after replace + clean"
        );

        // The Add is gone; c1 survives (now the return's value).
        assert!(!ctx.is_live(add_node), "replaced-away Add is culled");
        assert!(ctx.is_live(ctx.producer(c1)), "surviving c1 stays live");
    }

    /// Killing a cached node evicts its dedup-cache entry (via
    /// `detach_node_inputs`), so re-creating the same shape mints a FRESH
    /// node — the killed id is never resurrected — and the fresh node is
    /// tracked live (and as a root, being input-less).
    #[test]
    fn kill_cached_node_then_recreate_yields_fresh_live_node() {
        let mut b = single_region_builder();
        let root = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let mut ctx = EditFunction::new(&mut function).unwrap();

        let node = ctx.create_node(
            NodeKind::IntConst(IntPayload::Small(42)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        ctx.kill_node(node);
        assert!(!ctx.is_live(node));

        let recreated = ctx.create_node(
            NodeKind::IntConst(IntPayload::Small(42)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_ne!(
            recreated, node,
            "the killed node's cache entry was evicted, so the same shape mints a fresh node"
        );
        assert!(ctx.is_live(recreated), "re-created node is live");
        assert!(ctx.is_root(recreated), "input-less re-created const is a root");
    }

    /// `cull_dead` is idempotent: the first call kills the dead consumer of
    /// a live producer; the second call changes nothing (live + roots
    /// snapshots are identical).
    #[test]
    fn cull_dead_twice_is_idempotent() {
        let mut b = single_region_builder();
        let root = b.build_int_const(5u64, ValueType::I64).unwrap();
        // A dead consumer of the live const: a Neg whose output nothing uses.
        let dead_neg = b
            .build_int_unary_operation(root, crate::node::IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let dead_neg_node = function.producer(dead_neg);

        let mut ctx = EditFunction::new(&mut function).unwrap();
        assert!(
            !ctx.is_live(dead_neg_node),
            "the unreachable Neg is excluded from the live set at populate"
        );

        ctx.cull_dead();
        assert!(
            ctx.function().node_inputs(dead_neg_node).is_empty(),
            "first cull detaches the dead consumer's inputs"
        );
        let live_after_first = ctx.live_snapshot();
        let roots_after_first = ctx.roots_snapshot();

        ctx.cull_dead();
        assert_eq!(
            ctx.live_snapshot().iter().collect::<Vec<_>>(),
            live_after_first.iter().collect::<Vec<_>>(),
            "second cull must not change the live set"
        );
        assert_eq!(
            ctx.roots_snapshot().iter().collect::<Vec<_>>(),
            roots_after_first.iter().collect::<Vec<_>>(),
            "second cull must not change the roots set"
        );
    }

    /// `replace_value` onto a value whose producer was unreachable (dead) at
    /// `EditFunction::new` time: the redirect makes the producer
    /// entry-reachable, and the attach hook resurrects it — together with its
    /// transitive input cone — into the cached live/roots state, so the cached
    /// walks see it and a subsequent `clean` + `cull_dead` leaves it intact.
    #[test]
    fn replace_value_resurrects_previously_dead_producer() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        // Orphan producer: a Neg nothing consumes (unreachable at populate).
        let orphan = b
            .build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let orphan_node = function.producer(orphan);
        let c2_node = function.producer(c2);

        let mut ctx = EditFunction::new(&mut function).unwrap();
        assert!(!ctx.is_live(orphan_node), "orphan starts outside the live set");
        assert!(!ctx.is_live(c2_node), "the orphan's operand starts dead too");

        // Redirect the Return's value from c1 to the orphan's output.
        ctx.replace_value(c1, orphan).unwrap();

        assert!(ctx.is_live(orphan_node), "attach resurrects the orphan producer");
        assert!(ctx.is_live(c2_node), "…and its transitive input cone");
        assert!(ctx.is_root(c2_node), "the resurrected input-less const is a root");
        assert!(!ctx.is_root(orphan_node), "the Neg has an input, so it is not a root");
        assert!(
            ctx.postorder().contains(&orphan_node),
            "the cached postorder visits the resurrected node"
        );

        // Drain the now-unused c1 const, cull, and check the core invariant:
        // the cached live set equals a fresh entry-reachable walk.
        ctx.clean();
        ctx.cull_dead();
        assert!(ctx.is_live(orphan_node), "cull_dead keeps the resurrected node");
        let entry = ctx.entry();
        let info = crate::walk::GraphWalkInfo::compute_full(ctx.function().graph(), entry);
        let fresh: BTreeSet<usize> = info.live_nodes.iter().map(|n| n.index()).collect();
        let cached: BTreeSet<usize> =
            ctx.live_snapshot().iter().map(|n| n.index()).collect();
        assert_eq!(
            cached, fresh,
            "cached live_nodes must equal the entry-reachable set after resurrect + clean + cull"
        );
    }

    /// `update_input` onto a previously-dead producer with a multi-node input
    /// cone (`Neg(Neg(k))`): the attach hook must resurrect the WHOLE cone
    /// transitively, marking its input-less leaf as a root.
    #[test]
    fn update_input_resurrects_previously_dead_producer() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        // A dead 3-node cone: Neg(Neg(k)), nothing consumes the outer Neg.
        let k = b.build_int_const(7u64, ValueType::I64).unwrap();
        let inner = b
            .build_int_unary_operation(k, crate::node::IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        let outer = b
            .build_int_unary_operation(inner, crate::node::IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let k_node = function.producer(k);
        let inner_node = function.producer(inner);
        let outer_node = function.producer(outer);
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function).unwrap();
        for n in [k_node, inner_node, outer_node] {
            assert!(!ctx.is_live(n), "the whole orphan cone starts dead");
        }

        // Rewire the Return's c1 slot onto the dead cone's output.
        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == c1)
            .expect("Return consumes c1");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, outer);

        for n in [k_node, inner_node, outer_node] {
            assert!(ctx.is_live(n), "the whole resurrected cone is live");
        }
        assert!(ctx.is_root(k_node), "the cone's input-less const becomes a root");
        assert!(!ctx.is_root(inner_node), "inner Neg has an input — not a root");
        assert!(!ctx.is_root(outer_node), "outer Neg has an input — not a root");

        let post = ctx.postorder();
        for n in [k_node, inner_node, outer_node] {
            assert!(post.contains(&n), "cached postorder visits the resurrected cone");
        }
    }

    /// `add_node_input` of a previously-dead producer's output: the appended
    /// use resurrects the producer and its input cone.
    #[test]
    fn add_node_input_resurrects_previously_dead_producer() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        // Orphan producer: a Neg nothing consumes (unreachable at populate).
        let orphan = b
            .build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let orphan_node = function.producer(orphan);
        let c2_node = function.producer(c2);
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function).unwrap();
        assert!(!ctx.is_live(orphan_node), "orphan starts outside the live set");

        ctx.add_node_input(return_node, orphan).unwrap();

        assert!(ctx.is_live(orphan_node), "attach resurrects the orphan producer");
        assert!(ctx.is_live(c2_node), "…and its transitive input cone");
        assert!(ctx.is_root(c2_node), "the resurrected input-less const is a root");
        assert!(
            ctx.postorder().contains(&orphan_node),
            "the cached postorder visits the resurrected node"
        );
    }

    /// Attaching a value whose producer is ALREADY live is the fast path: one
    /// set lookup, no walk — the cached live/roots state is unchanged.
    #[test]
    fn attach_already_live_value_keeps_cached_state_unchanged() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        let add = b
            .build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function).unwrap();
        let live_before = ctx.live_snapshot();
        let roots_before = ctx.roots_snapshot();

        // Redirect the Return's add slot onto c1, whose producer is live.
        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == add)
            .expect("Return consumes the Add");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, c1);

        assert_eq!(
            ctx.live_snapshot().iter().collect::<Vec<_>>(),
            live_before.iter().collect::<Vec<_>>(),
            "attaching an already-live value must not change the live set"
        );
        assert_eq!(
            ctx.roots_snapshot().iter().collect::<Vec<_>>(),
            roots_before.iter().collect::<Vec<_>>(),
            "attaching an already-live value must not change the roots set"
        );
    }

    /// The corruption case the attach hook exists to prevent: the orphan
    /// consumes a LIVE value, so it is raw-reachable from a live root and
    /// `cull_dead`'s walk visits it.  Without accurate liveness, that cull
    /// would `kill_node` a producer whose output the Return now uses.  After
    /// the resurrect, `cull_dead` must keep it intact and the graph must
    /// still validate.
    #[test]
    fn cull_dead_after_resurrect_keeps_node_and_validates() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        // Orphan consuming the LIVE c1 (raw-reachable from the c1 root).
        let orphan = b
            .build_int_unary_operation(c1, crate::node::IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let orphan_node = function.producer(orphan);
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function).unwrap();
        assert!(!ctx.is_live(orphan_node), "orphan starts outside the live set");

        // Rewire the Return's c1 slot onto the orphan's output.
        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == c1)
            .expect("Return consumes c1");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, orphan);
        assert!(ctx.is_live(orphan_node), "attach resurrects the orphan producer");

        ctx.cull_dead();
        assert!(
            ctx.is_live(orphan_node),
            "cull_dead must not kill the resurrected node"
        );
        assert!(
            !ctx.function().node_inputs(orphan_node).is_empty(),
            "the resurrected node's inputs stay attached"
        );
        let entry = ctx.entry();
        crate::validate::validate(ctx.function(), entry)
            .expect("graph validates after resurrect + cull_dead");
    }

    /// Attaching a value produced by an explicitly-killed side-effecting node
    /// must NOT resurrect it — side-effecting liveness changes only through
    /// the explicit verbs, mirroring `enqueue_killed_def_node`'s exemption on
    /// the cull side.  This is the corpse shape a pass rewiring inside a
    /// not-yet-torn-down dead branch produces: the killed `If` is detached
    /// (0 inputs) but its dead control output is still wired downstream.
    #[test]
    fn attach_output_of_killed_side_effecting_node_does_not_resurrect_it() {
        let mut b = single_region_builder();
        let t = b.create_region().unwrap();
        let f = b.create_region().unwrap();
        let cond = b.build_boolean_const(true);
        b.build_if(cond, t, f).unwrap();
        b.set_region(t);
        b.build_return(None, &[]).unwrap();
        b.set_region(f);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let if_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::If))
            .expect("an If node");
        // If outputs: [ctrl_true, ctrl_false].
        let ctrl_false = function.node_outputs(if_node)[1];
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function).unwrap();
        ctx.kill_node(if_node);
        assert!(!ctx.is_live(if_node), "the killed If leaves the live set");

        // Attach the corpse's dangling control output to a live consumer.
        ctx.add_node_input(return_node, ctrl_false).unwrap();

        assert!(
            !ctx.is_live(if_node),
            "a side-effecting corpse must not be resurrected by an attach"
        );
        assert!(
            !ctx.is_root(if_node),
            "the detached (0-input) corpse must not become a cached root"
        );
    }

    /// Resurrecting a pure node (a `Load`) whose data input cone reaches a
    /// side-effecting MEMORY producer (a `Store` on the memory chain) must
    /// mark that `Store` live too — otherwise the cached live set omits a
    /// node whose output the resurrected `Load` consumes, and `cull_dead`
    /// would kill an in-use `Store` (graph corruption).  The exemption in
    /// `will_attach_value` is for CONTROL corpses only, not memory writes
    /// reached as genuine data inputs.
    #[test]
    fn resurrect_load_marks_its_memory_store_live() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        let data = b.build_int_const(0x42u64, ValueType::I64).unwrap();
        // A dead Store→Load chain hung off the live InitialMemory (an extra
        // consumer of it — valid), but whose own outputs nothing on the live
        // spine consumes, so neither Store nor Load is entry-reachable. Built
        // via the low-level create_node so the memory edge bypasses the
        // builder's current-region threading.
        let init_mem = b.entry_memory;
        let store_node = b.create_node(
            NodeKind::Store(rsleigh::VnSpace::RAM),
            [init_mem, addr, data],
            [ValueKind::Memory],
        );
        let store_mem = b.function().node_outputs_exact::<1>(store_node).unwrap()[0];
        let load_node = b.create_node(
            NodeKind::Load(rsleigh::VnSpace::RAM),
            [store_mem, addr],
            [ValueKind::Typed(ValueType::I64)],
        );
        let loaded = b.function().node_outputs_exact::<1>(load_node).unwrap()[0];
        // The live spine returns an UNRELATED const.
        let ret_val = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(ret_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        // EditFunction::new does NOT cull pre-existing dead nodes, so the
        // dead Store/Load chain is present-but-not-live (still wired together)
        // — exactly the state a pass would resurrect from.
        let mut ctx = EditFunction::new(&mut function).unwrap();
        assert!(!ctx.is_live(load_node), "Load starts dead");
        assert!(!ctx.is_live(store_node), "Store starts dead");

        // Rewire the Return's value slot onto the dead Load's output. The
        // attach hook resurrects the Load; the Store on its memory input
        // must come live with it.
        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == ret_val)
            .expect("Return consumes ret_val");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, loaded);

        assert!(ctx.is_live(load_node), "the resurrected Load is live");
        assert!(
            ctx.is_live(store_node),
            "the Store on the resurrected Load's memory input must be live too"
        );

        // cull_dead must not kill the in-use Store, and the graph must validate.
        ctx.cull_dead();
        assert!(
            ctx.is_live(store_node),
            "cull_dead must not kill the in-use memory Store"
        );
        let entry = ctx.entry();
        crate::validate::validate(ctx.function(), entry)
            .expect("graph validates after resurrecting a Load over a memory Store");
    }

    /// Side-effecting (`Store`) and control (`Return`) nodes are never
    /// enqueued or culled, even when a maybe-dead drain is forced over them.
    #[test]
    fn clean_keeps_side_effect_node() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        let data = b.build_int_const(0x42u64, ValueType::I64).unwrap();
        b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
        let ret_val = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(ret_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let store_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Store(_)))
            .expect("a Store node");
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function).unwrap();
        ctx.cull_dead();

        // Force-enqueue both as maybe-dead, then drain. `has_side_effects()`
        // guards them: `enqueue_killed_def_node` returns early and `clean`'s
        // `is_node_dead` is false, so neither is culled.
        ctx.enqueue_killed_def_node(store_node);
        ctx.enqueue_killed_def_node(return_node);
        ctx.clean();

        assert!(ctx.is_live(store_node), "Store (side-effecting) never culled");
        assert!(ctx.is_live(return_node), "Return (control) never culled");
    }
}
