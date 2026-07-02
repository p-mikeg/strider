//! `PyFunction` — wraps `strider_ir::Function` and exposes dot rendering
//! plus pattern queries and rewrites.
//!
//! The IR graph's dot dumper requires a borrowed `Sleigh` for
//! register-name resolution.  PyFunction keeps a `Py<PyCfg>` reference
//! so the Sleigh stays alive for the graph's lifetime and is
//! reachable through `strider_cfg::Cfg::sleigh`.

use std::sync::{Arc, RwLock, TryLockError};

use pyo3::prelude::*;
use strider_ir::node::NodeKind;
use strider_ir::IRWalker;

use crate::cfg::PyCfg;

/// Opaque wrapper over `strider_ir::Function`.
///
/// The graph is held in `Arc<RwLock<...>>` so optimization passes can
/// mutate it without requiring `&mut self` on the PyFunction wrapper,
/// and so the same graph can be shared across multiple Python
/// references.
#[pyclass(name = "Function", module = "strider")]
pub struct PyFunction {
    pub(crate) inner: Arc<RwLock<strider_ir::Function>>,
    /// Strong reference to the parent Cfg; keeps the Sleigh alive for
    /// dot rendering and ensures destruction order is graph-then-cfg.
    pub(crate) cfg: Py<PyCfg>,
}

