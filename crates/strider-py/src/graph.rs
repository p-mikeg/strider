//! `PyGraph` — wraps `ir::BuiltFunctionGraph` and exposes dot
//! rendering plus (in later tasks) pattern queries and rewrites.
//!
//! The IR graph's dot dumper requires a borrowed `Sleigh` for
//! register-name resolution.  PyGraph keeps a `Py<PyCfg>` reference
//! so the Sleigh stays alive for the graph's lifetime and is
//! reachable through `cfg::Cfg::sleigh`.

use std::sync::{Arc, RwLock};

use pyo3::prelude::*;

use crate::cfg::PyCfg;
use crate::dot::{dot_style_for, dump_dot, dump_html, html_str};

/// Opaque wrapper over `ir::BuiltFunctionGraph`.
///
/// The graph is held in `Arc<RwLock<...>>` so optimization passes
/// (added in phase 3) can mutate it without requiring `&mut self` on
/// the PyGraph wrapper, and so the same graph can be shared across
/// multiple Python references.
#[pyclass(name = "Graph", module = "strider")]
pub struct PyGraph {
    pub(crate) inner: Arc<RwLock<ir::BuiltFunctionGraph>>,
    /// Strong reference to the parent Cfg; keeps the Sleigh alive for
    /// dot rendering and ensures destruction order is graph-then-cfg.
    pub(crate) cfg: Py<PyCfg>,
}

/// Convert a Python-supplied `u32` node id into a validated `ir::NodeId`,
/// returning `StriderError` on lookup failure.
fn node_id_from_u32(graph: &ir::BuiltFunctionGraph, node_id: u32) -> PyResult<ir::node::NodeId> {
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
    pub(crate) fn new(graph: ir::BuiltFunctionGraph, cfg: Py<PyCfg>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(graph)),
            cfg,
        }
    }

    /// Borrow the inner graph for read.  Returns an `anyhow::Error`
    /// when the lock is poisoned.
    #[allow(dead_code)]
    pub(crate) fn read_inner(&self) -> anyhow::Result<std::sync::RwLockReadGuard<'_, ir::BuiltFunctionGraph>> {
        self.inner
            .read()
            .map_err(|_| anyhow::anyhow!("Graph lock poisoned"))
    }

    /// Borrow the inner graph for write.  Returns an `anyhow::Error`
    /// when the lock is poisoned.
    #[allow(dead_code)]
    pub(crate) fn write_inner(&self) -> anyhow::Result<std::sync::RwLockWriteGuard<'_, ir::BuiltFunctionGraph>> {
        self.inner
            .write()
            .map_err(|_| anyhow::anyhow!("Graph lock poisoned"))
    }
}

#[pymethods]
impl PyGraph {
    #[pyo3(signature = (path, style=None))]
    fn to_html(&self, py: Python<'_>, path: &str, style: Option<&str>) -> PyResult<()> {
        let style = style.unwrap_or("dark");
        let cfg_borrow = self.cfg.borrow(py);
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let dumper = graph.dot_dumper(&cfg_borrow.inner.sleigh);
        let d = dot::GraphDot::new(dumper, dot_style_for(Some(style)));
        dump_html(&d, path)
    }

    #[pyo3(signature = (path,))]
    fn to_dot(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let cfg_borrow = self.cfg.borrow(py);
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let dumper = graph.dot_dumper(&cfg_borrow.inner.sleigh);
        let d = dot::GraphDot::new(dumper, dot_style_for(Some("dark")));
        dump_dot(&d, path)
    }

    #[pyo3(signature = (style=None))]
    fn html_str(&self, py: Python<'_>, style: Option<&str>) -> PyResult<String> {
        let style = style.unwrap_or("dark");
        let cfg_borrow = self.cfg.borrow(py);
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let dumper = graph.dot_dumper(&cfg_borrow.inner.sleigh);
        let d = dot::GraphDot::new(dumper, dot_style_for(Some(style)));
        html_str(&d)
    }

    fn node_count(&self) -> PyResult<usize> {
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        Ok(graph.all_node_ids().count())
    }

