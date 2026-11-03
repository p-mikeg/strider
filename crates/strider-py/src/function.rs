use std::cell::{Ref, RefCell};

use pyo3::prelude::*;
use strider_ir::IRWalker;
use strider_ir::node::NodeKind;

use crate::cfg::PyCfg;
use crate::dot::Pretty;
use crate::errors::into_strider_err;

/// A lifted IR function: pattern queries, rewrites, walks and dot rendering.
#[pyclass(name = "Function", module = "strider.ir", frozen)]
pub struct PyFunction {
    pub(crate) inner: RefCell<strider_ir::Function>,
    /// The `Cfg` this function was lifted from.
    pub(crate) cfg: Py<PyCfg>,
}

impl PyFunction {
    pub(crate) fn new(function: strider_ir::Function, cfg: Py<PyCfg>) -> Self {
        Self {
            inner: RefCell::new(function),
            cfg,
        }
    }

    /// `to_dot` / `to_html`: the graph as stored, or the pretty render in
    /// `pretty`'s theme, naming registers through the arch's table rather
    /// than a decoder.
    fn render(&self, path: Option<&str>, pretty: &Pretty, html: bool) -> PyResult<Option<String>> {
        let function = self.read()?;
        let Some(style) = pretty.theme() else {
            return crate::dot::render(&function.raw_graph_dot(), html, path);
        };
        let arch = self.cfg.get().arch;
        let d = dot::GraphDot::new(
            function.dot_dumper(arch.names()),
            crate::dot::dot_style_for(style)?,
        );
        crate::dot::render(&d, html, path)
    }

    pub(crate) fn read(&self) -> PyResult<Ref<'_, strider_ir::Function>> {
        self.inner.try_borrow().map_err(|_| {
            into_strider_err(anyhow::anyhow!(
                "Function is currently borrowed for mutation"
            ))
        })
    }

    /// Run `f` on the function borrowed for mutation, moving the generation
    /// first: a mutation erroring part-way leaves the graph rewritten, and a
    /// bump after the `?` never happens.
    pub(crate) fn edit<R>(
        &self,
        f: impl FnOnce(&mut strider_ir::Function) -> anyhow::Result<R>,
    ) -> PyResult<R> {
        let mut function = self.inner.try_borrow_mut().map_err(|_| {
            into_strider_err(anyhow::anyhow!(
                "Function mutation rejected: the function is currently borrowed for read \
                 (typically because this call is from inside a `.when()` predicate \
                 invoked by `find_all`/`find_unique`).  Mutating the function \
                 from within a pattern predicate is not supported: collect matches \
                 first and mutate after `find_all` returns."
            ))
        })?;
        function.graph_mut().bump_generation();
        f(&mut function).map_err(into_strider_err)
    }
}