impl PyFunction {
    pub(crate) fn new(function: strider_ir::Function, cfg: Py<PyCfg>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(function)),
            cfg,
        }
    }

    /// Borrow the inner graph for read.  Returns an `anyhow::Error`
    /// when the lock is poisoned.
    pub(crate) fn read_inner(
        &self,
    ) -> anyhow::Result<std::sync::RwLockReadGuard<'_, strider_ir::Function>> {
        self.inner
            .read()
            .map_err(|_| anyhow::anyhow!("Function lock poisoned"))
    }

    /// Try to acquire the write lock without blocking.  Used by mutating
    /// methods (`optimize`, `compact`, `rewrite`, `reoptimize`) so that a
    /// re-entrant call from inside a `.when()` predicate (which holds the
    /// read lock for the duration of `find_all`) surfaces a typed error
    /// rather than deadlocking the thread.
    pub(crate) fn try_write_inner(
        &self,
    ) -> anyhow::Result<std::sync::RwLockWriteGuard<'_, strider_ir::Function>> {
        self.inner.try_write().map_err(|e| match e {
            TryLockError::Poisoned(_) => anyhow::anyhow!("Function lock poisoned"),
            TryLockError::WouldBlock => anyhow::anyhow!(
                "Function mutation rejected: the function is currently borrowed for read \
                 (typically because this call is from inside a `.when()` predicate \
                 invoked by `find_all`/`find_joined`).  Mutating the function \
                 from within a pattern predicate is not supported — collect matches \
                 first and mutate after `find_all` returns."
            ),
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
    /// the operation in the surfaced error (`"optimize"` / `"reoptimize"`).
    fn run_pipeline_in_place(
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
/// Shared by the `to_raw_dot` / `to_raw_html` file-dump methods.
fn write_to(path: &str, contents: String) -> PyResult<()> {
    std::fs::write(path, contents).map_err(|e| crate::errors::into_strider_err(anyhow::anyhow!(e)))
}

#[pymethods]
impl PyFunction {
    /// The snapshot `Cfg` this function was lifted from — kept alive for
    /// dot rendering (its `Sleigh` resolves register names).  Combine
    /// with `Lifter.dump_html(function, path)` (or the `Cfg`'s own
    /// `to_html`) for a self-describing render without a separate result
    /// wrapper.
    #[getter(cfg)]
    fn get_cfg(&self, py: Python<'_>) -> Py<PyCfg> {
        self.cfg.clone_ref(py)
    }

    /// Render the graph **exactly as stored** to a Graphviz `.dot` string:
    /// one node per `NodeId` (every arena node, incl. detached ones), one
    /// edge per input edge, side-tables (stack offset, phi tag, asm
    /// fingerprints, call-other name, clobber override, arg index) shown
    /// inline.  No constant inlining, virtual nodes, or commutative
    /// reordering — a debugging view of the real graph shape, distinct from
    /// the pretty `to_dot`/`html_str`.
    fn raw_dot_str(&self) -> PyResult<String> {
        self.with_read_value(strider_ir::Function::raw_dot)?
            .map_err(crate::errors::into_strider_err)
    }

    /// Like `raw_dot_str` but wraps the DOT in a self-contained HTML page
    /// (embedded viz.js; no external `dot` binary needed).
    fn raw_html_str(&self) -> PyResult<String> {
        self.with_read_value(strider_ir::Function::raw_html)?
            .map_err(crate::errors::into_strider_err)
    }

    /// Write the raw (as-stored) Graphviz `.dot` rendering to `path`.
    /// See `raw_dot_str` for what "raw" means.
    fn to_raw_dot(&self, path: &str) -> PyResult<()> {
        write_to(path, self.raw_dot_str()?)
    }

    /// Write the raw (as-stored) standalone HTML rendering to `path`.
    /// See `raw_dot_str` for what "raw" means.
    fn to_raw_html(&self, path: &str) -> PyResult<()> {
        write_to(path, self.raw_html_str()?)
    }

    /// Returns the number of node ids in the IR arena — every allocated
    /// slot, reachable or not.  After in-place optimization, culled-but-not-
    /// compacted nodes are still counted; analyze with compaction (or compare
    /// against [`count_regions`], which walks from entry) to exclude them.
    fn node_count(&self) -> PyResult<usize> {
        self.with_read_value(|function| function.graph().all_node_ids().count())
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

    /// Apply a `PyOptimizerPipeline` to this graph in place.  Drains
    /// the pipeline (subsequent calls to the same pipeline see an
    /// empty pass list); rebuild it from `OptimizerPipeline.default()`
    /// or the equivalent classmethods if you need to apply it again.
    ///
    /// The pipeline runs without a rom image (`OptCtx::new(None)`); any
    /// `LoadReadOnly` pass present in the pipeline short-circuits
    /// silently.  Callers that need rom-driven folding should route
    /// through `strider.run(..., rom=mem)` instead.
    fn optimize(&self, pipeline: &crate::opt::PyOptimizerPipeline) -> PyResult<()> {
        let real_pipeline = pipeline.drain_into_pipeline(false)?;
        self.run_pipeline_in_place(real_pipeline, "optimize")
    }

    /// Convenience: re-run the default optimizer pipeline on this graph.
    /// Useful after a manual rewrite (`graph.rewrite(...)`) to
    /// re-converge the graph.
    fn reoptimize(&self) -> PyResult<()> {
        let pipe = strider_orchestrator::opt::default_pipeline();
        self.run_pipeline_in_place(pipe, "reoptimize")
    }

    /// Deep-copy this function into a fully independent `Function`.
    ///
    /// The clone owns a fresh graph + side-tables (its own generation
    /// counter), so mutating it via `rewrite(...)` / `reoptimize()` leaves the
    /// original untouched — the idiom for a non-destructive rewrite is
    /// `g2 = fn.clone(); g2.rewrite(find, replace)`.  The parent `Cfg` (Sleigh
    /// for dot rendering) is shared by handle.
    #[pyo3(name = "clone")]
    fn py_clone(&self, py: Python<'_>) -> PyResult<PyFunction> {
        let cloned = self
            .read_inner()
            .map_err(crate::errors::into_strider_err)?
            .clone();
        Ok(PyFunction {
            inner: Arc::new(RwLock::new(cloned)),
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
    #[pyo3(signature = (pat, ignore_casts=false, ignore_casts_mask=None))]
    fn find_all(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatLike<'_>,
        ignore_casts: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
    ) -> PyResult<Vec<crate::matcher::PyMatch>> {
        reject_conflicting_cast_flags("find_all", ignore_casts, &ignore_casts_mask)?;
        // The cast-walk-through mask now lives on the `Pattern`; build a
        // fresh `Pattern` per query and fold the mask onto it.
        let pattern = apply_cast_mask(pat.to_pattern(py)?, ignore_casts, ignore_casts_mask);
        let (raw, generation) = run_query(&slf, py, |matcher| matcher.find_all(&pattern))?;
        let mut out = Vec::with_capacity(raw.len());
        for m in raw {
            out.push(crate::matcher::PyMatch {
                inner: m,
                function: slf.clone_ref(py),
                generation,
            });
        }
        Ok(out)
    }

    /// Find the first site where `pat` matches, or `None` if nothing
    /// matches.  A one-shot convenience over `find_all` for the common
    /// `hits = find_all(p); hits[0] if hits else None` idiom — it
    /// short-circuits on the first match in the Rust matcher rather than
    /// collecting every hit.
    ///
    /// `pat` and the matcher options (`ignore_casts`,
    /// `ignore_casts_mask`) mirror `find_all`.  The returned `Match`
    /// is the same as `find_all`'s first element.
    #[pyo3(signature = (pat, ignore_casts=false, ignore_casts_mask=None))]
    fn find_one(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatLike<'_>,
        ignore_casts: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
    ) -> PyResult<Option<crate::matcher::PyMatch>> {
        reject_conflicting_cast_flags("find_one", ignore_casts, &ignore_casts_mask)?;
        let pattern = apply_cast_mask(pat.to_pattern(py)?, ignore_casts, ignore_casts_mask);
        let (raw, generation) = run_query(&slf, py, |matcher| matcher.find_first(&pattern))?;
        Ok(raw.map(|m| crate::matcher::PyMatch {
            inner: m,
            function: slf.clone_ref(py),
            generation,
        }))
    }

    /// Run multiple patterns and intersect their matches on shared
    /// `Capture` objects.  Returns one tuple per joined match — each
    /// tuple holds one `Match` per input pattern (in input order),
    /// where every `Capture` appearing in more than one pattern binds
    /// to the same node (and value output, when applicable) across
    /// every pattern in which it appears.
    ///
    /// Use case: `find K and shared such that store(<shared>+K, 0)
    /// AND call(at=F).arg(0, <shared>) both match with the same
    /// <shared> binding`.  Today an equivalent query requires
    /// post-filtering the cross-product of two `find_all` calls in
    /// Python; this routes it to the matcher in one pass with
    /// shared-capture filtering done at the binding level.
    ///
    /// Edge cases:
    ///
    /// * Empty `pats` → empty list.
    /// * Single pattern → equivalent to wrapping each `find_all` hit
    ///   in a one-element tuple.
    /// * Any pattern with zero matches → empty result.
    ///
    /// The matcher walk-through flags (`ignore_casts`,
    /// `ignore_casts_mask`) apply uniformly to every pattern, mirroring
    /// `find_all`.
    #[pyo3(signature = (pats, ignore_casts=false, ignore_casts_mask=None))]
    fn find_joined(
        slf: Py<Self>,
        py: Python<'_>,
        pats: Vec<crate::pattern::PatLike<'_>>,
        ignore_casts: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
    ) -> PyResult<Vec<Vec<crate::matcher::PyMatch>>> {
        reject_conflicting_cast_flags("find_joined", ignore_casts, &ignore_casts_mask)?;
        // Build a fresh `Pattern` per input (the cast mask is folded onto
        // each), then pass `&[&Pattern]` to the matcher.
        let owned: Vec<strider_pattern::Pattern> = pats
            .iter()
            .map(|p| {
                Ok(apply_cast_mask(
                    p.to_pattern(py)?,
                    ignore_casts,
                    ignore_casts_mask,
                ))
            })
            .collect::<PyResult<Vec<_>>>()?;
        let pat_refs: Vec<&strider_pattern::Pattern> = owned.iter().collect();
        let (raw, generation) = run_query(&slf, py, |matcher| matcher.find_joined(&pat_refs))?;
        let mut out: Vec<Vec<crate::matcher::PyMatch>> = Vec::with_capacity(raw.len());
        for tuple in raw {
            let mut py_tuple: Vec<crate::matcher::PyMatch> = Vec::with_capacity(tuple.len());
            for m in tuple {
                py_tuple.push(crate::matcher::PyMatch {
                    inner: m,
                    function: slf.clone_ref(py),
                    generation,
                });
            }
            out.push(py_tuple);
        }
        Ok(out)
    }

    /// Apply a single `find → replace` rewrite rule across the graph.
    /// Returns the number of times the rule fired.  Both `find` and
    /// `replace` accept `PatLike` (so e.g.
    /// `g.rewrite(find=call().arg(0, …), replace=…)` works without
    /// an explicit `.into_pat()` conversion).
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
        replace: crate::pattern::PatLike<'_>,
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
    /// (sum across pairs and nodes).
    fn rewrite_all(
        &self,
        py: Python<'_>,
        pairs: Vec<(crate::pattern::PatLike<'_>, crate::pattern::PatLike<'_>)>,
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
    /// values, and recover the `fingerprint()` — instead of juggling raw
    /// `u32` ids through the typed `node_*` getters.
    ///
    /// Raises `StriderError` for an invalid `node_id`.
    fn node(slf: Py<Self>, py: Python<'_>, node_id: u32) -> PyResult<crate::node::PyNode> {
        crate::node::PyNode::new(py, slf, node_id)
    }
}

/// Reject the mutually-exclusive `ignore_casts` + `ignore_casts_mask`
/// combination, naming `op` (`"find_all"` / `"find_one"` /
/// `"find_joined"`) in the error so the message points at the caller.
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

/// Run a matcher query and snapshot the generation, collapsing the
/// borrow → `read_inner` → `Matcher::new` → run → generation-snapshot
/// → drop-guards → pending-control-flow scaffold the three query entry
/// points (`find_all` / `find_one` / `find_joined`) share.
///
/// `run` receives the freshly-built `Matcher` and produces the raw match
/// payload; the returned `generation` is what each raw `Match` must be
/// tagged with so a later in-place rewrite / compaction invalidates the
/// derived `PyMatch` handles.
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
    let raw = run(&matcher).map_err(crate::errors::into_strider_err)?;
    let generation = function_guard.graph().generation();
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
