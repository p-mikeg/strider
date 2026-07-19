//! `PyFunction` — wraps `strider_ir::Function` and exposes dot rendering
//! plus pattern queries and rewrites.
//!
//! The IR graph's dot dumper requires a borrowed `Sleigh` for
//! register-name resolution.  PyFunction keeps a `Py<PyCfg>` reference
//! so the Sleigh stays alive for the graph's lifetime and is
//! reachable through `strider_cfg::Cfg::sleigh`.

use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use pyo3::prelude::*;
use strider_ir::IRWalker;
use strider_ir::node::NodeKind;

use crate::cfg::PyCfg;

/// Opaque wrapper over `strider_ir::Function`.
///
/// The graph is held in `Rc<RefCell<...>>` so optimization passes can
/// mutate it without requiring `&mut self` on the PyFunction wrapper, and
/// so the same graph can be shared across multiple Python references.  `Rc`
/// (not `Arc`) and the `unsendable` pyclass because the workspace is
/// single-threaded and `Function` is `!Sync` (its SP-decomposition cache is a
/// `RefCell`); Python access is GIL-serialised regardless.
#[pyclass(name = "Function", module = "strider.ir", unsendable)]
pub struct PyFunction {
    pub(crate) inner: Rc<RefCell<strider_ir::Function>>,
    /// Strong reference to the parent Cfg; keeps the Sleigh alive for
    /// dot rendering and ensures destruction order is graph-then-cfg.
    pub(crate) cfg: Py<PyCfg>,
}

impl PyFunction {
    pub(crate) fn new(function: strider_ir::Function, cfg: Py<PyCfg>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(function)),
            cfg,
        }
    }

    /// Borrow the inner graph for read.  Returns an `anyhow::Error` if the
    /// graph is currently borrowed for mutation (a `RefCell` conflict).
    pub(crate) fn read_inner(&self) -> anyhow::Result<Ref<'_, strider_ir::Function>> {
        self.inner
            .try_borrow()
            .map_err(|_| anyhow::anyhow!("Function is currently borrowed for mutation"))
    }

    /// Try to borrow the inner graph mutably.  Used by mutating methods
    /// (`compact`, `rewrite`, and the `run_pipeline_in_place` helper
    /// `Lifter.optimize` drives) so that a re-entrant call from inside a
    /// `.when()` predicate (which holds the read borrow for the duration of
    /// `find_all`) surfaces a typed error rather than panicking on the
    /// already-borrowed `RefCell`.
    pub(crate) fn try_write_inner(&self) -> anyhow::Result<RefMut<'_, strider_ir::Function>> {
        self.inner.try_borrow_mut().map_err(|_| {
            anyhow::anyhow!(
                "Function mutation rejected: the function is currently borrowed for read \
                 (typically because this call is from inside a `.when()` predicate \
                 invoked by `find_all`/`find_unique`).  Mutating the function \
                 from within a pattern predicate is not supported — collect matches \
                 first and mutate after `find_all` returns."
            )
        })
    }

    /// Borrow the inner graph for read, then run `f` against it.  Centralises
    /// the `self.read_inner().map_err(into_strider_err)?` incantation that
    /// every read-only `#[pymethods]` accessor would otherwise repeat.  Use
    /// this variant when `f` itself returns a `PyResult` (e.g. it builds an
    /// error from graph state).
    fn with_read<R>(&self, f: impl FnOnce(&strider_ir::Function) -> PyResult<R>) -> PyResult<R> {
        let function = self.read_inner().map_err(crate::errors::into_strider_err)?;
        f(&function)
    }

    /// Like [`Self::with_read`] but for accessors whose closure just
    /// produces a value with no further fallible step — saves the
    /// per-site `Ok(...)` wrapping.
    fn with_read_value<R>(&self, f: impl FnOnce(&strider_ir::Function) -> R) -> PyResult<R> {
        let function = self.read_inner().map_err(crate::errors::into_strider_err)?;
        Ok(f(&function))
    }

    /// Run `pipeline` over this graph in place, bumping the generation
    /// first so any stale handle is invalidated even if a pass errors
    /// mid-run and leaves the arena partially rewritten.  `label` names
    /// the operation in the surfaced error.
    ///
    /// `pub(crate)` (rather than a private fn) because `Lifter.optimize`
    /// (`strider_cls.rs`) drives the same in-place-run logic — it lives
    /// here so `PyFunction`'s lock-acquisition/generation-bump contract
    /// stays in one place rather than being duplicated at the call site.
    pub(crate) fn run_pipeline_in_place(
        &self,
        pipeline: strider_orchestrator::opt::OptimizerPipeline,
        label: &str,
    ) -> PyResult<()> {
        let mut function = self
            .try_write_inner()
            .map_err(crate::errors::into_strider_err)?;
        // Bump the generation BEFORE running: the pipeline mutates the
        // arena in place, and a pass that errors mid-run can leave the
        // graph partially rewritten.  Invalidating outstanding handles
        // unconditionally means a stale handle can never silently read
        // that partially-optimized graph after the error is surfaced.
        function.graph_mut().bump_generation();
        pipeline
            .run(
                &mut function,
                &mut strider_orchestrator::opt::OptCtx::new(None),
            )
            .map_err(|e| {
                crate::errors::into_strider_err(anyhow::anyhow!("{label} failed: {e:?}"))
            })?;
        Ok(())
    }
}