#[pymethods]
impl PyFunction {
    /// Exposes the strong `cfg` back-reference so the cyclic GC can see a
    /// cycle routed through a `Function`. The cycle is broken at the reader's
    /// `__dict__` / `PyLifter::__clear__`, and `cfg` is load-bearing for as
    /// long as the `Function` lives.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.cfg)
    }

    /// The `Cfg` this function was lifted from.
    #[getter(cfg)]
    fn get_cfg(&self, py: Python<'_>) -> Py<PyCfg> {
        self.cfg.clone_ref(py)
    }

    /// The claims in force: the ones `analyze` ran under, plus every set
    /// `optimize` added since. A copy; no optimize withdraws a claim,
    /// and none adds `msr_register_keeps_mode` or `fp_rounding_mode_is_nearest`,
    /// which only the lift reads.
    #[getter]
    fn assumptions(&self) -> PyResult<crate::options::PyAssumptionOptions> {
        Ok(crate::options::PyAssumptionOptions {
            inner: self.read()?.assumptions().clone(),
        })
    }

    /// Run an optimizer pipeline over this graph in place.  `pipeline=None`
    /// runs the default pipeline; a given `OptimizerPipeline` is copied, so it
    /// stays usable.  The claims `assumptions` holds join `self.assumptions`
    /// and none is withdrawn: `assumptions=None` adds nothing, and a set that
    /// leaves out a claim already in force, or adds `msr_register_keeps_mode`
    /// or `fp_rounding_mode_is_nearest` (read by the lift, so analyse again
    /// with it), raises `StriderError` before anything runs.  Add one with::
    ///
    ///     function.optimize(assumptions=function.assumptions
    ///                       | AssumptionOptions.none(escape_analysis=True))
    ///
    /// Runs against the rom of the `Lifter` that produced this function, so
    /// `LoadReadOnly` folds here exactly as it does inside `analyze`, and
    /// raises `StriderError` while that `Lifter` is in an `analyze`,
    /// `build_cfg` or `add_elf`.  Invalidates outstanding `Node` / `Match`
    /// handles for this function.
    ///
    /// Holds the GIL for the whole run, unlike `analyze`.
    #[pyo3(signature = (assumptions=None, pipeline=None))]
    fn optimize(
        &self,
        py: Python<'_>,
        assumptions: Option<PyRef<'_, crate::options::PyAssumptionOptions>>,
        pipeline: Option<Bound<'_, crate::opt::PyOptimizerPipeline>>,
    ) -> PyResult<()> {
        let lifter = self.cfg.get().lifter.clone_ref(py);
        crate::strider_cls::optimize_function(
            lifter.bind(py),
            self,
            pipeline.map(|p| p.borrow().build_pipeline()),
            assumptions.as_deref(),
        )
    }

    /// Render the IR graph to Graphviz DOT. Returns the string when `path` is
    /// `None`, else writes it to `path` and returns `None`.
    ///
    /// `pretty=False` (the default) renders the graph exactly as stored: one
    /// node per node id reachable from entry, one edge per input edge,
    /// side-tables inline, no constant inlining or virtual nodes.
    ///
    /// `pretty=True` inlines constants, adds virtual nodes and resolves
    /// register names through the arch's name table, which needs no decoder.
    /// A theme name in place of `True` picks the dot theme.
    #[pyo3(signature = (path=None, *, pretty=Pretty::Flag(false)))]
    fn to_dot(&self, path: Option<&str>, pretty: Pretty) -> PyResult<Option<String>> {
        self.render(path, &pretty, /* html */ false)
    }

    /// Like `to_dot` but wraps the DOT in a self-contained HTML page (embedded
    /// viz.js, no external `dot` binary).
    #[pyo3(signature = (path=None, *, pretty=Pretty::Flag(false)))]
    fn to_html(&self, path: Option<&str>, pretty: Pretty) -> PyResult<Option<String>> {
        self.render(path, &pretty, /* html */ true)
    }

    /// The canonical text form: a header, then one line per node reachable
    /// from entry, values named in print order, so equal graphs print equal
    /// text.
    #[pyo3(signature = (fingerprints = false))]
    fn to_text(&self, fingerprints: bool) -> PyResult<String> {
        Ok(self.read()?.to_text(fingerprints))
    }

    /// Total number of node ids in the graph, whether or not they are
    /// reachable from entry.
    fn node_count(&self) -> PyResult<usize> {
        Ok(self.read()?.graph().all_node_ids().count())
    }

    /// Node id of the function's `Entry` node.
    fn entry_node(&self) -> PyResult<u32> {
        Ok(self.read()?.entry().as_u32())
    }

    fn __repr__(&self) -> PyResult<String> {
        let function = self.read()?;
        Ok(format!(
            "Function(entry=#{}, {} nodes)",
            function.entry().as_u32(),
            function.graph().all_node_ids().count(),
        ))
    }

    /// Render the depth-`depth` neighborhood around node `center`.
    ///
    /// `pretty=False` (the default) draws the nodes exactly as stored;
    /// `pretty=True` inlines constants, adds virtual nodes and resolves
    /// register names through the arch's name table.
    #[pyo3(signature = (center, depth=5, hub_cap=12, max_nodes=60, count_producers=false, *, pretty=false))]
    #[allow(clippy::too_many_arguments)]
    fn neighborhood_dot(
        &self,
        center: u32,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
        count_producers: bool,
        pretty: bool,
    ) -> PyResult<String> {
        let arch = pretty.then(|| self.cfg.get().arch);
        let function = self.read()?;
        let nid = function
            .graph()
            .node_id_from_u32(center)
            .ok_or_else(|| into_strider_err(anyhow::anyhow!("invalid node id {center}")))?;
        match arch {
            Some(arch) => function.dot_dumper(arch.names()).neighborhood_dot(
                nid,
                depth,
                hub_cap,
                max_nodes,
                count_producers,
            ),
            None => function.raw_neighborhood_dot(nid, depth, hub_cap, max_nodes, count_producers),
        }
        .map_err(into_strider_err)
    }

    /// Number of `Region` (control-flow join) nodes reachable from entry.
    fn count_regions(&self) -> PyResult<usize> {
        let function = self.read()?;
        Ok(function
            .walk_kind(|k| matches!(k, NodeKind::Region))
            .count())
    }

    /// Every node id in the graph, reachable or not, as raw integers.
    fn node_ids(&self) -> PyResult<Vec<u32>> {
        Ok(self
            .read()?
            .graph()
            .all_node_ids()
            .map(|n| n.as_u32())
            .collect())
    }

    /// Re-validate the graph: `None` on success, else an error message.
    fn validate(&self) -> PyResult<Option<String>> {
        let function = self.read()?;
        Ok(strider_ir::validate::validate(&function)
            .err()
            .map(|e| e.report(&function).to_string()))
    }

    /// Drop every node unreachable from `entry`. Mutates in place, and
    /// invalidates all pre-compaction node ids.
    fn compact(&self) -> PyResult<()> {
        self.edit(|f| {
            f.compact();
            Ok(())
        })
    }

    /// Deep-copy into an independent `Function`, leaving this one untouched.
    /// The parent `Cfg` is shared.
    #[pyo3(name = "clone")]
    fn py_clone(&self, py: Python<'_>) -> PyResult<PyFunction> {
        let cloned = self.read()?.clone();
        Ok(PyFunction::new(cloned, self.cfg.clone_ref(py)))
    }

    /// Find every site where `pat` matches. `pat` takes a `Pat`, a typed
    /// builder like `CallPat`, or a `Capture`; typed builders are finalised
    /// implicitly, no `.into_pat()` needed. A LIST of
    /// patterns joins on shared captures; `constraints=[...]` (JoinConstraint /
    /// JoinPredicate) then filters the joined tuples.
    ///
    /// `ignore_casts` walks through value-passthrough casts (Extend /
    /// Truncate / bits-reinterpret): `True` for every kind, or a `CastMask`
    /// (`CastMask.extend() | CastMask.truncate()`) for a chosen few.
    #[pyo3(signature = (pat, ignore_root=false, ignore_casts=IgnoreCasts::Flag(false), constraints=None))]
    fn find_all(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatQuery<'_>,
        ignore_root: bool,
        ignore_casts: IgnoreCasts,
        constraints: Option<Vec<Bound<'_, PyAny>>>,
    ) -> PyResult<Vec<crate::matcher::PyMatch>> {
        query(&slf, py, pat, &ignore_casts, constraints, ignore_root)
    }

    /// Find the single binding of `pat`, erroring if there is not exactly one.
    /// Arguments mirror `find_all`. The count is taken after deduplication, so
    /// `ignore_root` decides whether distinct roots binding the same captures
    /// count as one match or many.
    ///
    /// `ignore_root=True` with a pattern that binds NO captures leaves every
    /// match keyed identically, so this succeeds on any number of occurrences
    /// and returns an arbitrary one. To ask "does this shape occur exactly
    /// once", leave `ignore_root` false or give the pattern a capture.
    #[pyo3(signature = (pat, ignore_root=false, ignore_casts=IgnoreCasts::Flag(false), constraints=None))]
    fn find_unique(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatQuery<'_>,
        ignore_root: bool,
        ignore_casts: IgnoreCasts,
        constraints: Option<Vec<Bound<'_, PyAny>>>,
    ) -> PyResult<crate::matcher::PyMatch> {
        let mut matches = query(&slf, py, pat, &ignore_casts, constraints, ignore_root)?;
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

    /// The single distinct constant value bound to `capture` across all matches
    /// of `pat`, deduplicated by VALUE (not by node): `None` when no match
    /// binds a constant there, the value when every match agrees, and
    /// `StriderError` when two or more distinct values are bound. Unlike
    /// `find_unique`, matches that differ only structurally but agree on the
    /// value collapse to one.
    ///
    /// `signed=True` reads the constant as two's-complement (so `-8` rather
    /// than a large unsigned bit pattern), the right choice for a stack /
    /// struct offset. `pat` and `constraints` mirror `find_all`: a LIST of
    /// patterns joins on shared captures, and `constraints=[...]` filters the
    /// joined tuples.
    #[pyo3(signature = (pat, capture, ignore_casts=IgnoreCasts::Flag(false), constraints=None, signed=false))]
    fn find_unique_value(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatQuery<'_>,
        capture: crate::matcher::CaptureKey<'_>,
        ignore_casts: IgnoreCasts,
        constraints: Option<Vec<Bound<'_, PyAny>>>,
        signed: bool,
    ) -> PyResult<Option<PyObject>> {
        let cap = capture.resolve()?;
        let matches = query(&slf, py, pat, &ignore_casts, constraints, true)?;
        if signed {
            unique_value(
                py,
                &matches,
                cap,
                crate::node::sint_of,
                crate::bigint::ibig_to_py_int,
            )
        } else {
            unique_value(
                py,
                &matches,
                cap,
                crate::node::uint_of,
                crate::bigint::ubig_to_py_int,
            )
        }
    }

    /// Apply one find/replace rewrite rule across the graph, returning the
    /// number of times it fired. `find` takes any pattern-like value;
    /// `replace` is a `strider.template.Template` built from the
    /// `strider.template` free functions, a `Capture` or a capture's name.
    ///
    /// The RHS is validated up front: every node must be a concrete builder or
    /// a capture bound by the LHS. A `.when()` predicate on the LHS is
    /// rejected: the function is held for mutation while the rule fires.
    ///
    /// A match the replacement cannot stand in for is left alone and not
    /// counted: a replacement of another width than the matched value, or, for
    /// a `find` that matches through a phi or region, one that would read the
    /// matched value or is not available everywhere that value is read.
    ///
    /// Every outstanding `Node` handle goes stale, a return of 0 included: the
    /// graph generation moves before the rule runs, since a rule erroring
    /// part-way leaves the graph rewritten.
    fn rewrite(
        &self,
        find: crate::pattern::PatLike<'_>,
        replace: crate::pattern::TemplateLike<'_>,
    ) -> PyResult<usize> {
        self.rewrite_all(vec![(find, replace)])
    }

    /// Apply the first matching `(find, replace)` pair at every reachable
    /// node, returning the total fire count. See `rewrite`.
    fn rewrite_all(
        &self,
        pairs: Vec<(
            crate::pattern::PatLike<'_>,
            crate::pattern::TemplateLike<'_>,
        )>,
    ) -> PyResult<usize> {
        let templates: Vec<_> = pairs
            .iter()
            .map(|(_, rhs)| rhs.to_template())
            .collect::<PyResult<_>>()?;
        let rules: Vec<_> = pairs
            .iter()
            .zip(templates)
            .map(|((lhs, _), rhs)| {
                let lhs = crate::pattern::compile_rewrite_lhs(lhs)?;
                strider_opt::rewrite_rule_runtime(lhs, rhs).map_err(into_strider_err)
            })
            .collect::<PyResult<_>>()?;
        self.edit(|f| {
            strider_opt::apply_rules_count(&mut strider_opt::EditFunction::new(f), &rules)
        })
    }

    /// A `Node` handle on the node at `node_id`. Raises `StriderError` for an
    /// invalid `node_id`.
    ///
    /// A raw id is meaningful only in the graph generation it came from:
    /// `compact` renumbers, and `optimize` / `rewrite` rewire nodes out from
    /// under it, so an id held across either can name something else or
    /// nothing. A `Node` carries its generation and goes stale instead; a bare
    /// int cannot.
    fn node(slf: Py<Self>, py: Python<'_>, node_id: u32) -> PyResult<crate::node::PyNode> {
        let generation = {
            let function = slf.borrow(py);
            let function = function.read()?;
            crate::node::node_id(&function, node_id)?;
            function.graph().generation()
        };
        Ok(crate::node::PyNode {
            at: FunctionAt::new(slf, generation),
            id: node_id,
        })
    }

    /// Control-only reachability (the CFG skeleton) from the entry, in
    /// ascending node-id order.
    fn cfg_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        Self::nodes(slf, py, |function| {
            Ok(
                strider_ir::walk::cfg_reachable(function.graph(), function.entry())
                    .iter()
                    .collect(),
            )
        })
    }

    /// Every node reachable from the entry (data-in + control-out), pre-order.
    fn data_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        Self::nodes(slf, py, |function| Ok(function.walk().collect()))
    }

    /// Memory-touching nodes (Load / Store / Call / CallOther / MemPhi plus
    /// the InitialMemory root) reached by following memory-token edges forward
    /// from InitialMemory, in pre-order, each once. At a `MemPhi` join a node
    /// can come before one of its own memory predecessors.
    fn mem_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        Self::nodes(slf, py, |function| {
            Ok(strider_ir::walk::memory_reachable(function))
        })
    }

    /// Every node reachable from `node_id` (data-in + control-out), pre-order.
    fn walk(slf: Py<Self>, py: Python<'_>, node_id: u32) -> PyResult<Vec<crate::node::PyNode>> {
        Self::nodes(slf, py, |function| {
            let nid = crate::node::node_id(function, node_id)?;
            Ok(function.walk_from(nid).collect())
        })
    }
}

