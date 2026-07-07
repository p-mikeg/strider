//! `PyCfg` — wraps `strider_cfg::Cfg` and exposes dot rendering plus the
//! p-code lookup surface (`pcode_at` / `fingerprint_pcode`).
//!
//! A `Cfg` is built by `Lifter.build_cfg`/`Lifter.analyze`, which borrows
//! the `Lifter`'s owned `Sleigh` mutably for the duration of the build.
//! The `Cfg` is a pure data structure and does not own the Sleigh; `PyCfg`
//! keeps a shared `Py<PyLifter>` handle (the `Lifter` that built it) and
//! borrows the owned Sleigh from it on demand for dot rendering and
//! register-name resolution.
//!
//! `Cfg::regions()` already holds every decoded p-code op
//! (`strider_cfg::RegionInstruction { addr: PcodeInsnAddr, insn:
//! rsleigh::Insn }`), decoded in the exact context the real lift used
//! (sequential-within-region, from entry).  So `pcode_at` /
//! `fingerprint_pcode` are LOOKUPS against those stored decodes, not a
//! fresh re-decode — correct even on context-dependent architectures
//! (ARM/Thumb, MIPS16) where a fresh default-context Sleigh would render
//! the wrong p-code for a mid-function mode switch.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use pyo3::prelude::*;

use crate::dot::dot_style_for;
use crate::errors::into_strider_err;
use crate::node::PyNode;
use crate::reader::AnyMemReader;
use crate::strider_cls::PyLifter;

/// Control-flow graph of a single function, produced by `Lifter.build_cfg`
/// / returned as element 0 of `Lifter.analyze`.  Renderable to Graphviz
/// dot / dark-themed HTML for inspection; also the p-code audit-trail
/// lookup (`pcode_at` / `fingerprint_pcode`).
#[pyclass(name = "Cfg", module = "strider", unsendable)]
pub struct PyCfg {
    pub(crate) inner: strider_cfg::Cfg,
    /// Shared handle to the `Lifter` that built `inner`.  The `Cfg` is a
    /// pure data structure and does not own the Sleigh; the `Lifter` owns
    /// it.  Dot rendering and the IR lift (`Lifter.analyze_cfg`) borrow
    /// the Sleigh through this handle to resolve register names.
    pub(crate) lifter: Py<PyLifter>,
    /// Lazily-built `machine_addr -> joined p-code text` lookup, built
    /// once from `inner.regions()` on the first `pcode_at` /
    /// `fingerprint_pcode` call and reused thereafter.
    pcode_map: OnceLock<HashMap<u64, String>>,
}

impl PyCfg {
    /// Construct a `PyCfg` over an already-built `strider_cfg::Cfg` and
    /// its owning `Lifter` handle.  The single constructor keeps the
    /// `pcode_map` cache initialisation in one place.
    pub(crate) fn new(inner: strider_cfg::Cfg, lifter: Py<PyLifter>) -> Self {
        Self {
            inner,
            lifter,
            pcode_map: OnceLock::new(),
        }
    }

    /// Borrow the parent `Lifter` and run `f` with the `Lifter`'s owned
    /// `rsleigh::Sleigh`.
    fn with_sleigh<R>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&rsleigh::Sleigh<AnyMemReader>) -> PyResult<R>,
    ) -> PyResult<R> {
        let lifter_borrow = self.lifter.borrow(py);
        f(lifter_borrow.sleigh())
    }

    /// Borrow the parent `Lifter`'s owned `Sleigh`, build a `GraphDot` over
    /// this CFG at `style`, and dispatch to `op`.  Centralises the
    /// `with_sleigh` + `GraphDot::new` + terminal-render skeleton shared by
    /// `to_html` / `to_dot` / `html_str` (mirrors `PyLifter::dispatch_dot`).
    /// `style` is already resolved by each caller so their exact per-method
    /// defaults are preserved.
    fn dispatch_dot(
        &self,
        py: Python<'_>,
        style: &str,
        op: CfgDotOp<'_>,
    ) -> PyResult<CfgDotResult> {
        self.with_sleigh(py, |sleigh| {
            let d = dot::GraphDot::new(self.inner.dot_dumper(sleigh), dot_style_for(Some(style)));
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
            }
        })
    }

    /// The cached `machine_addr -> joined p-code text` map, built ONCE by
    /// a single pass over every region's `RegionInstruction`s.  Ops
    /// belonging to the same machine instruction (same `machine_addr`,
    /// distinguished by `insn_index`) are joined with `"; "` in
    /// `insn_index` order; a machine instruction that lifts to zero
    /// p-code ops (e.g. `endbr64`) still gets an entry, mapped to `""`.
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

