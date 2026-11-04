//! `pcode_at` / `fingerprint_pcode` read the CFG's own decodes, never
//! re-decode, so a mid-function ISA-mode switch renders right.

use std::collections::HashMap;
use std::sync::OnceLock;

use pyo3::prelude::*;

use crate::dot::{DEFAULT_CFG_STYLE, dot_style_for, render};
use crate::errors::into_strider_err;
use crate::node::PyNode;
use crate::strider_cls::PyLifter;

/// Control-flow graph of one function, from `Lifter.build_cfg` or
/// `Lifter.analyze`.
#[pyclass(name = "Cfg", module = "strider.cfg", frozen)]
pub struct PyCfg {
    pub(crate) inner: strider_cfg::Cfg,
    pub(crate) lifter: Py<PyLifter>,
    /// Kept so a render never borrows `lifter`.
    pub(crate) arch: strider_target::SleighArch,
    /// `machine_addr -> joined p-code text`.
    pcode_map: OnceLock<HashMap<u64, String>>,
}

/// `incomplete` at machine-address granularity, sorted and deduplicated.
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

    /// `u32::MAX` is petgraph's end sentinel, never a region.
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
        let d = dot::GraphDot::new(
            self.inner.dot_dumper(self.arch.names()),
            dot_style_for(style.unwrap_or(DEFAULT_CFG_STYLE))?,
        );
        render(&d, html, path)
    }

    /// Ops joined with `"; "` in `insn_index` order.
    fn pcode_map(&self) -> &HashMap<u64, String> {
        self.pcode_map.get_or_init(|| {
            let mut grouped: HashMap<u64, Vec<(u64, &rsleigh::Insn)>> = HashMap::new();
            for region in self.inner.regions() {
                for ri in &region.insns {
                    grouped
                        .entry(ri.addr.machine_addr.addr)
                        .or_default()
                        .push((ri.addr.insn_index, &ri.insn));
                }
            }
            grouped
                .into_iter()
                .map(|(addr, mut ops)| {
                    ops.sort_by_key(|(idx, _)| *idx);
                    let text = crate::pcode::pcode_text(
                        ops.into_iter().map(|(_, insn)| insn),
                        self.inner.space_ids(),
                    );
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

    /// Sorted `(machine_addr, reason)` pairs this CFG cannot vouch for; empty
    /// is complete. Check it before trusting an absence ("no store to X").
    ///
    /// `"unresolved_branch"` and `"unverified_seed"` name a dispatch site; the
    /// other five a branch target:
    ///
    /// - `"unresolved_branch"`: an indirect branch with no complete successor
    ///   set, including a return not provably to the caller and not seeded
    ///   `"return"`.
    /// - `"unverified_seed"`: a seated answer nothing verified: a
    ///   `known_targets` seat, a consumed tail call, or a `"return"` seed.
    /// - `"isa_mode_conflict"`: reached in two ISA modes; the losing path
    ///   decodes in the other.
    /// - `"interior_target"`: off every instruction boundary, or a region
    ///   start a later decode stepped over.
    /// - `"unmapped_target"`: no byte of the image backs it.
    /// - `"undecodable_target"`: holds no instruction, or `data_ranges` marks
    ///   it as data.
    /// - `"past_function_end"`: code falls through past `function_max_size`
    ///   or a `fragment_ranges` end (a tail call to this address), or a call
    ///   not to a `no_return_targets` entry would return here.
    ///
    /// A seeded arm that falls through into an unmapped, undecodable or
    /// past-the-end target is dropped and named there, unless a call precedes
    /// those bytes (the call then does not return). A branch from an arm's
    /// code to an unmapped target leaves through a stub and keeps the arm.
    ///
    /// From `analyze`, target reasons accumulate over the resolver's rounds
    /// and site reasons are the final round's.
    #[getter]
    fn incomplete(&self) -> Vec<(u64, &'static str)> {
        incomplete_list(self.inner.incomplete())
    }

    /// Lets the cyclic GC see a cycle through `lifter` to its Python reader.
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.lifter)
    }

    /// Render to DOT: the string when `path` is `None`, else written to
    /// `path`. `style` is the theme (default `"dark_cfg"`).
    #[pyo3(signature = (path=None, style=None))]
    fn to_dot(&self, path: Option<&str>, style: Option<&str>) -> PyResult<Option<String>> {
        self.render_dot(style, false, path)
    }

    /// Render to a standalone HTML page: the string when `path` is `None`,
    /// else written to `path`. `style` is the theme (default `"dark_cfg"`).
    #[pyo3(signature = (path=None, style=None))]
    fn to_html(&self, path: Option<&str>, style: Option<&str>) -> PyResult<Option<String>> {
        self.render_dot(style, true, path)
    }

    /// The p-code ops this CFG decoded at `addr`, joined with `"; "`, or
    /// `None`. An instruction lifting to no ops (x86 `endbr64`) also gives
    /// `None`; `Lifter.pcode_at` returns `""` for it.
    fn pcode_at(&self, addr: u64) -> Option<String> {
        self.pcode_map().get(&addr).cloned()
    }

    /// `node`'s asm fingerprint as sorted `(addr, pcode)` pairs, skipping an
    /// address this CFG has no decode for; `[]` for a structural node. Raises
    /// `StriderError` for a `node` of another `Cfg`'s function.
    fn fingerprint_pcode(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        node: PyRef<'_, PyNode>,
    ) -> PyResult<Vec<(u64, String)>> {
        if !node.at.function.borrow(py).cfg.bind(py).is(slf) {
            return Err(into_strider_err(anyhow::anyhow!(
                "this Node belongs to a function another Cfg lifted; a \
                 fingerprint is machine addresses, which collide across images, \
                 so the two must be the same Cfg"
            )));
        }
        let slf = slf.get();
        let addrs = node.asm_fingerprint(py)?;
        let map = slf.pcode_map();
        Ok(addrs
            .into_iter()
            .filter_map(|addr| map.get(&addr).cloned().map(|text| (addr, text)))
            .collect())
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

    /// DOT of the regions within `depth` edges of region `center`, capped at
    /// `max_nodes`.
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

    /// The region holding `addr`; of overlapping ones, the latest start.
    fn region_at(&self, addr: u64) -> Option<u32> {
        self.inner
            .region_containing(strider_cfg::PcodeInsnAddr::at_machine_start(addr))
            .map(|id| id.index() as u32)
    }
}

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    m.add(
        "DOT_STYLES",
        pyo3::types::PyTuple::new_bound(py, crate::dot::DOT_STYLES.map(|(name, _)| name)),
    )?;
    m.add(
        "INCOMPLETE_REASONS",
        pyo3::types::PyTuple::new_bound(
            py,
            strider_cfg::Reason::ALL.map(strider_cfg::Reason::name),
        ),
    )?;
    Ok(())
}