/// Write `contents` to `path`, mapping any I/O error to a `StriderError`.
/// Shared by the `to_dot` / `to_html` file-dump paths.
fn write_to(path: &str, contents: String) -> PyResult<()> {
    std::fs::write(path, contents).map_err(|e| crate::errors::into_strider_err(anyhow::anyhow!(e)))
}

#[pymethods]
impl PyFunction {
    /// Expose the strong `Py<PyCfg>` back-reference to Python's cyclic GC
    /// so a cycle routed through a `Function` is detectable (broken at the
    /// reader's `__dict__` / `PyLifter::__clear__`; the `cfg` handle is
    /// load-bearing while the `Function` is alive, so no `__clear__` here).
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.cfg)
    }

    /// The snapshot `Cfg` this function was lifted from — kept alive for
    /// dot rendering (its `Sleigh` resolves register names).  Combine
    /// with `Lifter.to_html(function, path)` (or the `Cfg`'s own
    /// `to_html`) for a self-describing render without a separate result
    /// wrapper.
    #[getter(cfg)]
    fn get_cfg(&self, py: Python<'_>) -> Py<PyCfg> {
        self.cfg.clone_ref(py)
    }

    /// Render the graph **exactly as stored** to DOT: one node per
    /// `NodeId` (every arena node, incl. detached ones), one edge per
    /// input edge, side-tables (stack offset, phi tag, asm fingerprints,
    /// call-other name, clobber override, arg index) shown inline.  No
    /// constant inlining, virtual nodes, or commutative reordering — a
    /// debugging view of the real graph shape, distinct from the pretty
    /// `Lifter.to_dot`/`to_html`.  Returns the string when `path` is
    /// `None`, else writes it to `path` and returns `None`.
    #[pyo3(signature = (path=None))]
    fn to_dot(&self, path: Option<&str>) -> PyResult<Option<String>> {
        let s = self
            .with_read_value(strider_ir::Function::raw_dot)?
            .map_err(crate::errors::into_strider_err)?;
        match path {
            Some(p) => {
                write_to(p, s)?;
                Ok(None)
            }
            None => Ok(Some(s)),
        }
    }

    /// Like `to_dot` but wraps the DOT in a self-contained HTML page
    /// (embedded viz.js; no external `dot` binary needed).
    #[pyo3(signature = (path=None))]
    fn to_html(&self, path: Option<&str>) -> PyResult<Option<String>> {
        let s = self
            .with_read_value(strider_ir::Function::raw_html)?
            .map_err(crate::errors::into_strider_err)?;
        match path {
            Some(p) => {
                write_to(p, s)?;
                Ok(None)
            }
            None => Ok(Some(s)),
        }
    }

    /// Returns the number of node ids in the IR arena — every allocated
    /// slot, reachable or not.  After in-place optimization, culled-but-not-
    /// compacted nodes are still counted; analyze with compaction (or compare
    /// against [`count_regions`], which walks from entry) to exclude them.
    fn node_count(&self) -> PyResult<usize> {
        self.with_read_value(|function| function.graph().all_node_ids().count())
    }

    /// The IR node id of the function's `Entry` node — the natural starting
    /// center for the interactive explorer's neighborhood view.
    fn entry_node(&self) -> PyResult<u32> {
        self.with_read_value(|function| function.entry().as_u32())
    }

    /// Raw (structure-faithful) render of the depth-`depth` neighborhood around
    /// node `center` — the same BFS/budget as the pretty explorer view, but
    /// showing the graph exactly as stored (one `n<id>` box per IR node, edges
    /// as stored, side-tables inline, no virtuals / const-dup / Sleigh names).
    /// Needs no Sleigh, so it lives on `Function`; the debug view for when the
    /// pretty output can't be trusted.
    #[pyo3(signature = (center, depth=5, hub_cap=12, max_nodes=60))]
    fn neighborhood_dot(
        &self,
        center: u32,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
    ) -> PyResult<String> {
        self.with_read_value(|function| {
            let nid = function
                .graph()
                .node_id_from_u32(center)
                .ok_or_else(|| anyhow::anyhow!("invalid node id {center}"))?;
            function.raw_neighborhood_dot(nid, depth, hub_cap, max_nodes)
        })?
        .map_err(crate::errors::into_strider_err)
    }

    /// Returns the count of `Region` (control-flow join) nodes
    /// reachable from entry.  This is a single linear pre-order sweep
    /// using the IR's own kind-filtered walker, whose visited-set is a
    /// `DenseEntitySet<NodeId>` (see [`strider_ir::walk::PreOrder`]),
    /// so it satisfies the "use entity-set bookkeeping" memory
    /// directive by routing through the canonical IR traversal helper.
    fn count_regions(&self) -> PyResult<usize> {
        self.with_read_value(|function| {
            function
                .walk_kind(|k| matches!(k, NodeKind::Region))
                .count()
        })
    }

    /// Returns a list of all node ids in the IR arena (reachable or not) as
    /// raw integers.  Useful for iterating from Python without going
    /// through pattern matching.
    fn node_ids(&self) -> PyResult<Vec<u32>> {
        self.with_read_value(|function| {
            function
                .graph()
                .all_node_ids()
                .map(|n| n.as_u32())
                .collect()
        })
    }

    /// Re-validates the graph and returns `None` on success or a
    /// human-readable error message on failure.
    ///
    /// The asm-fingerprint Layer-C check is always-on: every reachable
    /// non-exempt node must carry a non-empty contributor list.
    fn validate(&self) -> PyResult<Option<String>> {
        self.with_read(|function| match strider_ir::validate::validate(function) {
            Ok(()) => Ok(None),
            Err(e) => Ok(Some(format!("{e}"))),
        })
    }

    /// Compact the graph arena: drop every node not reachable from
    /// `entry` via [`strider_ir::graph::Graph::walk_from`].  Mutates in place.
    /// Pre-compaction node ids become invalid across this call.
    fn compact(&self) -> PyResult<()> {
        let mut function = self
            .try_write_inner()
            .map_err(crate::errors::into_strider_err)?;
        let _remap = function
            .compact()
            .map_err(crate::errors::into_strider_err)?;
        Ok(())
    }

    /// Deep-copy this function into a fully independent `Function`.
    ///
    /// The clone owns a fresh graph + side-tables (its own generation
    /// counter), so mutating it via `rewrite(...)` / `Lifter.optimize(...)`
    /// leaves the original untouched — the idiom for a non-destructive
    /// rewrite is `g2 = fn.clone(); g2.rewrite(find, replace)`.  The parent
    /// `Cfg` (Sleigh for dot rendering) is shared by handle.
    #[pyo3(name = "clone")]
    fn py_clone(&self, py: Python<'_>) -> PyResult<PyFunction> {
        let cloned = self
            .read_inner()
            .map_err(crate::errors::into_strider_err)?
            .clone();
        Ok(PyFunction {
            inner: Rc::new(RefCell::new(cloned)),
            cfg: self.cfg.clone_ref(py),
        })
    }

    /// Find every site where `pat` matches.  `pat` accepts any
    /// `PatLike` (a `Pat`, a typed builder like `CallPat`, a
    /// `Capture`, or a string capture-name) — typed builders (e.g.
    /// `call().arg(0, int_const(8))`) are finalised implicitly so
    /// the call site stays uncluttered by `.into_pat()`.
    ///
    /// Matcher options:
    /// * `ignore_casts=True` — walk through every value-passthrough
    ///   cast `NodeKind` (Extend / Truncate / CastTo* / Bits-cast).
    ///   Equivalent to `ignore_casts_mask=CastMask.all()`.
    /// * `ignore_casts_mask=mask` — granular per-cast walk-through.
    ///   Compose via `CastMask.extend() | CastMask.truncate()`.
    ///   Mutually exclusive with `ignore_casts`; passing both is an
    ///   error.
    #[pyo3(signature = (pat, ignore_root=false, ignore_casts=false, ignore_casts_mask=None, constraints=None))]
    fn find_all(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatQuery<'_>,
        ignore_root: bool,
        ignore_casts: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
        constraints: Option<Vec<PyRef<'_, crate::pattern::PyJoinConstraint>>>,
    ) -> PyResult<Vec<crate::matcher::PyMatch>> {
        reject_conflicting_cast_flags("find_all", ignore_casts, &ignore_casts_mask)?;
        let patterns = build_query_patterns(py, pat, ignore_casts, ignore_casts_mask)?;
        let constraints = collect_constraints(&constraints);
        let (raw, generation) = run_pattern_query(&slf, py, &patterns, &constraints)?;
        dedup_matches(&slf, py, raw, generation, ignore_root)
    }

    /// Find the single binding of `pat`, erroring if there is not exactly
    /// one.  Replaces the `hits = find_all(p); assert len(hits) == 1; hits[0]`
    /// idiom with distinct error messages for the 0-match and >1-match cases.
    ///
    /// `pat`, `ignore_root`, and the matcher options mirror `find_all` — the
    /// count is taken *after* deduplication, so `ignore_root` controls whether
    /// distinct roots binding the same captures count as one or many.
    #[pyo3(signature = (pat, ignore_root=false, ignore_casts=false, ignore_casts_mask=None, constraints=None))]
    fn find_unique(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatQuery<'_>,
        ignore_root: bool,
        ignore_casts: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
        constraints: Option<Vec<PyRef<'_, crate::pattern::PyJoinConstraint>>>,
    ) -> PyResult<crate::matcher::PyMatch> {
        reject_conflicting_cast_flags("find_unique", ignore_casts, &ignore_casts_mask)?;
        let patterns = build_query_patterns(py, pat, ignore_casts, ignore_casts_mask)?;
        let constraints = collect_constraints(&constraints);
        let (raw, generation) = run_pattern_query(&slf, py, &patterns, &constraints)?;
        let mut matches = dedup_matches(&slf, py, raw, generation, ignore_root)?;
        match matches.len() {
            1 => Ok(matches.pop().unwrap()),
            0 => Err(crate::errors::into_strider_err(anyhow::anyhow!(
                "find_unique: expected exactly one match, found none"
            ))),
            n => Err(crate::errors::into_strider_err(anyhow::anyhow!(
                "find_unique: expected exactly one match, found {n}"
            ))),
        }
    }

    /// Apply a single `find → replace` rewrite rule across the graph.
    /// Returns the number of times the rule fired.  `find` accepts
    /// `PatLike` (so e.g. `g.rewrite(find=call().arg(0, …), replace=…)`
    /// works without an explicit `.into_pat()` conversion); `replace`
    /// is typed as `strider.template.Template` — build it via the
    /// `strider.template` free functions (`tpl.var(c)`, `tpl.add(...)`,
    /// …).  A bare `strider.pattern.Pat` (its build-valid subset only),
    /// a `Capture`, or a string capture-name is still accepted for
    /// back-compat.
    ///
    /// The RHS is validated at rule-construction time via
    /// `rewrite_rule_dynamic` — every node must be either a concrete
    /// builder (e.g. `int_const(0)`, `add(...)`) or a capture bound by
    /// the LHS.  Using a wildcard / kind-`Any` shape on the RHS
    /// surfaces a `StriderError` here rather than at first-match time.
    fn rewrite(
        &self,
        py: Python<'_>,
        find: crate::pattern::PatLike<'_>,
        replace: crate::pattern::TemplateLike<'_>,
    ) -> PyResult<usize> {
        let lhs = find.to_pattern(py)?;
        let rhs = replace.to_template(py)?;
        let rule =
            strider_opt::rewrite_rule_runtime(lhs, rhs).map_err(crate::errors::into_strider_err)?;
        let mut function = self
            .try_write_inner()
            .map_err(crate::errors::into_strider_err)?;
        apply_rules_count_on(&mut function, std::slice::from_ref(&rule))
    }

    /// Apply a list of `(find, replace)` pairs across the graph round-
    /// robin at every reachable node.  Returns the total fire count
    /// (sum across pairs and nodes).  `replace` is typed as
    /// `strider.template.Template` — see `rewrite`'s doc comment.
    fn rewrite_all(
        &self,
        py: Python<'_>,
        pairs: Vec<(
            crate::pattern::PatLike<'_>,
            crate::pattern::TemplateLike<'_>,
        )>,
    ) -> PyResult<usize> {
        // Build a match `Pattern` (LHS) and a build `Template` (RHS) per
        // pair, then box each rule.
        let mut rules: Vec<strider_opt::BoxedRule> = Vec::with_capacity(pairs.len());
        for (lhs, rhs) in pairs {
            let lhs_pat = lhs.to_pattern(py)?;
            let rhs_tpl = rhs.to_template(py)?;
            let rule = strider_opt::rewrite_rule_runtime(lhs_pat, rhs_tpl)
                .map_err(crate::errors::into_strider_err)?;
            rules.push(rule);
        }
        let mut function = self
            .try_write_inner()
            .map_err(crate::errors::into_strider_err)?;
        apply_rules_count_on(&mut function, &rules)
    }

    /// Returns a `Node` handle on the node at `node_id`.
    ///
    /// The handle is a discoverable entry point into the IR graph: from
    /// it you can read the node's `kind()`, walk its `inputs()` (which
    /// return more `Node`s), pull out `const_int()` / `const_bool()`
    /// values, and recover the `asm_fingerprint()` — instead of juggling
    /// raw `u32` ids through the typed `node_*` getters.
    ///
    /// Raises `StriderError` for an invalid `node_id`.
    fn node(slf: Py<Self>, py: Python<'_>, node_id: u32) -> PyResult<crate::node::PyNode> {
        crate::node::PyNode::new(py, slf, node_id)
    }
}

