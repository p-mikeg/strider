//! `pcode_at` / `fingerprint_pcode` are LOOKUPS against the CFG's own
//! lift-time decodes rather than a fresh re-decode, so they stay correct on
//! context-dependent architectures (ARM/Thumb, MIPS16) where a default-context
//! Sleigh would render the wrong p-code for a mid-function mode switch.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use pyo3::prelude::*;

use crate::dot::{DEFAULT_CFG_STYLE, dot_style_for};
use crate::errors::into_strider_err;
use crate::node::PyNode;
use crate::reader::AnyMemReader;
use crate::strider_cls::{PyLifter, machine_addrs};

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
    region_index: OnceLock<RegionIndex>,
    reports: CfgReports,
}

/// The incompleteness channels as `analyze` reported them. Read from here
/// rather than from `inner`, which is the FINAL round's CFG: a round that
/// raised a report and was then rebuilt without the edge that raised it
/// leaves nothing behind in `inner`.
///
/// `unresolved`, `isa_mode_conflicts` and `interior_branch_targets`
/// accumulate over the resolver's rounds, so a later round cannot launder an
/// earlier loss; `unverified_seeded` is derived once from the final CFG.
pub(crate) struct CfgReports {
    /// The same list `AnalyzeResult.unresolved` carries, held here so
    /// `is_complete` can test all four channels from one object. Empty for a
    /// `build_cfg` result, which resolves nothing.
    pub(crate) unresolved: Vec<u64>,
    /// Empty for a `build_cfg` result: only `analyze` runs the resolver that
    /// decides which seeds went unchecked.
    pub(crate) unverified_seeded: Vec<u64>,
    pub(crate) isa_mode_conflicts: Vec<u64>,
    pub(crate) interior_branch_targets: Vec<u64>,
}

/// Region starts in address order, plus the longest span any region covers:
/// a region containing `addr` starts no lower than `addr - max_span`, which
/// bounds `region_at`'s reverse walk. `Region::contains_addr` stays the only
/// authority on containment.
struct RegionIndex {
    starts: Vec<(strider_cfg::PcodeInsnAddr, strider_cfg::RegionId)>,
    max_span: u64,
}

/// Over-estimating costs probes, never a missed owner.
fn span_upper_bound(region: &strider_cfg::Region) -> u64 {
    let start = region.start_addr.machine_addr.addr;
    match region.insns.last() {
        Some(last) => last
            .addr
            .machine_addr
            .addr
            .saturating_add(u64::from(last.len)),
        None => start.saturating_add(u64::from(region.empty_span_len)),
    }
    .saturating_sub(start)
}

impl PyCfg {
    /// The `build_cfg` path: one build, no resolver, so that build's own
    /// reports are the whole accumulation.
    pub(crate) fn new(inner: strider_cfg::Cfg, lifter: Py<PyLifter>) -> Self {
        let reports = CfgReports {
            unresolved: Vec::new(),
            unverified_seeded: Vec::new(),
            isa_mode_conflicts: machine_addrs(inner.isa_mode_conflicts()),
            interior_branch_targets: machine_addrs(inner.interior_branch_targets()),
        };
        Self::with_reports(inner, lifter, reports)
    }

    pub(crate) fn with_reports(
        inner: strider_cfg::Cfg,
        lifter: Py<PyLifter>,
        reports: CfgReports,
    ) -> Self {
        Self {
            inner,
            lifter,
            pcode_map: OnceLock::new(),
            region_index: OnceLock::new(),
            reports,
        }
    }

    /// Sound to cache: `inner` is moved in here and never mutated, the same
    /// reason `pcode_map` caches.
    fn region_index(&self) -> &RegionIndex {
        self.region_index.get_or_init(|| {
            let g = self.inner.region_graph();
            let regions = || {
                g.node_indices().map(|id| {
                    let region = g
                        .node_weight(id)
                        .expect("node_indices() only yields present nodes");
                    (id, region)
                })
            };
            let mut starts: Vec<_> = regions().map(|(id, r)| (r.start_addr, id)).collect();
            starts.sort_unstable_by_key(|&(addr, id)| (addr, id.index()));
            RegionIndex {
                starts,
                max_span: regions()
                    .map(|(_, r)| span_upper_bound(r))
                    .max()
                    .unwrap_or(0),
            }
        })
    }

