//! `pcode_at` / `fingerprint_pcode` are LOOKUPS against the CFG's own
//! lift-time decodes rather than a fresh re-decode, so they stay correct on
//! context-dependent architectures (ARM/Thumb, MIPS16) where a default-context
//! Sleigh would render the wrong p-code for a mid-function mode switch.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use pyo3::prelude::*;

use crate::dot::dot_style_for;
use crate::errors::into_strider_err;
use crate::node::PyNode;
use crate::reader::AnyMemReader;
use crate::strider_cls::PyLifter;

/// Control-flow graph of a single function, from `Lifter.build_cfg` or
/// `Lifter.analyze`. Renders to Graphviz dot / dark-themed HTML, and serves
/// the p-code audit trail (`pcode_at` / `fingerprint_pcode`).
#[pyclass(name = "Cfg", module = "strider.cfg", unsendable)]
pub struct PyCfg {
    pub(crate) inner: strider_cfg::Cfg,
    /// The `Lifter` that built `inner`, borrowed for its Sleigh.
    pub(crate) lifter: Py<PyLifter>,
    /// `machine_addr -> joined p-code text`.
    pcode_map: OnceLock<HashMap<u64, String>>,
}

impl PyCfg {
    pub(crate) fn new(inner: strider_cfg::Cfg, lifter: Py<PyLifter>) -> Self {
        Self {
            inner,
            lifter,
            pcode_map: OnceLock::new(),
        }
    }

    fn with_sleigh<R>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&rsleigh::Sleigh<AnyMemReader>) -> PyResult<R>,
    ) -> PyResult<R> {
        let lifter_borrow = self.lifter.borrow(py);
        f(lifter_borrow.sleigh())
    }

    fn dispatch_dot(
        &self,
        py: Python<'_>,
        style: &str,
        op: CfgDotOp<'_>,
    ) -> PyResult<CfgDotResult> {
        self.with_sleigh(py, |sleigh| {
            let d = dot::GraphDot::new(self.inner.dot_dumper(sleigh), dot_style_for(Some(style))?);
            match op {
                CfgDotOp::ToHtml(p) => d
                    .dump_as_html(Path::new(p))
                    .map(|()| CfgDotResult::Unit)
                    .map_err(into_strider_err),
                CfgDotOp::ToDot(p) => d
                    .dump_as_dot(Path::new(p))
                    .map(|()| CfgDotResult::Unit)
                    .map_err(into_strider_err),
                CfgDotOp::HtmlStr => d
                    .as_html_from_dot()
                    .map(CfgDotResult::Html)
                    .map_err(into_strider_err),
                CfgDotOp::DotStr => d.as_dot().map(CfgDotResult::Dot).map_err(into_strider_err),
            }
        })
    }

    /// Ops of one machine instruction are joined with `"; "` in `insn_index`
    /// order.
    fn pcode_map(&self) -> &HashMap<u64, String> {
        self.pcode_map.get_or_init(|| {
            let mut grouped: HashMap<u64, Vec<(u64, String)>> = HashMap::new();
            for region in self.inner.regions() {
                for ri in &region.insns {
                    grouped
                        .entry(ri.addr.machine_addr.addr)
                        .or_default()
                        .push((ri.addr.insn_index, ri.insn.to_string()));
                }
            }
            grouped
                .into_iter()
                .map(|(addr, mut ops)| {
                    ops.sort_by_key(|(idx, _)| *idx);
                    let text = ops
                        .into_iter()
                        .map(|(_, text)| text)
                        .collect::<Vec<_>>()
                        .join("; ");
                    (addr, text)
                })
                .collect()
        })
    }
}

