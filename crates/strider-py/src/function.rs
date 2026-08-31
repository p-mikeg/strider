use std::cell::{Ref, RefCell, RefMut};

use pyo3::prelude::*;
use strider_ir::IRWalker;
use strider_ir::node::NodeKind;

use crate::cfg::PyCfg;
use crate::dot::Pretty;

/// A lifted IR function: pattern queries, rewrites, walks and dot rendering.
#[pyclass(name = "Function", module = "strider.ir")]
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

    /// Pretty render for `to_dot(pretty=True)` / `to_html(pretty=True)`.
    fn pretty_dot(
        &self,
        py: Python<'_>,
        style: Option<&str>,
        path: Option<&str>,
        html: bool,
        with: Option<&Bound<'_, crate::strider_cls::PyLifter>>,
    ) -> PyResult<Option<String>> {
        use crate::strider_cls::{DotOp, DotResult};
        let cfg = self.cfg.bind(py).try_borrow()?;
        let borrowed;
        let lifter = match with {
            Some(l) => l.try_borrow()?,
            None => {
                borrowed = cfg.lifter.bind(py);
                borrowed.try_borrow()?
            }
        };
        let op = match (html, path) {
            (true, Some(p)) => DotOp::DumpHtml(p),
            (false, Some(p)) => DotOp::DumpDot(p),
            (true, None) => DotOp::HtmlStr,
            (false, None) => DotOp::DotStr,
        };
        match lifter.dispatch_dot(self, style, op)? {
            DotResult::Html(s) | DotResult::Dot(s) => Ok(Some(s)),
            DotResult::Unit => Ok(None),
        }
    }

    pub(crate) fn read_inner(&self) -> anyhow::Result<Ref<'_, strider_ir::Function>> {
        self.inner
            .try_borrow()
            .map_err(|_| anyhow::anyhow!("Function is currently borrowed for mutation"))
    }

    pub(crate) fn try_write_inner(&self) -> anyhow::Result<RefMut<'_, strider_ir::Function>> {
        self.inner.try_borrow_mut().map_err(|_| {
            anyhow::anyhow!(
                "Function mutation rejected: the function is currently borrowed for read \
                 (typically because this call is from inside a `.when()` predicate \
                 invoked by `find_all`/`find_unique`).  Mutating the function \
                 from within a pattern predicate is not supported: collect matches \
                 first and mutate after `find_all` returns."
            )
        })
    }

    fn with_read<R>(&self, f: impl FnOnce(&strider_ir::Function) -> PyResult<R>) -> PyResult<R> {
        let function = self.read_inner().map_err(crate::errors::into_strider_err)?;
        f(&function)
    }

    /// [`Self::with_read`] for infallible closures.
    fn with_read_value<R>(&self, f: impl FnOnce(&strider_ir::Function) -> R) -> PyResult<R> {
        let function = self.read_inner().map_err(crate::errors::into_strider_err)?;
        Ok(f(&function))
    }

    /// Run `pipeline` over this graph in place; `label` names the operation in
    /// the surfaced error.
    ///
    /// `rom` and `options` are what `analyze` builds its `OptCtx` from, so a
    /// hand-built pipeline folds read-only loads and honours the memory
    /// precision knobs the same way.
    pub(crate) fn run_pipeline_in_place(
        &self,
        pipeline: strider_orchestrator::opt::OptimizerPipeline,
        label: &str,
        rom: Option<&dyn strider_orchestrator::opt::ReadOnlyMemory>,
        options: strider_orchestrator::opt::OptOptions,
    ) -> PyResult<()> {
        let mut function = self
            .try_write_inner()
            .map_err(crate::errors::into_strider_err)?;
        // Bump BEFORE running: a pass that errors mid-run leaves the arena
        // partially rewritten, and a bump after the `?` never happens.
        function.graph_mut().bump_generation();
        let mut ctx = strider_orchestrator::opt::OptCtx::new(rom);
        ctx.options = options;
        pipeline.run(&mut function, &mut ctx).map_err(|e| {
            crate::errors::into_strider_err(anyhow::anyhow!("{label} failed: {e:?}"))
        })?;
        Ok(())
    }
}

