//! `pcode_at` / `fingerprint_pcode` are LOOKUPS against the CFG's own
//! lift-time decodes rather than a fresh re-decode, so they stay correct on
//! context-dependent architectures (ARM/Thumb, MIPS16) where a default-context
//! Sleigh would render the wrong p-code for a mid-function mode switch.

use std::collections::HashMap;
use std::sync::OnceLock;

use pyo3::prelude::*;

use crate::dot::{DEFAULT_CFG_STYLE, dot_style_for, render};
use crate::errors::into_strider_err;
use crate::node::PyNode;
use crate::strider_cls::PyLifter;

/// Control-flow graph of a single function, from `Lifter.build_cfg` or
/// `Lifter.analyze`. Renders to Graphviz dot / dark-themed HTML, and serves
/// the p-code audit trail (`pcode_at` / `fingerprint_pcode`).
#[pyclass(name = "Cfg", module = "strider.cfg")]
pub struct PyCfg {
    pub(crate) inner: strider_cfg::Cfg,
    /// The `Lifter` that built `inner`: `Cfg.lifter`, and `optimize`'s
    /// same-handle check.
    pub(crate) lifter: Py<PyLifter>,
    /// That lifter's arch, whose name table every render reads, so a render
    /// never borrows `lifter` (which `analyze` holds mutably with the GIL
    /// released).
    pub(crate) arch: strider_target::SleighArch,
    /// `machine_addr -> joined p-code text`.
    pcode_map: OnceLock<HashMap<u64, String>>,
}

/// `incomplete` at machine-address granularity, sorted and deduplicated: what
/// `Cfg.incomplete` reads.
pub(crate) fn incomplete_list(
    incomplete: &strider_cfg::Incompleteness,
) -> Vec<(u64, &'static str)> {
    let by_machine_addr: std::collections::BTreeSet<(u64, strider_cfg::Reason)> = incomplete
        .iter()
        .map(|(addr, reason)| (addr.machine_addr.addr, reason))
        .collect();
    by_machine_addr
        .into_iter()
        .map(|(addr, reason)| (addr, reason.name()))
        .collect()
}

impl PyCfg {
    pub(crate) fn new(
        inner: strider_cfg::Cfg,
        lifter: Py<PyLifter>,
        arch: strider_target::SleighArch,
    ) -> Self {
        Self {
            inner,
            lifter,
            arch,
            pcode_map: OnceLock::new(),
        }
    }

    /// `center` as a region id this CFG actually has.
    ///
    /// `RegionId::new` takes any `usize`, so an out-of-range argument would
    /// render an empty graph rather than say anything, and `u32::MAX` is
    /// petgraph's end sentinel, never a region.
    fn region_id(&self, center: u32) -> PyResult<strider_cfg::RegionId> {
        if center != u32::MAX {
            let id = strider_cfg::RegionId::new(center as usize);
            if self.inner.region_graph().node_weight(id).is_some() {
                return Ok(id);
            }
        }
        Err(into_strider_err(anyhow::anyhow!(
            "invalid region id {center}"
        )))
    }

    fn render_dot(
        &self,
        style: Option<&str>,
        html: bool,
        path: Option<&str>,
    ) -> PyResult<Option<String>> {
        let style = style.unwrap_or(DEFAULT_CFG_STYLE);
        let d = dot::GraphDot::new(
            self.inner.dot_dumper(self.arch.names()),
            dot_style_for(Some(style))?,
        );
        render(&d, html, path)
    }

