//! `PyGraph` — wraps `strider_ir::Graph` and exposes dot rendering
//! plus pattern queries and rewrites.
//!
//! The IR graph's dot dumper requires a borrowed `Sleigh` for
//! register-name resolution.  PyGraph keeps a `Py<PyCfg>` reference
//! so the Sleigh stays alive for the graph's lifetime and is
//! reachable through `strider_lift::cfg::Cfg::sleigh`.

use std::path::Path;
use std::sync::{Arc, RwLock};

use pyo3::prelude::*;

use crate::cfg::PyCfg;
use crate::dot::dot_style_for;

/// Opaque wrapper over `strider_ir::Graph`.
///
/// The graph is held in `Arc<RwLock<...>>` so optimization passes can
/// mutate it without requiring `&mut self` on the PyGraph wrapper,
/// and so the same graph can be shared across multiple Python
/// references.
#[pyclass(name = "Graph", module = "strider")]
pub struct PyGraph {
    pub(crate) inner: Arc<RwLock<strider_ir::Graph>>,
    /// Strong reference to the parent Cfg; keeps the Sleigh alive for
    /// dot rendering and ensures destruction order is graph-then-cfg.
    pub(crate) cfg: Py<PyCfg>,
}

/// Discriminator for [`PyGraph::dispatch_dot`].  Each variant carries
/// the per-op arguments the public accessor `to_html` / `to_dot` /
/// `html_str` would otherwise duplicate the cfg-borrow / graph-borrow
/// / dumper-construction ritual for.
enum DotOp<'a> {
    DumpHtml(&'a str),
    DumpDot(&'a str),
    HtmlStr,
}

/// Return shape of [`PyGraph::dispatch_dot`].  Returning a sum lets a
/// single helper cover both unit-returning dump methods and the
/// string-returning `html_str` without separate variants per
/// dispatch.
enum DotResult {
    Unit,
    Html(String),
}

/// Convert a Python-supplied `u32` node id into a validated `strider_ir::NodeId`,
/// returning `StriderError` on lookup failure.
fn node_id_from_u32(graph: &strider_ir::Graph, node_id: u32) -> PyResult<strider_ir::node::NodeId> {
    let nid = graph
        .all_node_ids()
        .find(|n| n.as_u32() == node_id)
        .ok_or_else(|| {
            crate::errors::into_strider_err(anyhow::anyhow!(
                "no node with id {node_id} in graph"
            ))
        })?;
    Ok(nid)
}

impl PyGraph {
    pub(crate) fn new(graph: strider_ir::Graph, cfg: Py<PyCfg>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(graph)),
            cfg,
        }
    }

    /// Borrow the inner graph for read.  Returns an `anyhow::Error`
    /// when the lock is poisoned.
    pub(crate) fn read_inner(&self) -> anyhow::Result<std::sync::RwLockReadGuard<'_, strider_ir::Graph>> {
        self.inner
            .read()
            .map_err(|_| anyhow::anyhow!("Graph lock poisoned"))
    }

    /// Borrow the inner graph for write.  Returns an `anyhow::Error`
    /// when the lock is poisoned.
    pub(crate) fn write_inner(&self) -> anyhow::Result<std::sync::RwLockWriteGuard<'_, strider_ir::Graph>> {
        self.inner
            .write()
            .map_err(|_| anyhow::anyhow!("Graph lock poisoned"))
    }

    /// Try to acquire the write lock without blocking.  Used by mutating
    /// methods (`optimize`, `compact`, `rewrite`, `reoptimize`) so that a
    /// re-entrant call from inside a `.when()` predicate (which holds the
    /// read lock for the duration of `find_all`) surfaces a typed error
    /// rather than deadlocking the thread.
    pub(crate) fn try_write_inner(&self) -> anyhow::Result<std::sync::RwLockWriteGuard<'_, strider_ir::Graph>> {
        use std::sync::TryLockError;
        self.inner.try_write().map_err(|e| match e {
            TryLockError::Poisoned(_) => anyhow::anyhow!("Graph lock poisoned"),
            TryLockError::WouldBlock => anyhow::anyhow!(
                "Graph mutation rejected: the graph is currently borrowed for read \
                 (typically because this call is from inside a `.when()` predicate \
                 invoked by `find_all`/`find_all_requirements`).  Mutating the graph \
                 from within a pattern predicate is not supported — collect matches \
                 first and mutate after `find_all` returns."
            ),
        })
    }

    /// Borrow the inner graph for read, then run `f` against it.  Centralises
    /// the `self.read_inner().map_err(into_strider_err)?` incantation that
    /// every read-only `#[pymethods]` accessor would otherwise repeat.  Use
    /// this variant when `f` itself returns a `PyResult` (e.g. it propagates
    /// `?` from `node_id_from_u32` or builds an error from graph state).
    fn with_read<R>(
        &self,
        f: impl FnOnce(&strider_ir::Graph) -> PyResult<R>,
    ) -> PyResult<R> {
        let g = self.read_inner().map_err(crate::errors::into_strider_err)?;
        f(&g)
    }

    /// Like [`Self::with_read`] but for accessors whose closure just
    /// produces a value with no further fallible step — saves the
    /// per-site `Ok(...)` wrapping.
    fn with_read_value<R>(
        &self,
        f: impl FnOnce(&strider_ir::Graph) -> R,
    ) -> PyResult<R> {
        let g = self.read_inner().map_err(crate::errors::into_strider_err)?;
        Ok(f(&g))
    }

    /// Enum tagging the three dot-rendering operations the public
    /// surface needs.  Lets [`Self::dispatch_dot`] funnel them
    /// through a single helper that builds the `GraphDot` once and
    /// dispatches to the right `dot::GraphDot` method, instead of
    /// repeating the borrow / dumper / GraphDot construction at every
    /// caller (the dumper type is `pub(crate)` in `strider-ir` and
    /// can't be named from this crate's closures).
    fn dispatch_dot(
        &self,
        py: Python<'_>,
        style: Option<&str>,
        op: DotOp<'_>,
    ) -> PyResult<DotResult> {
        let cfg_borrow = self.cfg.borrow(py);
        self.with_read(|graph| {
            let dumper = graph
                .dot_dumper(cfg_borrow.inner.sleigh())
                .map_err(crate::errors::into_strider_err)?;
            let d = dot::GraphDot::new(dumper, dot_style_for(style));
            match op {
                DotOp::DumpHtml(p) => d
                    .dump_as_html(Path::new(p))
                    .map(|()| DotResult::Unit)
                    .map_err(crate::errors::into_strider_err),
                DotOp::DumpDot(p) => d
                    .dump_as_dot(Path::new(p))
                    .map(|()| DotResult::Unit)
                    .map_err(crate::errors::into_strider_err),
                DotOp::HtmlStr => d
                    .as_html_from_dot()
                    .map(DotResult::Html)
                    .map_err(crate::errors::into_strider_err),
            }
        })
    }
}