fn write_to(path: &str, contents: String) -> PyResult<()> {
    std::fs::write(path, contents).map_err(|e| crate::errors::into_strider_err(anyhow::anyhow!(e)))
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

    /// Render the IR graph to Graphviz DOT. Returns the string when `path` is
    /// `None`, else writes it to `path` and returns `None`.
    ///
    /// `pretty=False` (the default) renders the graph exactly as stored: one
    /// node per node id reachable from entry, one edge per input edge,
    /// side-tables inline, no constant inlining or virtual nodes.
    ///
    /// `pretty=True` inlines constants, adds virtual nodes and resolves
    /// register names, which needs a `Sleigh`; by default the one behind the
    /// parent `Cfg`'s `Lifter`. A theme name in place of `True` picks the dot
    /// theme.
    ///
    /// `lifter=` renders through a different handle instead. Decoding is
    /// pinned to its creating thread, so a renderer on another thread passes
    /// its own handle here; the tables a render reads are identical for any
    /// handle on the same arch.
    #[pyo3(signature = (path=None, *, pretty=Pretty::Flag(false), lifter=None))]
    fn to_dot(
        &self,
        py: Python<'_>,
        path: Option<&str>,
        pretty: Pretty,
        lifter: Option<&Bound<'_, crate::strider_cls::PyLifter>>,
    ) -> PyResult<Option<String>> {
        if let Some(style) = pretty.theme() {
            return self.pretty_dot(py, Some(style), path, /* html */ false, lifter);
        }
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

    /// Like `to_dot` but wraps the DOT in a self-contained HTML page (embedded
    /// viz.js, no external `dot` binary). Same arguments and caveats.
    #[pyo3(signature = (path=None, *, pretty=Pretty::Flag(false)))]
    fn to_html(
        &self,
        py: Python<'_>,
        path: Option<&str>,
        pretty: Pretty,
    ) -> PyResult<Option<String>> {
        if let Some(style) = pretty.theme() {
            return self.pretty_dot(py, Some(style), path, /* html */ true, None);
        }
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

    /// Total number of node ids in the graph, whether or not they are
    /// reachable from entry.
    fn node_count(&self) -> PyResult<usize> {
        self.with_read_value(|function| function.graph().all_node_ids().count())
    }

    /// Node id of the function's `Entry` node.
    fn entry_node(&self) -> PyResult<u32> {
        self.with_read_value(|function| function.entry().as_u32())
    }

    fn __repr__(&self) -> PyResult<String> {
        self.with_read_value(|function| {
            format!(
                "Function(entry=#{}, {} nodes)",
                function.entry().as_u32(),
                function.graph().all_node_ids().count(),
            )
        })
    }

    /// Render the depth-`depth` neighborhood around node `center`.
    ///
    /// `pretty=False` (the default) draws the nodes exactly as stored;
    /// `pretty=True` inlines constants, adds virtual nodes and resolves
    /// register names, so it needs the `Sleigh` behind `cfg`.
    #[pyo3(signature = (center, depth=5, hub_cap=12, max_nodes=60, count_producers=false, *, pretty=false, lifter=None))]
    #[allow(clippy::too_many_arguments)]
    fn neighborhood_dot(
        &self,
        py: Python<'_>,
        center: u32,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
        count_producers: bool,
        pretty: bool,
        lifter: Option<&Bound<'_, crate::strider_cls::PyLifter>>,
    ) -> PyResult<String> {
        if pretty {
            let cfg = self.cfg.bind(py).try_borrow()?;
            let borrowed;
            let lifter = match lifter {
                Some(l) => l.try_borrow()?,
                None => {
                    borrowed = cfg.lifter.bind(py);
                    borrowed.try_borrow()?
                }
            };
            return lifter.dispatch_neighborhood_dot(
                self,
                center,
                depth,
                hub_cap,
                max_nodes,
                count_producers,
            );
        }
        self.with_read_value(|function| {
            let nid = function
                .graph()
                .node_id_from_u32(center)
                .ok_or_else(|| anyhow::anyhow!("invalid node id {center}"))?;
            function.raw_neighborhood_dot(nid, depth, hub_cap, max_nodes, count_producers)
        })?
        .map_err(crate::errors::into_strider_err)
    }

    /// Number of `Region` (control-flow join) nodes reachable from entry.
    fn count_regions(&self) -> PyResult<usize> {
        self.with_read_value(|function| {
            function
                .walk_kind(|k| matches!(k, NodeKind::Region))
                .count()
        })
    }

    /// Every node id in the graph, reachable or not, as raw integers.
    fn node_ids(&self) -> PyResult<Vec<u32>> {
        self.with_read_value(|function| {
            function
                .graph()
                .all_node_ids()
                .map(|n| n.as_u32())
                .collect()
        })
    }

    /// Re-validate the graph: `None` on success, else an error message.
    fn validate(&self) -> PyResult<Option<String>> {
        self.with_read(|function| match strider_ir::validate::validate(function) {
            Ok(()) => Ok(None),
            Err(e) => Ok(Some(format!("{e}"))),
        })
    }

    /// Drop every node unreachable from `entry`. Mutates in place, and
    /// invalidates all pre-compaction node ids.
    fn compact(&self) -> PyResult<()> {
        let mut function = self
            .try_write_inner()
            .map_err(crate::errors::into_strider_err)?;
        let _remap = function
            .compact()
            .map_err(crate::errors::into_strider_err)?;
        Ok(())
    }

    /// Deep-copy into an independent `Function`, leaving this one untouched.
    /// The parent `Cfg` is shared.
    #[pyo3(name = "clone")]
    fn py_clone(&self, py: Python<'_>) -> PyResult<PyFunction> {
        let cloned = self
            .read_inner()
            .map_err(crate::errors::into_strider_err)?
            .clone();
        Ok(PyFunction {
            inner: RefCell::new(cloned),
            cfg: self.cfg.clone_ref(py),
        })
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
        let patterns = build_query_patterns(py, pat, &ignore_casts)?;
        let constraints = collect_constraints(&constraints)?;
        let (raw, generation) = run_pattern_query(&slf, py, &patterns, &constraints)?;
        dedup_matches(&slf, py, raw, generation, ignore_root)
    }

    /// Find the single binding of `pat`, erroring if there is not exactly one.
    /// Arguments mirror `find_all`. The count is taken after deduplication, so
    /// `ignore_root` decides whether distinct roots binding the same captures
    /// count as one match or many.
    #[pyo3(signature = (pat, ignore_root=false, ignore_casts=IgnoreCasts::Flag(false), constraints=None))]
    fn find_unique(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatQuery<'_>,
        ignore_root: bool,
        ignore_casts: IgnoreCasts,
        constraints: Option<Vec<Bound<'_, PyAny>>>,
    ) -> PyResult<crate::matcher::PyMatch> {
        let patterns = build_query_patterns(py, pat, &ignore_casts)?;
        let constraints = collect_constraints(&constraints)?;
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
        let patterns = build_query_patterns(py, pat, &ignore_casts)?;
        let constraints = collect_constraints(&constraints)?;
        let (raw, generation) = run_pattern_query(&slf, py, &patterns, &constraints)?;
        let matches = dedup_matches(&slf, py, raw, generation, true)?;
        if signed {
            collect_unique(py, &matches, cap, crate::matcher::PyMatch::sint_for)
        } else {
            collect_unique(py, &matches, cap, crate::matcher::PyMatch::uint_for)
        }
    }

    /// Apply one find/replace rewrite rule across the graph, returning the
    /// number of times it fired. `find` takes any pattern-like value;
    /// `replace` is a `strider.template.Template` built from the
    /// `strider.template` free functions (a build-valid `strider.pattern.Pat`,
    /// a `Capture` or a capture-name string are accepted for back-compat).
    ///
    /// The RHS is validated up front: every node must be a concrete builder or
    /// a capture bound by the LHS. A `.when()` predicate on the LHS is
    /// rejected: the function is held for mutation while the rule fires.
    ///
    /// Every outstanding `Node` handle goes stale, a return of 0 included: the
    /// graph generation moves before the rule runs, since a rule erroring
    /// part-way leaves the graph rewritten.
    fn rewrite(
        &self,
        py: Python<'_>,
        find: crate::pattern::PatLike<'_>,
        replace: crate::pattern::TemplateLike<'_>,
    ) -> PyResult<usize> {
        let lhs = crate::pattern::compile_rewrite_lhs(py, &find)?;
        let rhs = replace.to_template(py)?;
        let rule =
            strider_opt::rewrite_rule_runtime(lhs, rhs).map_err(crate::errors::into_strider_err)?;
        let mut function = self
            .try_write_inner()
            .map_err(crate::errors::into_strider_err)?;
        apply_rules_count_on(&mut function, std::slice::from_ref(&rule))
    }

    /// Apply `(find, replace)` pairs round-robin at every reachable node,
    /// returning the total fire count across pairs and nodes. See `rewrite`.
    fn rewrite_all(
        &self,
        py: Python<'_>,
        pairs: Vec<(
            crate::pattern::PatLike<'_>,
            crate::pattern::TemplateLike<'_>,
        )>,
    ) -> PyResult<usize> {
        let mut rules: Vec<strider_opt::BoxedRule> = Vec::with_capacity(pairs.len());
        for (lhs, rhs) in pairs {
            let lhs_pat = crate::pattern::compile_rewrite_lhs(py, &lhs)?;
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

    /// A `Node` handle on the node at `node_id`. Raises `StriderError` for an
    /// invalid `node_id`.
    ///
    /// A raw id is meaningful only in the graph generation it came from:
    /// `compact` and `optimize` renumber, and an id held across either names
    /// a different node. A `Node` carries its generation and goes stale
    /// instead; a bare int cannot.
    fn node(slf: Py<Self>, py: Python<'_>, node_id: u32) -> PyResult<crate::node::PyNode> {
        crate::node::PyNode::new(py, slf, node_id)
    }

    /// Control-only reachability (the CFG skeleton) from the entry, in
    /// ascending node-id order.
    fn cfg_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        let ids: Vec<u32> = slf.borrow(py).with_read_value(|function| {
            strider_ir::walk::cfg_reachable(function.graph(), function.entry())
                .iter()
                .map(|n| n.as_u32())
                .collect()
        })?;
        Self::nodes_from_ids(slf, py, ids)
    }

    /// Every node reachable from the entry (data-in + control-out), pre-order.
    fn data_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        let ids: Vec<u32> = slf
            .borrow(py)
            .with_read_value(|function| function.walk().map(|n| n.as_u32()).collect())?;
        Self::nodes_from_ids(slf, py, ids)
    }

    /// Memory-touching nodes (Load / Store / Call / CallOther / MemPhi plus
    /// the InitialMemory root) reached by following memory-token edges forward
    /// from InitialMemory.  Each node appears once, that root first; the order at
    /// a `MemPhi` join is unspecified, so a node can precede one of its own
    /// memory predecessors.
    fn mem_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        let ids: Vec<u32> = slf.borrow(py).with_read_value(|function| {
            strider_ir::walk::memory_reachable(function, function.entry())
                .into_iter()
                .map(|n| n.as_u32())
                .collect()
        })?;
        Self::nodes_from_ids(slf, py, ids)
    }

    /// Every node reachable from `node_id` (data-in + control-out), pre-order.
    fn walk(slf: Py<Self>, py: Python<'_>, node_id: u32) -> PyResult<Vec<crate::node::PyNode>> {
        let ids: Vec<u32> = slf.borrow(py).with_read(|function| {
            let nid = function.graph().node_id_from_u32(node_id).ok_or_else(|| {
                crate::errors::into_strider_err(anyhow::anyhow!("no node with id {node_id}"))
            })?;
            Ok(function.walk_from(nid).map(|n| n.as_u32()).collect())
        })?;
        Self::nodes_from_ids(slf, py, ids)
    }
}

impl PyFunction {
    /// Callers must have dropped the read borrow they collected `ids` under.
    fn nodes_from_ids(
        slf: Py<Self>,
        py: Python<'_>,
        ids: Vec<u32>,
    ) -> PyResult<Vec<crate::node::PyNode>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(crate::node::PyNode::new(py, slf.clone_ref(py), id)?);
        }
        Ok(out)
    }
}

