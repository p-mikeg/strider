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
    pub(crate) cfg: Py<PyCfg>,
}

impl PyFunction {
    pub(crate) fn new(function: strider_ir::Function, cfg: Py<PyCfg>) -> Self {
        Self {
            inner: RefCell::new(function),
            cfg,
        }
    }

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

    /// Run `f` on the function borrowed for mutation. The generation moves
    /// first: a mutation erroring part-way still rewrote the graph.
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
    // The cycle is broken at the reader's `__dict__` / `PyLifter::__clear__`.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.cfg)
    }

    /// The `Cfg` this function was lifted from.
    #[getter(cfg)]
    fn get_cfg(&self, py: Python<'_>) -> Py<PyCfg> {
        self.cfg.clone_ref(py)
    }

    /// A copy of the claims in force: those `analyze` ran under plus every set
    /// `optimize` added since.
    #[getter]
    fn assumptions(&self) -> PyResult<crate::options::PyAssumptionOptions> {
        Ok(crate::options::PyAssumptionOptions {
            inner: self.read()?.assumptions().clone(),
        })
    }

    /// Run `pipeline` (default: the default pipeline) over this function in
    /// place. `assumptions` must include every claim in force and may not add
    /// the lift-time claims `msr_register_keeps_mode` /
    /// `fp_rounding_mode_is_nearest`, else `StriderError` before running. Add
    /// a claim with::
    ///
    ///     function.optimize(assumptions=function.assumptions
    ///                       | AssumptionOptions.none(escape_analysis=True))
    ///
    /// Reads the producing `Lifter`'s rom; raises `StriderError` while that
    /// `Lifter` is in `analyze`, `build_cfg` or `add_elf`. Stales `Node` /
    /// `Match` handles. Holds the GIL.
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

    /// The graph as DOT, or written to `path` (returning `None`).
    /// `pretty=False` renders it as stored; `pretty=True` inlines constants
    /// and names registers; a `DotStyle` name picks the pretty theme.
    #[pyo3(signature = (path=None, *, pretty=Pretty::Flag(false)))]
    fn to_dot(&self, path: Option<&str>, pretty: Pretty) -> PyResult<Option<String>> {
        self.render(path, &pretty, /* html */ false)
    }

    /// `to_dot` wrapped in a self-contained HTML page.
    #[pyo3(signature = (path=None, *, pretty=Pretty::Flag(false)))]
    fn to_html(&self, path: Option<&str>, pretty: Pretty) -> PyResult<Option<String>> {
        self.render(path, &pretty, /* html */ true)
    }

    /// The canonical text form: a header, then one line per node reachable
    /// from entry. Equal graphs print equal text.
    #[pyo3(signature = (fingerprints = false))]
    fn to_text(&self, fingerprints: bool) -> PyResult<String> {
        Ok(self.read()?.to_text(fingerprints))
    }

    /// The number of node ids, reachable from entry or not.
    fn node_count(&self) -> PyResult<usize> {
        Ok(self.read()?.graph().all_node_ids().count())
    }

    /// The id of the `Entry` node.
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

    /// DOT for the nodes within `depth` hops of `center`, at most
    /// `max_nodes`; a hub past `hub_cap` consumers is not followed.
    /// `pretty` as in `to_dot`.
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

    /// The number of `Region` nodes reachable from entry.
    fn count_regions(&self) -> PyResult<usize> {
        let function = self.read()?;
        Ok(function
            .walk_kind(|k| matches!(k, NodeKind::Region))
            .count())
    }

    /// Every node id, reachable or not.
    fn node_ids(&self) -> PyResult<Vec<u32>> {
        Ok(self
            .read()?
            .graph()
            .all_node_ids()
            .map(|n| n.as_u32())
            .collect())
    }

    /// Check the graph's invariants: `None` on success, else the error.
    fn validate(&self) -> PyResult<Option<String>> {
        let function = self.read()?;
        Ok(strider_ir::validate::validate(&function)
            .err()
            .map(|e| e.report(&function).to_string()))
    }

    /// Drop every node unreachable from entry. Invalidates node ids.
    fn compact(&self) -> PyResult<()> {
        self.edit(|f| {
            f.compact();
            Ok(())
        })
    }

    /// An independent copy, sharing the parent `Cfg`.
    #[pyo3(name = "clone")]
    fn py_clone(&self, py: Python<'_>) -> PyResult<PyFunction> {
        let cloned = self.read()?.clone();
        Ok(PyFunction::new(cloned, self.cfg.clone_ref(py)))
    }

    /// Every deduplicated `Match` of `pat`. A list of patterns joins on
    /// shared captures, and `constraints` filter the joined results.
    /// `ignore_casts` (`True`, or a `CastMask`) looks through extend /
    /// truncate / bit-reinterpret casts.
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

    /// The single `Match` of `pat` after dedup, else `StriderError`.
    /// Arguments as in `find_all`; with `ignore_root=True` a pattern binding
    /// no captures succeeds on any number of occurrences.
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

    /// The one constant `capture` binds across the matches of `pat`: `None`
    /// when none binds one, `StriderError` for two distinct values.
    /// `signed=True` reads it as two's-complement.
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

    /// Apply one `find` -> `replace` rewrite in one walk, returning how often
    /// it fired. `replace` is a `Template`, a `Capture` or a capture name.
    /// Raises `StriderError` up front if `replace` names a capture `find`
    /// does not bind, or `find` has a `.when()` guard.
    ///
    /// A match is skipped when the replacement has another width, or, for a
    /// `find` through a phi or region, would read the matched value or is not
    /// available everywhere that value is read. Stales every `Node` handle,
    /// even on a return of 0.
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

    /// A `Node` handle on `node_id`, else `StriderError`. A raw id is
    /// meaningful only until `compact`, `optimize` or `rewrite`; a `Node` then
    /// goes stale, while an int silently names something else.
    fn node(
        slf: Py<Self>,
        py: Python<'_>,
        node_id: crate::bigint::Unsigned<u32>,
    ) -> PyResult<crate::node::PyNode> {
        let node_id = node_id.0;
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

    /// The nodes reachable from entry along control edges, by ascending id.
    fn cfg_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        Self::nodes(slf, py, |function| {
            Ok(
                strider_ir::walk::cfg_reachable(function.graph(), function.entry())
                    .iter()
                    .collect(),
            )
        })
    }

    /// Every node reachable from entry (data inputs, control outputs), in
    /// pre-order.
    fn data_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        Self::nodes(slf, py, |function| Ok(function.walk().collect()))
    }

    /// The memory nodes reached along memory edges from `InitialMemory`
    /// (included), in pre-order, each once. At a `MemPhi` a node can precede
    /// one of its memory predecessors.
    fn mem_walk(slf: Py<Self>, py: Python<'_>) -> PyResult<Vec<crate::node::PyNode>> {
        Self::nodes(slf, py, |function| {
            Ok(strider_ir::walk::memory_reachable(function))
        })
    }

    /// Every node reachable from `node_id` (data inputs, control outputs), in
    /// pre-order.
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
/// against.
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

/// The distinct constant `read` finds for `cap` across `matches`.
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
/// generation it ran against, which is pushed as the current query function
/// during `run` so a `.when()` predicate can build a `Match` on it.
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
    // A `.when()` error is stashed thread-locally (a restored PyErr would
    // become `SystemError` at the next call); drained on the error path too.
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

/// Every match of `pat` that survives `constraints`, keeping the first row
/// per [`crate::matcher::dedup_key`]. The coerced constraints live only for
/// this query, so need no GC visibility.
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
        // Constraints apply only in the join.
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