#[pymethods]
impl PyGraph {
    #[pyo3(signature = (path, style=None))]
    fn to_html(&self, py: Python<'_>, path: &str, style: Option<&str>) -> PyResult<()> {
        self.dispatch_dot(py, Some(style.unwrap_or("dark")), DotOp::DumpHtml(path))
            .map(|_| ())
    }

    #[pyo3(signature = (path,))]
    fn to_dot(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        self.dispatch_dot(py, Some("dark"), DotOp::DumpDot(path))
            .map(|_| ())
    }

    #[pyo3(signature = (style=None))]
    fn html_str(&self, py: Python<'_>, style: Option<&str>) -> PyResult<String> {
        match self.dispatch_dot(py, Some(style.unwrap_or("dark")), DotOp::HtmlStr)? {
            DotResult::Html(s) => Ok(s),
            DotResult::Unit => Err(crate::errors::into_strider_err(anyhow::anyhow!(
                "internal: DotOp::HtmlStr returned DotResult::Unit"
            ))),
        }
    }

    fn node_count(&self) -> PyResult<usize> {
        self.with_read_value(|graph| graph.all_node_ids().count())
    }

    /// Returns the count of `Region` join nodes reachable from
    /// entry.  Despite its name and historical docstring, this method
    /// is **not** a true loop-header detector: the previous
    /// implementation ran a per-predecessor forward-CFG DFS looking for
    /// a back-edge, but because the predecessor's direct Control edge
    /// into the join node is itself "a Control output whose consumer is
    /// the join node", the inner DFS returned `true` on its very first
    /// iteration for every reachable `Region`.  The observable
    /// behaviour is therefore equivalent to "count reachable
    /// `Region` nodes", which is what the existing test suite
    /// (`count_loops(g) >= 1` on `early_return`, `clamp`, etc.) depends
    /// on — those fixtures have no actual back-edge after `-O2`
    /// loop-rotation, yet the assertion holds because the count is
    /// driven by join arity, not loop topology.
    ///
    /// Preserve that contract while collapsing the
    /// O(|Region| x |graph|) cost — and the per-call
    /// `HashSet<NodeId>` allocation — into a single linear pre-order
    /// sweep using the IR's own kind-filtered walker.  The walker's
    /// visited-set is already a `DenseEntitySet<NodeId>` (see
    /// [`strider_ir::walk::PreOrder`]), so this satisfies the
    /// "use entity-set bookkeeping" memory directive by routing
    /// through the canonical IR traversal helper.
    fn count_loop_headers(&self) -> PyResult<usize> {
        use strider_ir::node::NodeKind;
        self.with_read_value(|graph| {
            graph
                .preorder_kind(|k| matches!(k, NodeKind::Region))
                .count()
        })
    }