impl PyFunction {
    /// A `Node` handle on each node `walk` names.
    fn nodes(
        slf: Py<Self>,
        py: Python<'_>,
        walk: impl FnOnce(&strider_ir::Function) -> PyResult<Vec<strider_ir::node::NodeId>>,
    ) -> PyResult<Vec<crate::node::PyNode>> {
        let (ids, generation) = {
            let function = slf.borrow(py);
            let function = function.read()?;
            (walk(&function)?, function.graph().generation())
        };
        let at = FunctionAt::new(slf, generation);
        Ok(ids.into_iter().map(|id| at.node(py, id)).collect())
    }
}

/// A `Function` at the graph generation a `Node` or `Match` was built
/// against: the one route from either back to the graph.
pub(crate) struct FunctionAt {
    pub(crate) function: Py<PyFunction>,
    pub(crate) generation: u64,
}

impl FunctionAt {
    pub(crate) fn new(function: Py<PyFunction>, generation: u64) -> Self {
        Self {
            function,
            generation,
        }
    }

    pub(crate) fn clone_ref(&self, py: Python<'_>) -> Self {
        Self::new(self.function.clone_ref(py), self.generation)
    }

    /// A `Node` handle on `node`, a node of this generation.
    pub(crate) fn node(
        &self,
        py: Python<'_>,
        node: strider_ir::node::NodeId,
    ) -> crate::node::PyNode {
        crate::node::PyNode {
            at: self.clone_ref(py),
            id: node.as_u32(),
        }
    }