    /// Ops of one machine instruction are joined with `"; "` in `insn_index`
    /// order.
    fn pcode_map(&self) -> &HashMap<u64, String> {
        self.pcode_map.get_or_init(|| {
            let mut grouped: HashMap<u64, Vec<(u64, String)>> = HashMap::new();
            for region in self.inner.regions() {
                for ri in &region.insns {
                    grouped.entry(ri.addr.machine_addr.addr).or_default().push((
                        ri.addr.insn_index,
                        strider_cfg::insn_plain_text(&ri.insn, self.inner.space_ids()).to_string(),
                    ));
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

#[pymethods]
impl PyCfg {
    /// The `Lifter` that built this CFG.
    #[getter]
    fn lifter(&self, py: Python<'_>) -> Py<PyLifter> {
        self.lifter.clone_ref(py)
    }

    /// Every machine address this CFG cannot vouch for, with why, sorted and
    /// deduplicated. Empty is complete. `analyze` and `build_cfg` return
    /// whatever this names, so check it before trusting an absence ("no store
    /// to X"): code behind a listed address may be missing.
    ///
    /// `"unresolved_branch"` and `"unverified_seed"` name a dispatch site, the
    /// address `CfgOptions(known_targets=...)` seeds; the other four a branch
    /// target, which a direct edge reaches too:
    ///
    /// - `"unresolved_branch"`: an indirect branch with no complete successor
    ///   set, among them a return whose target is not provably the caller's.
    /// - `"unverified_seed"`: a seated answer nothing verified, such as a site
    ///   seated with only your `known_targets`, every tail call the CFG
    ///   consumed, and your own `"return"` seat (where a failed re-check of it
    ///   lands too).
    /// - `"isa_mode_conflict"`: reached in two ISA modes; the losing path
    ///   decodes in the other.
    /// - `"interior_target"`: off every instruction boundary, or a region
    ///   start a later decode stepped over.
    /// - `"unmapped_target"`: no byte of the image backs it.
    /// - `"undecodable_target"`: holds no instruction, or `data_ranges` marks
    ///   it as data.
    ///
    /// From `build_cfg` this is that one build: every dispatch it left without
    /// an edge and every seed it seated. From `analyze` the target reasons
    /// accumulate over the resolver's rounds and the site reasons are the
    /// final round's.
    #[getter]
    fn incomplete(&self) -> Vec<(u64, &'static str)> {
        incomplete_list(self.inner.incomplete())
    }

    /// Exposes the strong `lifter` back-reference so the cyclic GC can see a
    /// cycle routed through a `Cfg` and on to the Lifter's Python reader. The
    /// cycle is broken at the reader's `__dict__` / `PyLifter::__clear__`, and
    /// `lifter` is load-bearing while the `Cfg` lives.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.lifter)
    }

    /// Render the CFG to DOT. Returns the DOT string when `path` is
    /// `None`, otherwise writes it to `path` and returns `None`.
    /// `style` selects the dot theme (default `"dark_cfg"`).
    #[pyo3(signature = (path=None, style=None))]
    fn to_dot(&self, path: Option<&str>, style: Option<&str>) -> PyResult<Option<String>> {
        self.render_dot(style, false, path)
    }

    /// Render the CFG to a standalone HTML page. Returns the HTML string
    /// when `path` is `None`, otherwise writes it and returns `None`.
    /// `style` selects the dot theme (default `"dark_cfg"`).
    #[pyo3(signature = (path=None, style=None))]
    fn to_html(&self, path: Option<&str>, style: Option<&str>) -> PyResult<Option<String>> {
        self.render_dot(style, true, path)
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
    ///
    /// Raises `StriderError` for a `node` from a function some other `Cfg`
    /// produced: addresses collide across images, so the answer would be this
    /// CFG's p-code labelled with the other's node.
    fn fingerprint_pcode(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        node: PyRef<'_, PyNode>,
    ) -> PyResult<Vec<(u64, String)>> {
        if !node.function.bind(py).try_borrow()?.cfg.bind(py).is(slf) {
            return Err(into_strider_err(anyhow::anyhow!(
                "this Node belongs to a function another Cfg lifted; a \
                 fingerprint is machine addresses, which collide across images, \
                 so the two must be the same Cfg"
            )));
        }
        let slf = slf.borrow();
        let addrs = node.asm_fingerprint(py)?;
        let map = slf.pcode_map();
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

    fn __repr__(&self) -> String {
        format!(
            "Cfg({} regions, entry=#{})",
            self.inner.regions().count(),
            self.inner.entry().index(),
        )
    }

    /// Pretty neighborhood DOT around region `center`: BFS over predecessor
    /// and successor regions, capped at `max_nodes`.
    #[pyo3(signature = (center, depth=5, max_nodes=60))]
    fn neighborhood_dot(&self, center: u32, depth: usize, max_nodes: usize) -> PyResult<String> {
        let node = self.region_id(center)?;
        self.inner
            .neighborhood_dot(self.arch.names(), node, depth, max_nodes)
            .map_err(into_strider_err)
    }

    /// Disassembly text per region index, joined with `"\n"`.
    #[pyo3(name = "_region_texts")]
    fn region_texts(&self) -> HashMap<u32, String> {
        let names = self.arch.names();
        let g = self.inner.region_graph();
        g.node_indices()
            .map(|idx| {
                let region = g
                    .node_weight(idx)
                    .expect("node_indices() only yields present nodes");
                let text = region
                    .insns
                    .iter()
                    .map(|ri| {
                        strider_cfg::insn_text(&ri.insn, self.inner.space_ids(), names).to_string()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (idx.index() as u32, text)
            })
            .collect()
    }

    /// The index of the region owning `addr`; where regions overlap, the one
    /// with the greatest start.
    fn region_at(&self, addr: u64) -> Option<u32> {
        self.inner
            .region_containing(strider_cfg::PcodeInsnAddr::at_machine_start(addr))
            .map(|id| id.index() as u32)
    }
}

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    m.add(
        "INCOMPLETE_REASONS",
        pyo3::types::PyTuple::new_bound(
            py,
            strider_cfg::Reason::ALL.map(strider_cfg::Reason::name),
        ),
    )?;
    Ok(())
}