enum CfgDotOp<'a> {
    ToHtml(&'a str),
    ToDot(&'a str),
    HtmlStr,
    DotStr,
}

enum CfgDotResult {
    Unit,
    Html(String),
    Dot(String),
}

#[pymethods]
impl PyCfg {
    /// Exposes the strong `lifter` back-reference so the cyclic GC can see a
    /// cycle routed through a `Cfg` and on to the Lifter's Python reader. No
    /// `__clear__`: the cycle is broken at the reader's `__dict__` /
    /// `PyLifter::__clear__`, and `lifter` is load-bearing while the `Cfg`
    /// lives.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.lifter)
    }

    /// Render the CFG to DOT. Returns the DOT string when `path` is
    /// `None`, otherwise writes it to `path` and returns `None`.
    #[pyo3(signature = (path=None))]
    fn to_dot(&self, py: Python<'_>, path: Option<&str>) -> PyResult<Option<String>> {
        match path {
            Some(p) => self
                .dispatch_dot(py, "dark_cfg", CfgDotOp::ToDot(p))
                .map(|_| None),
            None => match self.dispatch_dot(py, "dark_cfg", CfgDotOp::DotStr)? {
                CfgDotResult::Dot(s) => Ok(Some(s)),
                _ => Ok(None),
            },
        }
    }

    /// Render the CFG to a standalone HTML page. Returns the HTML string
    /// when `path` is `None`, otherwise writes it and returns `None`.
    /// `style` selects the dot theme (default `"dark_cfg"`).
    #[pyo3(signature = (path=None, style=None))]
    fn to_html(
        &self,
        py: Python<'_>,
        path: Option<&str>,
        style: Option<&str>,
    ) -> PyResult<Option<String>> {
        let style = style.unwrap_or("dark_cfg");
        match path {
            Some(p) => self
                .dispatch_dot(py, style, CfgDotOp::ToHtml(p))
                .map(|_| None),
            None => match self.dispatch_dot(py, style, CfgDotOp::HtmlStr)? {
                CfgDotResult::Html(s) => Ok(Some(s)),
                _ => Ok(None),
            },
        }
    }

    /// The lifted p-code for the machine instruction at `addr`: every p-code
    /// op decoded there, joined with `"; "`, or `None` when this CFG decoded
    /// nothing at `addr`.
    ///
    /// Known limitation: an instruction lifting to ZERO p-code ops (x86
    /// `endbr64`, AArch64 `paciasp`) is indistinguishable from an address
    /// never decoded; both give `None`.  `Lifter.pcode_at` returns `""`.
    fn pcode_at(&self, addr: u64) -> Option<String> {
        self.pcode_map().get(&addr).cloned()
    }

    /// `node`'s asm fingerprint as `(addr, text)` p-code pairs sorted by
    /// address. `[]` for structural nodes carrying no fingerprint.
    ///
    /// An address this CFG has no decode for is skipped, so every pair is a
    /// genuine hit.
    fn fingerprint_pcode(
        &self,
        py: Python<'_>,
        node: PyRef<'_, PyNode>,
    ) -> PyResult<Vec<(u64, String)>> {
        let addrs = node.asm_fingerprint(py)?;
        let map = self.pcode_map();
        let mut out: Vec<(u64, String)> = addrs
            .into_iter()
            .filter_map(|addr| map.get(&addr).cloned().map(|text| (addr, text)))
            .collect();
        out.sort_by_key(|(addr, _)| *addr);
        Ok(out)
    }

    /// Region index of the CFG entry.
    fn entry(&self) -> u32 {
        self.inner.entry().index() as u32
    }

    /// Pretty neighborhood DOT around region `center`: BFS over predecessor
    /// and successor regions, capped at `max_nodes`.
    #[pyo3(signature = (center, depth=5, max_nodes=60))]
    fn neighborhood_dot(
        &self,
        py: Python<'_>,
        center: u32,
        depth: usize,
        max_nodes: usize,
    ) -> PyResult<String> {
        let node = strider_cfg::RegionId::new(center as usize);
        self.with_sleigh(py, |sleigh| {
            self.inner
                .neighborhood_dot(sleigh, node, depth, max_nodes)
                .map_err(into_strider_err)
        })
    }

    /// Disassembly text per region index, joined with `"\n"`.
    #[pyo3(name = "_region_texts")]
    fn region_texts(&self, py: Python<'_>) -> PyResult<HashMap<u32, String>> {
        self.with_sleigh(py, |sleigh| {
            let regs = sleigh
                .regs()
                .map_err(|e| into_strider_err(anyhow::anyhow!("{e:?}")))?;
            let g = self.inner.region_graph();
            let mut out = HashMap::new();
            for idx in g.node_indices() {
                let region = g
                    .node_weight(idx)
                    .expect("node_indices() only yields present nodes");
                let text = region
                    .insns
                    .iter()
                    .map(|ri| ri.insn.ctx_fmt(sleigh, &regs).to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                out.insert(idx.index() as u32, text);
            }
            Ok(out)
        })
    }

    /// The region index whose instruction range contains `addr`, if any.
    fn region_at(&self, addr: u64) -> Option<u32> {
        let g = self.inner.region_graph();
        for idx in g.node_indices() {
            let region = g
                .node_weight(idx)
                .expect("node_indices() only yields present nodes");
            let start = region.start_addr.machine_addr.addr;
            let last = region
                .insns
                .last()
                .map_or(start, |i| i.addr.machine_addr.addr);
            if start <= addr && addr <= last {
                return Some(idx.index() as u32);
            }
        }
        None
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    Ok(())
}