    /// Run `f` on the function, borrowed for read, unless the graph moved on
    /// since this generation; `what` names the stale handle in the error.
    pub(crate) fn read<R>(
        &self,
        py: Python<'_>,
        what: &str,
        f: impl FnOnce(&strider_ir::Function) -> R,
    ) -> PyResult<R> {
        let function = self.function.borrow(py);
        let function = function.read()?;
        let now = function.graph().generation();
        if now != self.generation {
            return Err(into_strider_err(anyhow::anyhow!(
                "{what} is stale: the function was compacted or rewritten after this \
                 {what} was created (generation {} -> {now}); fetch it again from the \
                 function",
                self.generation,
            )));
        }
        Ok(f(&function))
    }
}

impl PartialEq for FunctionAt {
    fn eq(&self, other: &Self) -> bool {
        self.function.is(&other.function) && self.generation == other.generation
    }
}

impl std::hash::Hash for FunctionAt {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.function.as_ptr() as usize).hash(state);
        self.generation.hash(state);
    }
}

/// The one distinct constant value `read` finds for `cap` across `matches`.
fn unique_value<T: Ord>(
    py: Python<'_>,
    matches: &[crate::matcher::PyMatch],
    cap: strider_pattern::Capture,
    read: fn(&strider_ir::Function, strider_ir::node::NodeId) -> Option<T>,
    to_py: fn(Python<'_>, &T) -> PyResult<PyObject>,
) -> PyResult<Option<PyObject>> {
    let mut values = std::collections::BTreeSet::new();
    for m in matches {
        values.extend(m.read_bound(py, cap, read)?);
    }
    match values.len() {
        0 => Ok(None),
        1 => Ok(Some(to_py(py, values.first().unwrap())?)),
        n => Err(crate::errors::into_strider_err(anyhow::anyhow!(
            "find_unique_value: capture binds {n} distinct constant values; \
             use find_all to see them"
        ))),
    }
}

/// Pops `crate::pattern::CURRENT_QUERY_FUNCTION` on every exit path out of
/// [`run_query`], including a panic.
struct QueryFunctionGuard;

impl Drop for QueryFunctionGuard {
    fn drop(&mut self) {
        crate::pattern::pop_current_query_function();
    }
}

/// Run a matcher query, returning its result and the function at the
/// generation it ran against.
///
/// Pushes that onto `crate::pattern::CURRENT_QUERY_FUNCTION` for the duration
/// of `run`, so a `.when()` predicate can build a `Match` handle back onto this
/// live function.
fn run_query<T>(
    slf: &Py<PyFunction>,
    py: Python<'_>,
    run: impl FnOnce(&strider_pattern::Matcher<'_>) -> anyhow::Result<T>,
) -> PyResult<(T, FunctionAt)> {
    let function_borrow = slf.borrow(py);
    let function_guard = function_borrow.read()?;
    let matcher = strider_pattern::Matcher::new(&function_guard);
    let at = FunctionAt::new(slf.clone_ref(py), function_guard.graph().generation());
    crate::pattern::push_current_query_function(at.clone_ref(py));
    let _guard = QueryFunctionGuard;
    let raw = run(&matcher);
    drop(_guard);
    drop(function_guard);
    drop(function_borrow);
    // Surface anything a `.when()` predicate stashed: a control-flow exception
    // (KeyboardInterrupt / SystemExit) or a bad-return-type PyErr. A
    // thread-local cell, not `PyErr::restore`/`take`, because restore leaves
    // the error set between predicate calls and the next `call_bound` would
    // replace the original with `SystemError`. Drained on the error path too,
    // so a later, unrelated query cannot inherit it.
    let raw = crate::strider_cls::with_pending_control_flow(|| raw.map_err(into_strider_err))?;
    Ok((raw, at))
}

/// The query-level `ignore_casts=` argument: `True` is `CastMask::all()`.
#[derive(FromPyObject)]
pub enum IgnoreCasts {
    Flag(bool),
    Mask(crate::pattern::PyCastMask),
}

impl IgnoreCasts {
    fn mask(&self) -> strider_pattern::CastMask {
        match self {
            IgnoreCasts::Flag(false) => strider_pattern::CastMask::empty(),
            IgnoreCasts::Flag(true) => strider_pattern::CastMask::all(),
            IgnoreCasts::Mask(m) => m.inner,
        }
    }
}

/// Every match of `pat` (a LIST joins on shared captures, each result one
/// sub-match per pattern) that survives `constraints`, keeping the first of the
/// rows sharing a [`crate::matcher::dedup_key`].
///
/// The coerced constraints live only for this query, so the predicate handles
/// they retain need no GC visibility.
fn query(
    slf: &Py<PyFunction>,
    py: Python<'_>,
    pat: crate::pattern::PatQuery<'_>,
    ignore_casts: &IgnoreCasts,
    constraints: Option<Vec<Bound<'_, PyAny>>>,
    ignore_root: bool,
) -> PyResult<Vec<crate::matcher::PyMatch>> {
    let constraints: Vec<_> = constraints
        .iter()
        .flatten()
        .map(|c| Ok(crate::pattern::coerce_tree(c, &mut Vec::new())?.materialize()))
        .collect::<PyResult<_>>()?;
    let mask = ignore_casts.mask();
    let patterns: Vec<_> = pat
        .to_patterns()?
        .into_iter()
        .map(|p| p.ignore_casts_mask(mask))
        .collect();
    let refs: Vec<&strider_pattern::Pattern> = patterns.iter().collect();
    let (raw, at) = run_query(slf, py, |matcher| {
        // Any constraint, even over a single pattern's own captures, routes
        // through the constrained join so the CFG filter actually runs.
        if refs.len() == 1 && constraints.is_empty() {
            Ok(matcher.matches(refs[0])?.map(|m| vec![m]).collect())
        } else {
            matcher.find_joined_constrained(&refs, &constraints)
        }
    })?;
    at.read(py, "Match", |function| {
        let mut seen = rustc_hash::FxHashSet::default();
        raw.into_iter()
            .filter(|inner| seen.insert(crate::matcher::dedup_key(inner, function, ignore_root)))
            .map(|inner| crate::matcher::PyMatch {
                inner,
                at: at.clone_ref(py),
            })
            .collect()
    })
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyFunction>()
}