/// Discriminator for [`PyCfg::dispatch_dot`], mirroring
/// [`crate::strider_cls`]'s `DotOp`.  Each variant carries the per-op
/// arguments the public `to_html` / `to_dot` / `html_str` accessors would
/// otherwise duplicate the sleigh-borrow / dumper-construction ritual for.
enum CfgDotOp<'a> {
    ToHtml(&'a str),
    ToDot(&'a str),
    HtmlStr,
}

/// Return shape of [`PyCfg::dispatch_dot`].  Returning a sum lets a single
/// helper cover both the unit-returning dump methods and the
/// string-returning `html_str` without separate variants per dispatch.
enum CfgDotResult {
    Unit,
    Html(String),
}

#[pymethods]
impl PyCfg {
    /// Render the CFG to a standalone HTML file at `path`.  `style`
    /// selects the dot theme (default `"dark_cfg"`).
    #[pyo3(signature = (path, style=None))]
    fn to_html(&self, py: Python<'_>, path: &str, style: Option<&str>) -> PyResult<()> {
        let style = style.unwrap_or("dark_cfg");
        self.dispatch_dot(py, style, CfgDotOp::ToHtml(path))
            .map(|_| ())
    }
    /// Render the CFG to a Graphviz `.dot` file at `path`.
    #[pyo3(signature = (path,))]
    fn to_dot(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        self.dispatch_dot(py, "dark_cfg", CfgDotOp::ToDot(path))
            .map(|_| ())
    }
    /// Return the CFG rendered as an HTML string (default `"dark_cfg"`
    /// style) instead of writing it to a file.
    #[pyo3(signature = (style=None))]
    fn html_str(&self, py: Python<'_>, style: Option<&str>) -> PyResult<String> {
        let style = style.unwrap_or("dark_cfg");
        match self.dispatch_dot(py, style, CfgDotOp::HtmlStr)? {
            CfgDotResult::Html(s) => Ok(s),
            CfgDotResult::Unit => Err(into_strider_err(anyhow::anyhow!(
                "internal: CfgDotOp::HtmlStr returned CfgDotResult::Unit"
            ))),
        }
    }

    /// Look up the lifted p-code for the machine instruction at `addr`.
    ///
    /// Returns the joined p-code op text — every `RegionInstruction`
    /// whose `machine_addr == addr` (a machine instruction lifts to one
    /// or more p-code ops, one `RegionInstruction` each), rendered via
    /// `rsleigh::Insn`'s `Display` impl and joined with `"; "` — or
    /// `None` when `addr` has no `RegionInstruction` in this CFG.
    ///
    /// **Known limitation:** a machine instruction that lifts to ZERO
    /// p-code ops (e.g. x86 `endbr64`, AArch64 `paciasp`) has no
    /// `RegionInstruction` at all — `strider_cfg::Region` only stores an
    /// entry per decoded p-code OP, so a zero-op machine instruction
    /// leaves no trace.  Such an address is therefore indistinguishable
    /// from one this CFG never decoded — both return `None` here.
    /// `Lifter.pcode_at`, which re-decodes rather than looking up, still
    /// returns `""` for it (see that method's doc).
    ///
    /// This is otherwise a LOOKUP against the CFG's own stored decodes
    /// (the exact lift-time context — correct even for context-dependent
    /// architectures like ARM/Thumb or MIPS16), never a fresh re-decode.
    /// The lookup table is built once (on first call) and cached.
    fn pcode_at(&self, addr: u64) -> Option<String> {
        self.pcode_map().get(&addr).cloned()
    }

    /// The asm-fingerprint of `node` as `(addr, text)` p-code pairs,
    /// sorted by address — the CFG-lookup companion to
    /// `Node.fingerprint()` (addr-only).  Each fingerprint address is
    /// resolved via `pcode_at`; an address `pcode_at` returns `None` for
    /// (not present in this CFG — e.g. `node` belongs to a different
    /// `Cfg`/`Function`) is SKIPPED rather than emitting an empty-text
    /// entry, so every returned pair is a genuine lookup hit.  In
    /// practice a fingerprint address always names a real p-code-
    /// producing instruction (a zero-op instruction like `endbr64` never
    /// contributes a node's value), so this skip is purely a
    /// cross-CFG-mismatch safeguard, not an expected case.
    ///
    /// `[]` for structural nodes with no fingerprint (Entry,
    /// InitialMemory, InitialVar, Region, phis).  This is the audit
    /// trail — correct by construction, since it reads the exact
    /// lift-time decodes stored in this CFG rather than re-decoding.
    fn fingerprint_pcode(
        &self,
        py: Python<'_>,
        node: PyRef<'_, PyNode>,
    ) -> PyResult<Vec<(u64, String)>> {
        let addrs = node.fingerprint(py)?;
        let map = self.pcode_map();
        let mut out: Vec<(u64, String)> = addrs
            .into_iter()
            .filter_map(|addr| map.get(&addr).cloned().map(|text| (addr, text)))
            .collect();
        out.sort_by_key(|(addr, _)| *addr);
        Ok(out)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    Ok(())
}