    /// Returns a list of all reachable node ids in the graph as raw
    /// integers.  Useful for iterating from Python without going
    /// through pattern matching.
    fn node_ids(&self) -> PyResult<Vec<u32>> {
        self.with_read_value(|graph| graph.all_node_ids().map(|n| n.as_u32()).collect())
    }

    /// Returns the [`NodeKind`] of the node at `node_id`, formatted as
    /// a string (e.g. "IntConst", "Call", "Phi", "Add", …).  Useful
    /// for direct graph introspection from Python tests / debug
    /// scripts.
    ///
    /// Raises `StriderError` for an invalid `node_id`.
    fn node_kind(&self, node_id: u32) -> PyResult<String> {
        self.with_read(|graph| {
            let nid = node_id_from_u32(graph, node_id)?;
            Ok(format!("{:?}", graph.node_kind(nid)))
        })
    }

    /// Returns the asm-fingerprint addresses recorded on the node at
    /// `node_id` — a sorted, deduped list of machine-instruction
    /// addresses whose lift contributed to the node's value.
    ///
    /// Empty for "structural" node kinds (Entry, InitialMemory, phis,
    /// Region, FunctionArg) whose existence is synthesised by
    /// the IR builder rather than tied to a specific asm instruction.
    fn asm_fingerprint(&self, node_id: u32) -> PyResult<Vec<u64>> {
        self.with_read(|graph| {
            let nid = node_id_from_u32(graph, node_id)?;
            Ok(graph.asm_fingerprint(nid).to_vec())
        })
    }

    /// Returns the raw little-endian bytes of an `IntConstWide` node's
    /// value (32 bytes for U256, 64 for U512), or `None` for narrow
    /// `IntConst` and any non-const node kind.
    ///
    /// Use this for AVX-2 / AVX-512 register constants whose value
    /// doesn't fit in `u128`; narrow constants (≤ U128) are accessible
    /// via `Match.get_uint(c)` instead.
    fn wide_const_bytes(&self, node_id: u32) -> PyResult<Option<Vec<u8>>> {
        self.with_read(|graph| {
            let nid = node_id_from_u32(graph, node_id)?;
            match graph.node_kind(nid) {
                strider_ir::node::NodeKind::IntConstWide(id) => {
                    Ok(Some(graph.wide_const(*id).to_le_bytes()))
                }
                _ => Ok(None),
            }
        })
    }

    /// Returns the Sleigh user-op name attached to a `CallOther` node,
    /// or `None` for any other node kind.
    fn call_other_name(&self, node_id: u32) -> PyResult<Option<String>> {
        self.with_read(|graph| {
            let nid = node_id_from_u32(graph, node_id)?;
            Ok(graph.call_other_name(nid).map(str::to_owned))
        })
    }

    /// Re-validates the graph and returns `None` on success or a
    /// human-readable error message on failure.
    ///
    /// The asm-fingerprint Layer-C check is always-on: every reachable
    /// non-exempt node must carry a non-empty contributor list.
    fn validate(&self) -> PyResult<Option<String>> {
        self.with_read(|graph| {
            let entry = graph.entry().ok_or_else(|| {
                crate::errors::into_strider_err(anyhow::anyhow!(
                    "Graph.validate: graph has not been built (entry is None)"
                ))
            })?;
            match strider_ir::validate::validate(graph.graph(), entry) {
                Ok(()) => Ok(None),
                Err(e) => Ok(Some(format!("{e}"))),
            }
        })
    }