/// Reject the mutually-exclusive `ignore_casts` + `ignore_casts_mask`
/// combination, naming `op` (`"find_all"` /
/// `"find_unique"`) in the error so the message points at the caller.
fn reject_conflicting_cast_flags(
    op: &str,
    ignore_casts: bool,
    ignore_casts_mask: &Option<crate::pattern::PyCastMask>,
) -> PyResult<()> {
    if ignore_casts && ignore_casts_mask.is_some() {
        return Err(crate::errors::into_strider_err(anyhow::anyhow!(
            "{op}: pass either ignore_casts=True or ignore_casts_mask=...; not both"
        )));
    }
    Ok(())
}

/// RAII guard pairing a `crate::pattern::push_current_query_function`
/// call with its `pop_current_query_function` counterpart: the pop runs
/// from `Drop`, so it fires on every exit path out of [`run_query`] —
/// normal return, an early `?`, or a Rust panic unwinding through
/// `run` — instead of only the fall-through path a plain function-call
/// pair would cover. Without this, a panic inside `run` would leave a
/// stale `(Py<PyFunction>, u64)` on the thread-local stack forever.
struct QueryFunctionGuard;

impl Drop for QueryFunctionGuard {
    fn drop(&mut self) {
        crate::pattern::pop_current_query_function();
    }
}