    /// Returns the number of CFG loop headers — `ControlState` nodes
    /// reachable from entry that have at least one back-edge predecessor
    /// (a predecessor itself reachable from the `ControlState` via
    /// forward control flow).
    ///
    /// This is structurally robust under optimization: a loop with a
    /// loop-invariant tracked variable that `RedundantPhis` collapses
    /// (so no `VarPhi` remains at the header) is still counted, because
    /// the back-edge in the control-flow graph is unaffected.  Use this
    /// instead of counting `pat.phi()` matches when a test wants to
    /// assert "the lifter recognised a loop here".
    fn count_loop_headers(&self) -> PyResult<usize> {
        use std::collections::HashSet;
        use ir::node::{NodeId, NodeKind};
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let reachable: HashSet<NodeId> = graph.preorder().collect();
        let mut count = 0usize;
        for n in graph.all_node_ids() {
            if !reachable.contains(&n) {
                continue;
            }
            if !matches!(graph.graph.node_kind(n), NodeKind::ControlState) {
                continue;
            }
            let preds: Vec<_> = graph.graph.node_inputs(n).into_iter().collect();
            let has_back_edge = preds.iter().any(|&pred_out| {
                let pred = graph.graph.get_node_from_output(pred_out);
                let mut seen: HashSet<NodeId> = HashSet::new();
                let mut stack = vec![pred];
                while let Some(cur) = stack.pop() {
                    if !seen.insert(cur) {
                        continue;
                    }
                    for out in graph.graph.node_outputs(cur) {
                        if !graph.graph.output_kind(out).is_control() {
                            continue;
                        }
                        for (consumer, _) in graph.graph.output_uses(out) {
                            if consumer == n {
                                return true;
                            }
                            stack.push(consumer);
                        }
                    }
                }
                false
            });
            if has_back_edge {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Returns a list of all reachable node ids in the graph as raw
    /// integers.  Useful for iterating from Python without going
    /// through pattern matching.
    fn node_ids(&self) -> PyResult<Vec<u32>> {
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        Ok(graph.all_node_ids().map(|n| n.as_u32()).collect())
    }

    /// Returns the [`NodeKind`] of the node at `node_id`, formatted as
    /// a string (e.g. "IntConst", "Call", "VarPhi", "Add", …).  Useful
    /// for direct graph introspection from Python tests / debug
    /// scripts.
    ///
    /// Raises `StriderError` for an invalid `node_id`.
    fn node_kind(&self, node_id: u32) -> PyResult<String> {
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let nid = node_id_from_u32(&graph, node_id)?;
        Ok(format!("{:?}", graph.graph.node_kind(nid)))
    }

    /// Returns the asm-fingerprint addresses recorded on the node at
    /// `node_id` — a sorted, deduped list of machine-instruction
    /// addresses whose lift contributed to the node's value.
    ///
    /// Empty for "structural" node kinds (Entry, InitialMemory, phis,
    /// ControlState, FunctionArg) whose existence is synthesised by
    /// the IR builder rather than tied to a specific asm instruction.
    fn asm_fingerprint(&self, node_id: u32) -> PyResult<Vec<u64>> {
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let nid = node_id_from_u32(&graph, node_id)?;
        Ok(graph.graph.asm_fingerprint(nid).to_vec())
    }

    /// Returns the Sleigh user-op name attached to a `CallOther` node,
    /// or `None` for any other node kind.
    fn call_other_name(&self, node_id: u32) -> PyResult<Option<String>> {
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let nid = node_id_from_u32(&graph, node_id)?;
        Ok(graph.graph.call_other_name(nid).map(str::to_owned))
    }

    /// Re-validates the graph and returns `None` on success or a
    /// human-readable error message on failure.
    ///
    /// `check_asm_fingerprints=True` enables the opt-in Layer-C check
    /// that flags every reachable non-exempt node with an empty
    /// asm-fingerprint — useful when verifying a fresh opt pass
    /// preserves the superset contract.
    #[pyo3(signature = (check_asm_fingerprints = false))]
    fn validate(&self, check_asm_fingerprints: bool) -> PyResult<Option<String>> {
        let graph = self
            .inner
            .read()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let opts = ir::validate::ValidateOptions { check_asm_fingerprints };
        match ir::validate::validate_with_options(&graph.graph, graph.entry, opts) {
            Ok(()) => Ok(None),
            Err(e) => Ok(Some(format!("{e}"))),
        }
    }

    /// Compact the graph arena: drop every node not reachable from
    /// `entry` via [`ir::walk::walk_graph`].  Mutates in place.
    /// Pre-compaction node ids become invalid across this call.
    fn compact(&self) -> PyResult<()> {
        let mut graph = self
            .inner
            .write()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let _remap = graph.compact();
        Ok(())
    }

    /// Apply a `PyOptimizerPipeline` to this graph in place.  Drains
    /// the pipeline (subsequent calls to the same pipeline see an
    /// empty pass list); rebuild it from `OptimizerPipeline.default()`
    /// or the equivalent classmethods if you need to apply it again.
    fn optimize(&self, pipeline: &crate::opt::PyOptimizerPipeline) -> PyResult<()> {
        let real_pipeline = pipeline.drain_into_pipeline()?;
        let mut graph = self
            .inner
            .write()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        real_pipeline
            .run_on_built(&mut graph)
            .map_err(|e| crate::errors::into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))
    }

    /// Convenience: re-run the stable pipeline (and optionally the
    /// destructive pipeline) on this graph.  Useful after a manual
    /// rewrite (`graph.rewrite(...)`) to re-converge the graph.
    #[pyo3(signature = (destructive=false))]
    fn reoptimize(&self, destructive: bool) -> PyResult<()> {
        let mut pipe = opt::stable_default_pipeline();
        if destructive {
            // Append the destructive passes after the stable ones.
            pipe.add(opt::RedundantPhis);
            pipe.add(opt::DeadBranchElimination);
        }
        let mut graph = self
            .inner
            .write()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        pipe.run_on_built(&mut graph).map_err(|e| {
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
    /// * `ignore_control_states=True` — walk through `ControlState`
    ///   region-join nodes between an `If`'s output and the
    ///   matched consumer.
    #[pyo3(signature = (pat, ignore_casts=false, ignore_control_states=false, ignore_casts_mask=None))]
    fn find_all(
        slf: Py<Self>,
        py: Python<'_>,
        pat: crate::pattern::PatLike<'_>,
        ignore_casts: bool,
        ignore_control_states: bool,
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
        let mut matcher = pattern::Matcher::new(&graph_guard);
        if ignore_casts {
            matcher = matcher.ignore_casts();
        } else if let Some(m) = ignore_casts_mask {
            matcher = matcher.ignore_casts_mask(m.inner);
        }
        if ignore_control_states {
            matcher = matcher.ignore_control_states();
        }
        let raw = matcher.find_all(&pat);
        drop(graph_guard);
        drop(g_borrow);
        let mut out = Vec::with_capacity(raw.len());
        for m in raw {
            out.push(crate::matcher::PyMatch {
                inner: m,
                graph: slf.clone_ref(py),
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
    /// `ignore_casts_mask`, `ignore_control_states`) apply uniformly
    /// to every pattern, mirroring `find_all`.
    #[pyo3(signature = (pats, ignore_casts=false, ignore_control_states=false, ignore_casts_mask=None))]
    fn find_all_requirements(
        slf: Py<Self>,
        py: Python<'_>,
        pats: Vec<crate::pattern::PatLike<'_>>,
        ignore_casts: bool,
        ignore_control_states: bool,
        ignore_casts_mask: Option<crate::pattern::PyCastMask>,
    ) -> PyResult<Vec<Vec<crate::matcher::PyMatch>>> {
        if ignore_casts && ignore_casts_mask.is_some() {
            return Err(crate::errors::into_pattern_err(anyhow::anyhow!(
                "find_all_requirements: pass either ignore_casts=True or ignore_casts_mask=...; not both"
            )));
        }
        let mut owned: Vec<pattern::Pat> = Vec::with_capacity(pats.len());
        for p in pats {
            owned.push(p.into_pat()?);
        }
        let pat_refs: Vec<&pattern::Pat> = owned.iter().collect();
        let g_borrow = slf.borrow(py);
        let graph_guard = g_borrow.read_inner().map_err(crate::errors::into_strider_err)?;
        let mut matcher = pattern::Matcher::new(&graph_guard);
        if ignore_casts {
            matcher = matcher.ignore_casts();
        } else if let Some(m) = ignore_casts_mask {
            matcher = matcher.ignore_casts_mask(m.inner);
        }
        if ignore_control_states {
            matcher = matcher.ignore_control_states();
        }
        let raw = matcher.find_all_requirements(&pat_refs);
        drop(graph_guard);
        drop(g_borrow);
        let mut out: Vec<Vec<crate::matcher::PyMatch>> = Vec::with_capacity(raw.len());
        for tuple in raw {
            let mut py_tuple: Vec<crate::matcher::PyMatch> = Vec::with_capacity(tuple.len());
            for m in tuple {
                py_tuple.push(crate::matcher::PyMatch {
                    inner: m,
                    graph: slf.clone_ref(py),
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
        let rule = pattern::rewrite_rule(lhs, rhs);
        let mut graph = self
            .inner
            .write()
            .map_err(|_| crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned")))?;
        let mut rewriter = strider::GraphRewriter::wrap_built(&mut graph);
        rewriter.apply_rule(rule).map_err(|e| {
            crate::errors::into_rewrite_err(anyhow::anyhow!("rewrite failed: {e:?}"))
        })
    }

    /// Apply a list of `(find, replace)` pairs across the graph round-
    /// robin at every reachable node.  Returns the total fire count.
    fn rewrite_all(&self, pairs: Vec<(Py<crate::pattern::PyPat>, Py<crate::pattern::PyPat>)>) -> PyResult<usize> {
        Python::with_gil(|py| {
            let mut rules: Vec<pattern::BoxedRule> = Vec::with_capacity(pairs.len());
            for (lhs, rhs) in pairs {
                let lhs_pat = (*lhs.borrow(py).as_inner()).clone();
                let rhs_pat = (*rhs.borrow(py).as_inner()).clone();
                rules.push(pattern::boxed_rule(pattern::rewrite_rule(lhs_pat, rhs_pat)));
            }
            let mut graph = self.inner.write().map_err(|_| {
                crate::errors::into_strider_err(anyhow::anyhow!("Graph lock poisoned"))
            })?;
            let mut rewriter = strider::GraphRewriter::wrap_built(&mut graph);
            rewriter.apply_rules(&rules).map_err(|e| {
                crate::errors::into_rewrite_err(anyhow::anyhow!("rewrite_all failed: {e:?}"))
            })
        })
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyGraph>()
}