/// The distinct constant values `extract` reads for `cap` across `matches`,
/// reduced to the sole value.
fn collect_unique<T: IntoPy<PyObject> + Ord>(
    py: Python<'_>,
    matches: &[crate::matcher::PyMatch],
    cap: strider_pattern::Capture,
    extract: impl Fn(
        &crate::matcher::PyMatch,
        Python<'_>,
        strider_pattern::Capture,
    ) -> PyResult<Option<T>>,
) -> PyResult<Option<PyObject>> {
    let mut values = std::collections::BTreeSet::new();
    for m in matches {
        if let Some(v) = extract(m, py, cap)? {
            values.insert(v);
        }
    }
    unique_value(py, values)
}

fn unique_value<T: IntoPy<PyObject> + Ord>(
    py: Python<'_>,
    values: std::collections::BTreeSet<T>,
) -> PyResult<Option<PyObject>> {
    match values.len() {
        0 => Ok(None),
        1 => Ok(Some(values.into_iter().next().unwrap().into_py(py))),
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

/// Run a matcher query, returning its result and the graph generation it ran
/// against.
///
/// Pushes `slf` + the generation onto `crate::pattern::CURRENT_QUERY_FUNCTION`
/// for the duration of `run`, so a `.when()` predicate can build a `Match`
/// handle back onto this live function.
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
    drop(function_guard);
    drop(function_borrow);
    // Surface anything a `.when()` predicate stashed: a control-flow exception
    // (KeyboardInterrupt / SystemExit) or a bad-return-type PyErr. A
    // thread-local cell, not `PyErr::restore`/`take`, because restore leaves
    // the error set between predicate calls and the next `call_bound` would
    // replace the original with `SystemError`. Drained on the error path too,
    // so a later, unrelated query cannot inherit it.
    let raw = crate::strider_cls::with_pending_control_flow(|| {
        raw.map_err(crate::errors::into_strider_err)
    })?;
    Ok((raw, generation))
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

fn build_query_patterns(
    py: Python<'_>,
    pat: crate::pattern::PatQuery<'_>,
    ignore_casts: &IgnoreCasts,
) -> PyResult<Vec<strider_pattern::Pattern>> {
    let mask = ignore_casts.mask();
    Ok(pat
        .to_patterns(py)?
        .into_iter()
        .map(|p| p.ignore_casts_mask(mask))
        .collect())
}

/// The coerced constraints live only for this query, so the predicate handles
/// they retain need no GC visibility.
fn collect_constraints(
    constraints: &Option<Vec<Bound<'_, PyAny>>>,
) -> PyResult<Vec<strider_pattern::JoinConstraint>> {
    let mut held = Vec::new();
    constraints.as_deref().map_or_else(
        || Ok(Vec::new()),
        |v| {
            v.iter()
                .map(|c| crate::pattern::coerce_join_constraint(c, &mut held))
                .collect()
        },
    )
}

/// One sub-match group per result: a single pattern gives one-element groups,
/// several patterns join on shared captures and give one sub-match per pattern.
fn run_pattern_query(
    slf: &Py<PyFunction>,
    py: Python<'_>,
    patterns: &[strider_pattern::Pattern],
    constraints: &[strider_pattern::JoinConstraint],
) -> PyResult<(Vec<Vec<strider_pattern::Match>>, u64)> {
    let refs: Vec<&strider_pattern::Pattern> = patterns.iter().collect();
    run_query(slf, py, |matcher| {
        // Any constraint, even over a single pattern's own captures, routes
        // through the constrained join so the CFG filter actually runs.
        if refs.len() == 1 && constraints.is_empty() {
            Ok(matcher.matches(refs[0])?.map(|m| vec![m]).collect())
        } else {
            matcher.find_joined_constrained(&refs, constraints)
        }
    })
}

/// Dedup key is `(roots, capture-signatures)`, or capture-signatures alone
/// under `ignore_root`.
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

/// Returns the total per-`(node, rule)` fire count.
fn apply_rules_count_on<R>(function: &mut strider_ir::Function, rules: &[R]) -> PyResult<usize>
where
    R: for<'g> Fn(
        &mut strider_opt::EditFunction<'g>,
        strider_ir::node::NodeId,
    ) -> anyhow::Result<Option<strider_ir::node::ValueId>>,
{
    // Bump BEFORE running, as `run_pipeline_in_place` does: a rule that errors
    // part-way leaves the arena partially rewritten.
    function.graph_mut().bump_generation();
    let count = {
        let mut ctx = strider_opt::EditFunction::new(function);
        strider_opt::apply_rules_count(&mut ctx, rules).map_err(crate::errors::into_strider_err)?
    };
    Ok(count)
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyFunction>()
}