/// Run a matcher query and snapshot the generation, collapsing the
/// borrow → `read_inner` → `Matcher::new` → run → generation-snapshot
/// → drop-guards → pending-control-flow scaffold the three query entry
/// points (`find_all` / `find_unique`) share.
///
/// `run` receives the freshly-built `Matcher` and produces the raw match
/// payload; the returned `generation` is what each raw `Match` must be
/// tagged with so a later in-place rewrite / compaction invalidates the
/// derived `PyMatch` handles.
///
/// Pushes `slf` + the sampled generation onto
/// `crate::pattern::CURRENT_QUERY_FUNCTION` for the duration of `run`, so
/// a `.when()` predicate fired from inside the matcher can build a
/// genuine `Match` handle back onto this same live function (patterns
/// are built well before any `Function` is known, so the predicate
/// closure itself can't capture `slf`). The push is paired with its pop
/// via a [`QueryFunctionGuard`] rather than a plain trailing call so the
/// pop still runs if `run` panics.
fn run_query<T>(
    slf: &Py<PyFunction>,
    py: Python<'_>,
    run: impl FnOnce(&strider_pattern::Matcher<'_>) -> anyhow::Result<T>,
) -> PyResult<(T, u64)> {
    let function_borrow = slf.borrow(py);
    let function_guard = function_borrow
        .read_inner()
        .map_err(crate::errors::into_strider_err)?;
    let matcher = strider_pattern::Matcher::new(&function_guard);
    let generation = function_guard.graph().generation();
    crate::pattern::push_current_query_function(slf.clone_ref(py), generation);
    let _guard = QueryFunctionGuard;
    let raw = run(&matcher);
    drop(_guard);
    let raw = raw.map_err(crate::errors::into_strider_err)?;
    drop(function_guard);
    drop(function_borrow);
    // If a `.when()` predicate stashed a control-flow exception
    // (KeyboardInterrupt / SystemExit) or a bad return-type PyErr in the
    // thread-local pending-control-flow cell, surface it here.  See
    // `crate::pattern::PENDING_CONTROL_FLOW` for why a cell is used
    // instead of `PyErr::restore`/`take`: restore would leave the error
    // set between predicate calls and the next `call_bound` would replace
    // the original error with `SystemError`.
    if let Some(err) = crate::pattern::take_pending_control_flow() {
        return Err(err);
    }
    Ok((raw, generation))
}

/// Fold the matcher cast-walk-through flags onto a freshly-built
/// `Pattern`. `ignore_casts` is equivalent to `ignore_casts_mask =
/// CastMask::all()`; the two are mutually exclusive (checked by the
/// caller).
fn apply_cast_mask(
    pattern: strider_pattern::Pattern,
    ignore_casts: bool,
    ignore_casts_mask: Option<crate::pattern::PyCastMask>,
) -> strider_pattern::Pattern {
    if ignore_casts {
        pattern.ignore_casts()
    } else if let Some(m) = ignore_casts_mask {
        pattern.ignore_casts_mask(m.inner)
    } else {
        pattern
    }
}

/// Seal a query input into one `Pattern` per pattern, folding the shared
/// cast-walk-through mask onto each.  A single pattern yields one; a list
/// yields one per element (empty list → no patterns).
fn build_query_patterns(
    py: Python<'_>,
    pat: crate::pattern::PatQuery<'_>,
    ignore_casts: bool,
    ignore_casts_mask: Option<crate::pattern::PyCastMask>,
) -> PyResult<Vec<strider_pattern::Pattern>> {
    Ok(pat
        .to_patterns(py)?
        .into_iter()
        .map(|p| apply_cast_mask(p, ignore_casts, ignore_casts_mask))
        .collect())
}

/// Run the matcher for `patterns`, returning one sub-match group per result:
/// a single pattern maps each `find_all` hit to a one-element group; several
/// patterns join on shared captures (each group holds one sub-match per
/// pattern, which `PyMatch` presents as a merged binding).
/// Unwrap the optional Python constraint list into plain `JoinConstraint`s.
fn collect_constraints(
    constraints: &Option<Vec<PyRef<'_, crate::pattern::PyJoinConstraint>>>,
) -> Vec<strider_pattern::JoinConstraint> {
    constraints
        .as_deref()
        .map(|v| v.iter().map(|c| c.inner.clone()).collect())
        .unwrap_or_default()
}

fn run_pattern_query(
    slf: &Py<PyFunction>,
    py: Python<'_>,
    patterns: &[strider_pattern::Pattern],
    constraints: &[strider_pattern::JoinConstraint],
) -> PyResult<(Vec<Vec<strider_pattern::Match>>, u64)> {
    let refs: Vec<&strider_pattern::Pattern> = patterns.iter().collect();
    run_query(slf, py, |matcher| {
        // Single pattern with no constraints: the fast `find_all` path. Any
        // constraint (even over one pattern's own captures) routes through the
        // constrained join so the CFG filter runs.
        if refs.len() == 1 && constraints.is_empty() {
            Ok(matcher.matches(refs[0])?.map(|m| vec![m]).collect())
        } else {
            matcher.find_joined_constrained(&refs, constraints)
        }
    })
}

/// Wrap each raw sub-match group as a `PyMatch` and deduplicate.  The dedup
/// key is `(roots, capture-signatures)` — or capture-signatures alone when
/// `ignore_root` — so commutative-symmetry and multi-path hits collapse, and
/// `ignore_root` additionally collapses one binding reached from several roots.
fn dedup_matches(
    slf: &Py<PyFunction>,
    py: Python<'_>,
    raw: Vec<Vec<strider_pattern::Match>>,
    generation: u64,
    ignore_root: bool,
) -> PyResult<Vec<crate::matcher::PyMatch>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for inner in raw {
        let m = crate::matcher::PyMatch {
            inner,
            function: slf.clone_ref(py),
            generation,
        };
        if seen.insert(m.dedup_key(py, ignore_root)?) {
            out.push(m);
        }
    }
    Ok(out)
}

/// Drive a slice of rewrite rules round-robin across every reachable
/// node of `function`, returning the total per-`(node, rule)` fire
/// count (Python users assert "this rule fired N times").  The single-
/// rule caller passes `std::slice::from_ref(&rule)`.
///
/// An in-place rewrite mutates the arena without compacting it (node
/// ids stay valid), so it does NOT bump the generation on its own —
/// outstanding `Match` / `Node` handles would silently read the
/// post-rewrite graph.  Bump the generation afterwards so those handles
/// fail their staleness guard.
fn apply_rules_count_on<R>(function: &mut strider_ir::Function, rules: &[R]) -> PyResult<usize>
where
    R: for<'g> Fn(
        &mut strider_opt::EditFunction<'g>,
        strider_ir::node::NodeId,
    ) -> anyhow::Result<Option<strider_ir::node::ValueId>>,
{
    let count = {
        let mut ctx = strider_opt::EditFunction::new(function);
        strider_opt::apply_rules_count(&mut ctx, rules).map_err(crate::errors::into_strider_err)?
    };
    function.graph_mut().bump_generation();
    Ok(count)
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyFunction>()
}