    /// Compact the graph arena: drop every node not reachable from
    /// `entry` via [`strider_ir::graph::Graph::walk_from`].  Mutates in place.
    /// Pre-compaction node ids become invalid across this call.
    fn compact(&self) -> PyResult<()> {
        let mut graph = self.try_write_inner().map_err(crate::errors::into_strider_err)?;
        let _remap = graph.compact().map_err(crate::errors::into_strider_err)?;
        Ok(())
    }

    /// Apply a `PyOptimizerPipeline` to this graph in place.  Drains
    /// the pipeline (subsequent calls to the same pipeline see an
    /// empty pass list); rebuild it from `OptimizerPipeline.default()`
    /// or the equivalent classmethods if you need to apply it again.
    fn optimize(&self, pipeline: &crate::opt::PyOptimizerPipeline) -> PyResult<()> {
        let real_pipeline = pipeline.drain_into_pipeline()?;
        let mut graph = self.try_write_inner().map_err(crate::errors::into_strider_err)?;
        let entry = graph.entry().ok_or_else(|| {
            crate::errors::into_strider_err(anyhow::anyhow!(
                "Graph.optimize: graph has not been built (entry is None)"
            ))
        })?;
        real_pipeline
            .run(graph.graph_mut(), entry)
            .map_err(|e| crate::errors::into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))
    }

    /// Convenience: re-run the stable pipeline (and optionally the
    /// destructive pipeline) on this graph.  Useful after a manual
    /// rewrite (`graph.rewrite(...)`) to re-converge the graph.
    #[pyo3(signature = (destructive=false))]
    fn reoptimize(&self, destructive: bool) -> PyResult<()> {
        let mut pipe = strider_analyze::opt::stable_default_pipeline();
        if destructive {
            // Append the destructive passes after the stable ones.
            pipe.add(strider_analyze::opt::RedundantPhis);
            pipe.add(strider_analyze::opt::DeadBranchElimination);
        }
        let mut graph = self.try_write_inner().map_err(crate::errors::into_strider_err)?;
        let entry = graph.entry().ok_or_else(|| {
            crate::errors::into_strider_err(anyhow::anyhow!(
                "Graph.reoptimize: graph has not been built (entry is None)"
            ))
        })?;
        pipe.run(graph.graph_mut(), entry).map_err(|e| {
            crate::errors::into_strider_err(anyhow::anyhow!("reoptimize failed: {e:?}"))
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
    /// * `ignore_regions=True` — walk through `Region`
    ///   region-join nodes between an `If`'s output and the
    ///   matched consumer.
    #[pyo3(signature = (pat, ignore_casts=false, ignore_regions=false, ignore_casts_mask=None))]
    fn find_all(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatLike<'_>,
        ignore_casts: bool,
        ignore_regions: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
    ) -> PyResult<Vec<crate::matcher::PyMatch>> {
        if ignore_casts && ignore_casts_mask.is_some() {
            return Err(crate::errors::into_pattern_err(anyhow::anyhow!(
                "find_all: pass either ignore_casts=True or ignore_casts_mask=...; not both"
            )));
        }
        let pat = pat.into_pat()?;
        let g_borrow = slf.borrow(py);
        let graph_guard = g_borrow.read_inner().map_err(crate::errors::into_strider_err)?;
        let mut matcher = strider_analyze::pattern::Matcher::try_new(&graph_guard)
            .map_err(crate::errors::into_strider_err)?;
        if ignore_casts {
            matcher = matcher.ignore_casts();
        } else if let Some(m) = ignore_casts_mask {
            matcher = matcher.ignore_casts_mask(m.inner);
        }
        if ignore_regions {
            matcher = matcher.ignore_regions();
        }
        let raw = matcher.find_all(&pat);
        let generation = graph_guard.generation();
        drop(graph_guard);
        drop(g_borrow);
        // if a `.when()` predicate restored a control-flow
        // exception (KeyboardInterrupt / SystemExit) via PyErr::restore,
        // the exception state is set on this thread.  Surface it as
        // Err so PyO3 raises rather than panicking with
        // "returned a result with an exception set".
        if let Some(err) = PyErr::take(py) {
            return Err(err);
        }
        let mut out = Vec::with_capacity(raw.len());
        for m in raw {
            out.push(crate::matcher::PyMatch {
                inner: m,
                graph: slf.clone_ref(py),
                generation,
            });
        }
        Ok(out)
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
    /// `ignore_casts_mask`, `ignore_regions`) apply uniformly
    /// to every pattern, mirroring `find_all`.
    #[pyo3(signature = (pats, ignore_casts=false, ignore_regions=false, ignore_casts_mask=None))]
    fn find_all_requirements(
        slf: Py<Self>,
        py: Python<'_>,
        pats: Vec<crate::pattern::PatLike<'_>>,
        ignore_casts: bool,
        ignore_regions: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
    ) -> PyResult<Vec<Vec<crate::matcher::PyMatch>>> {
        if ignore_casts && ignore_casts_mask.is_some() {
            return Err(crate::errors::into_pattern_err(anyhow::anyhow!(
                "find_all_requirements: pass either ignore_casts=True or ignore_casts_mask=...; not both"
            )));
        }
        let mut owned: Vec<strider_analyze::pattern::Pat> = Vec::with_capacity(pats.len());
        for p in pats {
            owned.push(p.into_pat()?);
        }
        let pat_refs: Vec<&strider_analyze::pattern::Pat> = owned.iter().collect();
        let g_borrow = slf.borrow(py);
        let graph_guard = g_borrow.read_inner().map_err(crate::errors::into_strider_err)?;
        let mut matcher = strider_analyze::pattern::Matcher::try_new(&graph_guard)
            .map_err(crate::errors::into_strider_err)?;
        if ignore_casts {
            matcher = matcher.ignore_casts();
        } else if let Some(m) = ignore_casts_mask {
            matcher = matcher.ignore_casts_mask(m.inner);
        }
        if ignore_regions {
            matcher = matcher.ignore_regions();
        }
        let raw = matcher.find_all_requirements(&pat_refs);
        let generation = graph_guard.generation();
        drop(graph_guard);
        drop(g_borrow);
        // same propagation as `find_all` — restored
        // exceptions from `.when()` predicates surface here.
        if let Some(err) = PyErr::take(py) {
            return Err(err);
        }
        let mut out: Vec<Vec<crate::matcher::PyMatch>> = Vec::with_capacity(raw.len());
        for tuple in raw {
            let mut py_tuple: Vec<crate::matcher::PyMatch> = Vec::with_capacity(tuple.len());
            for m in tuple {
                py_tuple.push(crate::matcher::PyMatch {
                    inner: m,
                    graph: slf.clone_ref(py),
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
    fn rewrite(
        &self,
        find: crate::pattern::PatLike<'_>,
        replace: crate::pattern::PatLike<'_>,
    ) -> PyResult<usize> {
        let lhs = find.into_pat()?;
        let rhs = replace.into_pat()?;
        let rule = strider_analyze::pattern::rewrite_rule(lhs, rhs);
        let mut graph = self.try_write_inner().map_err(crate::errors::into_strider_err)?;
        let mut rewriter = strider_analyze::GraphRewriter::try_wrap_built(&mut graph)
            .map_err(crate::errors::into_strider_err)?;
        rewriter.apply_rule(rule).map_err(|e| {
            crate::errors::into_rewrite_err(anyhow::anyhow!("rewrite failed: {e:?}"))
        })
    }

    /// Apply a list of `(find, replace)` pairs across the graph round-
    /// robin at every reachable node.  Returns the total fire count.
    fn rewrite_all(
        &self,
        py: Python<'_>,
        pairs: Vec<(Py<crate::pattern::PyPat>, Py<crate::pattern::PyPat>)>,
    ) -> PyResult<usize> {
        // GIL is already held by the #[pymethods] dispatch; take it via
        // the parameter rather than re-acquiring with `Python::with_gil`.
        let mut rules: Vec<strider_analyze::pattern::BoxedRule> = Vec::with_capacity(pairs.len());
        for (lhs, rhs) in pairs {
            let lhs_pat = (*lhs.borrow(py).as_inner()).clone();
            let rhs_pat = (*rhs.borrow(py).as_inner()).clone();
            rules.push(strider_analyze::pattern::boxed_rule(strider_analyze::pattern::rewrite_rule(lhs_pat, rhs_pat)));
        }
        let mut graph = self.try_write_inner().map_err(crate::errors::into_strider_err)?;
        let mut rewriter = strider_analyze::GraphRewriter::try_wrap_built(&mut graph)
            .map_err(crate::errors::into_strider_err)?;
        rewriter.apply_rules(&rules).map_err(|e| {
            crate::errors::into_rewrite_err(anyhow::anyhow!("rewrite_all failed: {e:?}"))
        })
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyGraph>()
}