    fn with_sleigh<R>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&rsleigh::Sleigh<AnyMemReader>) -> PyResult<R>,
    ) -> PyResult<R> {
        // `borrow` would panic when the owning `Lifter` is mid-`analyze`
        // (a `read()` callback rendering this Cfg), and that panic aborts:
        // it cannot unwind out of rsleigh's `extern "C"` fetch callback.
        let lifter_borrow = self
            .lifter
            .try_borrow(py)
            .map_err(|_| crate::strider_cls::reentrant_lifter_err())?;
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
    /// Machine addresses this CFG reached carrying two different ISA modes.
    ///
    /// One region owns the bytes, decoded in whichever mode the work queue
    /// reached first, so the losing path's arm is not the instruction stream it
    /// believes. A direct edge can cause this, not only a resolved indirect
    /// branch, which is why it is reported here rather than in `unresolved`.
    /// Non-empty means part of this CFG is decoded in a mode some path into it
    /// disagrees with.
    ///
    /// Accumulated over every round `analyze` ran: a clash decodes the bytes
    /// twice and costs the site, whose next round rebuilds without the edge
    /// that raised it, so the final CFG alone would launder the report.
    fn isa_mode_conflicts(&self) -> Vec<u64> {
        self.reports.isa_mode_conflicts.clone()
    }

    /// Branch targets interior to a region but off every instruction boundary.
    ///
    /// No region can start there (decoding from inside an instruction yields a
    /// different stream), so the edge is seated on the region owning the
    /// bytes, whose instructions start earlier. A direct edge can cause this,
    /// not only a resolved indirect branch. Non-empty means this CFG claims a
    /// successor the branch does not actually enter at.
    ///
    /// Accumulated over every round `analyze` ran: the inexact edge fed the
    /// classifier, whose derived targets outlive it, so a later round that
    /// no longer carries the edge does not undo what it cost.
    fn interior_branch_targets(&self) -> Vec<u64> {
        self.reports.interior_branch_targets.clone()
    }

    /// Dispatch addresses nothing verified: a site seated with exactly the
    /// `known_targets` you supplied and nothing the classifier derived, plus
    /// every site the CFG consumed outright as a return or a tail call,
    /// whether that answer was seeded or derived.
    ///
    /// Not unresolved (you asserted the answer), but nothing checked it.
    /// Seating a seed changes the CFG the classifier reads, so a stale or wrong
    /// seed can stop it deriving and take the site's real arms with it. These
    /// are the sites where that cannot be ruled out. Always empty for a CFG
    /// from `build_cfg`, which runs no resolver.
    fn unverified_seeded_sites(&self) -> Vec<u64> {
        self.reports.unverified_seeded.clone()
    }

    /// Whether all four incompleteness channels are empty: the `unresolved`
    /// of the `AnalyzeResult` this CFG came from, `unverified_seeded_sites`,
    /// `isa_mode_conflicts` and `interior_branch_targets`.
    ///
    /// The answer to "may this be incomplete?", which none of the four gives
    /// alone. `False` is not always a loss: `unverified_seeded_sites` holds
    /// answers that are complete but unverified, so a site consumed as a
    /// return (an ARM `pop {pc}` epilogue) clears it. Read whichever channel
    /// is non-empty to tell the cases apart.
    ///
    /// On a `build_cfg` CFG the first two channels are empty by construction,
    /// so `True` there means only that no ISA mode clashed and no branch
    /// landed off an instruction boundary. It says nothing about indirect
    /// branches, which `build_cfg` never resolves.
    fn is_complete(&self) -> bool {
        let r = &self.reports;
        r.unresolved.is_empty()
            && r.unverified_seeded.is_empty()
            && r.isa_mode_conflicts.is_empty()
            && r.interior_branch_targets.is_empty()
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
    fn to_dot(
        &self,
        py: Python<'_>,
        path: Option<&str>,
        style: Option<&str>,
    ) -> PyResult<Option<String>> {
        let style = style.unwrap_or(DEFAULT_CFG_STYLE);
        match path {
            Some(p) => self
                .dispatch_dot(py, style, CfgDotOp::ToDot(p))
                .map(|_| None),
            None => match self.dispatch_dot(py, style, CfgDotOp::DotStr)? {
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
        let style = style.unwrap_or(DEFAULT_CFG_STYLE);
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
    /// Lowest index when regions overlap, as a full scan reports.
    fn region_at(&self, addr: u64) -> Option<u32> {
        let target = strider_cfg::PcodeInsnAddr::at_machine_start(addr);
        let index = self.region_index();
        let below = &index.starts[..index.starts.partition_point(|&(s, _)| s <= target)];
        let (greatest, _) = *below.last()?;
        // The greatest start below `addr` is always probed: it can own `addr`
        // however short every span is.
        let floor = addr
            .saturating_sub(index.max_span)
            .min(greatest.machine_addr.addr);
        let g = self.inner.region_graph();
        below
            .iter()
            .rev()
            .take_while(|(s, _)| s.machine_addr.addr >= floor)
            .filter(|(_, id)| {
                g.node_weight(*id)
                    .expect("node_indices() only yields present nodes")
                    .contains_addr(target)
            })
            .map(|(_, id)| id.index() as u32)
            .min()
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    Ok(())
}
