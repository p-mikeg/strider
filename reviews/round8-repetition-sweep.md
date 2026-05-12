# Round 8 — Repetition / Utility-Consolidation Sweep

**Branch:** `review/ai2` at `/mnt/c/Users/mikeg/Documents/strider`.
**Status:** Proposals only — no source files modified.
**Cross-reference:** Builds on `reviews/round8-simplifications.md` §2 (six
items already covered there are not re-listed; finding #1 below extends the
scope of round8-2.1 beyond the 50-site count it gave).

This document collects 24 concrete consolidation proposals identified by a
focused `Grep`/`Read` sweep across `crates/**/src/**/*.rs` and
`crates/**/tests/**/*.rs`.  Items already in
`round8-simplifications.md` §2 (poison-mutex, `analyze_known_bits` arms,
InitialVar/region-index walks, test-fixture promotion to `strider::test_utils`,
`find_all_unique_vns`, `compact()` `HashMap`) are not duplicated.

Severity legend (per task brief):
- HIGH = ≥4 occurrences, ≥4 LOC each
- MED  = 3 occurrences, ≥3 LOC each
- LOW  = 2 occurrences but worth normalising

---

## #1 (HIGH): Reachable-set HashSet pattern duplicated 14× in opt tests

**Pattern (≥3 LOC, 14 sites):**
```rust
let reachable: std::collections::HashSet<_> = fg.preorder().collect();
let n = fg.all_node_ids()
    .filter(|n| reachable.contains(n))
    .filter(|&n| matches!(fg.node_kind(n), NodeKind::X(...)))
    .count();
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/constant_fold/tests.rs:664-665`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/dead_branch/tests.rs:160-162`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/function_args/tests.rs:57-62, 120-125, 203-208, 386-389`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/load_readonly/tests.rs:150-152`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/redundant_phis/tests.rs:61-64, 99-103`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_load_forward/tests.rs:26-29, 398-401`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_store/tests.rs:48-53`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/tests/multi_pass.rs:47-49, 259-262, 310-313`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/tests/pipeline_default.rs:59-61`

**Proposed helper:** `pub fn count_reachable<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize`
(crate `opt`, in `tests/common/mod.rs`).  This already exists at
`/mnt/c/Users/mikeg/Documents/strider/crates/opt/tests/common/mod.rs:46-55`!
The helper is in `tests/common/` only — the 14 white-box sites in
`src/<pass>/tests.rs` cannot import from `tests/common/`.

**Migration:** Promote `count_reachable` (and `count`, `return_value`,
`return_kind`) to a test-utils gate in the `opt` crate's `src/test_utils.rs`
so both white-box (`src/<pass>/tests.rs`) and black-box (`tests/<file>.rs`)
suites import a single source.  The white-box `tests` modules switch from
`use std::collections::HashSet` + open-coded loop to
`use opt::test_utils::count_reachable`.  Mechanical.

**Effort:** S.

---

## #2 (HIGH): Six per-arch `__scan_ignore_<arch>!` macros are 16-line clones

**Pattern (16 LOC × 16 archs ≈ 256 lines):**
```rust
#[doc(hidden)]
#[macro_export]
macro_rules! __scan_ignore_<arch> {
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { <Arch>: $reason:literal $(, $($_rest:tt)*)? }) => {
        #[test] #[ignore = $reason]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::<Arch>, $case, $fn_name);
            $assert(&g);
        }
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
     { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
        $crate::__scan_ignore_<arch>!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
    };
    ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
        #[test]
        fn $fn() {
            let g = $crate::common::analyze($crate::common::Arch::<Arch>, $case, $fn_name);
            $assert(&g);
        }
    };
}
```

**Sites:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/mod.rs:471-871`
(16 macros, lines 471, 499, 523, 547, 571, 595, 619, 653, 677, 701, 725, 749, 773, 797, 825, 851).

**Proposed helper:** Either:
1. **Token-paste based generator** — one macro `__define_scan_ignore!(<arch>, Arch::<Arch>)`
   that uses `paste::paste!` to synthesise the `__scan_ignore_<arch>` macro.
2. Replace the per-arch ignore-block matching with **runtime dispatch**: collect
   ignore reasons into a `&[(arch_name, reason)]` slice and look up at test time.
   The latter eliminates the macro recursion entirely and reduces code by ~250
   LOC.  The author's own comment (line 641-649) acknowledges "the per-arch pattern
   is mechanical, we keep it explicit per arch".

**Migration:** Judgement.  Approach 2 trades macro complexity for a small
runtime cost, but the runtime cost is irrelevant for ignored tests (they don't
run).  Recommended: option 2.  ~16 macros (~256 LOC) collapse into one
`per_arch_test!` macro that emits 16 `#[test]` functions each consulting a
shared `is_ignored(arch, ignore_list)` runtime helper.

**Effort:** M.

---

## #3 (HIGH): `let cap = key.resolve()?; let g = self.graph.borrow(py); let g = g.read_inner().map_err(into_strider_err)?; Ok(...)` repeated 11+ times in matcher.rs

**Pattern (4 LOC × 11 sites = 44 lines):**
```rust
fn <accessor>(&self, py: Python<'_>, key: CaptureKey<'_>) -> PyResult<Option<X>> {
    let cap = key.resolve()?;
    let g = self.graph.borrow(py);
    let g = g.read_inner().map_err(into_strider_err)?;
    Ok(self.inner.<inner_accessor>(cap, &g)<.map(...)?>)
}
```

**Sites:** all 11 typed accessors on `PyMatch`:
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:72-77` (`uint`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:80-85` (`int_`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:88-93` (`bool_`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:95-100` (`float_bits`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:119-124` (`int_binary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:127-132` (`int_unary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:136-141` (`int_cmp_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:144-149` (`bool_binary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:152-157` (`bool_unary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:160-165` (`float_binary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:168-173` (`float_unary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:176-181` (`float_cmp_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:187-192` (`vn`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:202-...` (`stack_offset`)

**Proposed helper:** Private inherent method on `PyMatch`:
```rust
impl PyMatch {
    fn with_graph<F, R>(&self, py: Python<'_>, key: CaptureKey<'_>, f: F) -> PyResult<R>
    where F: FnOnce(pattern::Capture, &ir::Graph) -> R {
        let cap = key.resolve()?;
        let g = self.graph.borrow(py);
        let g = g.read_inner().map_err(into_strider_err)?;
        Ok(f(cap, &g))
    }
}
```
Each accessor becomes a one-liner: `fn uint(...) -> PyResult<Option<u128>> { self.with_graph(py, key, |c, g| self.inner.get_uint(c, g)) }`.

**Migration:** Mechanical.  ~44 LOC of repetition reduces to ~14 LOC.

**Effort:** S.

---

## #4 (HIGH): 16-method `capture/cap/when/into_pat` block duplicated 14× in PyO3 pattern builders

**Pattern (8 LOC × 14 builders = 112 lines):**
```rust
fn capture(&self, c: PyRef<'_, PyCapture>) -> PyPat {
    use pattern::IntoPat;
    PyPat::from_pat(self.finalise().capture(c.inner))
}
fn cap(&self, name: &str) -> PyResult<PyPat> {
    use pattern::IntoPat;
    let c = intern_str(name)?;
    Ok(PyPat::from_pat(self.finalise().capture(c)))
}
fn when(&self, f: PyObject) -> PyPat { PyPat::from_pat(wrap_when(self.finalise(), f)) }
fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
```

**Sites:** every typed Python pattern builder repeats this verbatim:
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:620-630` (`PyPhiPat`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:673-683` (`PyMemPhiPat`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:717-727` (`PyValuePhiPat`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:777-787`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:1126-1136`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:1220-1230`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:1290-1300`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:1352-1362`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:1462-...`
- 16 occurrences of `fn into_pat` in `pattern.rs` (verified via `grep -c`).

**Proposed helper:** A `macro_rules!` macro `impl_pyo3_pat_builder_finalise!(BuilderTy)` that
expands to the four-method block.  Or, simpler: define a private trait
`PyPatBuilder { fn finalise(&self) -> pattern::Pat; }` and put a default
inherent impl block via a `pyo3` re-usable wrapper.  `pyo3` doesn't allow
trait-dispatched `#[pymethods]`, so the macro is the cleaner route.

**Migration:** Mechanical.  Each site becomes
`impl_pyo3_pat_builder_finalise!(PyPhiPat);`.  ~112 LOC reduces to ~14 lines.

**Effort:** S-M.

---

## #5 (HIGH): `Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi())` boilerplate in 18+ test files

**Pattern (3 LOC × 18 sites = 54 lines):**
```rust
let arch = SleighArch::x86_64();
let regs = arch.probe_regs().expect("probe regs");
let strider = Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi())
    .expect("Strider::new");
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/analyze_cfg_with_overrides.rs:25-27, 42-44`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/bug_on_lifts_cleanly.rs:19-21, 48-50`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/compact.rs:23-25`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/flag_cmp_canonicalize_e2e.rs:33-35`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/graph_rewriter.rs:83-85`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/jump_table_lifting.rs:86-88, 231-233`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/per_address_cc.rs:28-30`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/per_address_cc_indirect.rs:28-30`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/r1_placeholder.rs:67-69, 110-112`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/orchestrator_indirect_branch.rs:30-31`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/indirect_resolve_helpers/orchestrator.rs:69-71`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/indirect_resolve_helpers/classify.rs:830-832`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:919-920`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:576-577`

**Proposed helper:** This expands round8-simplifications §2.4 with measured
breadth.  In `crates/strider/src/test_utils.rs` (gated `feature = "test-utils"`):
```rust
pub fn strider_x86_64() -> Strider { /* + any other arches */ }
pub fn strider_aarch64() -> Strider { ... }
pub fn strider_arm() -> Strider { ... }
pub fn strider_for(arch: SleighArch, cc: CallingConvention) -> Strider {
    let regs = arch.probe_regs().expect("probe regs");
    Strider::new(arch, regs, cc).expect("Strider::new")
}
```
Test files at `crates/strider/tests/common/mod.rs:140` already define
`strider_for(Arch)` — they would re-export from `strider::test_utils` instead.

**Migration:** Mechanical at 14 sites; the two `src/` sites
(`orchestrator.rs:919`, `pipeline.rs:576`) are gated by `#[cfg(test)]`.

**Effort:** S.

---

## #6 (HIGH): `let sleigh_borrow = sleigh.borrow(py); let regs = sleigh_borrow.regs.clone(); drop(sleigh_borrow); let built_cc = cc.inner.build(&regs).map_err(into_lift_err)?` repeated 6× in strider-py

**Pattern (6 LOC × 6 sites = 36 lines):**
```rust
let sleigh_borrow = sleigh.borrow(py);
let regs = sleigh_borrow.regs.clone();
drop(sleigh_borrow);
let built_cc = cc
    .inner
    .build(&regs)
    .map_err(crate::errors::into_lift_err)?;
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/opt.rs:327-333` (`PyStackStoreDetect`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/opt.rs:354-360` (`PyStackLoadForward`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/opt.rs:380-386` (`PyFunctionArgDetect`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/opt.rs:406-412` (`PyCallStackArgCollect`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/strider_cls.rs:52-58`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/strider_cls.rs:117-123`

**Proposed helper:** In `crates/strider-py/src/cc.rs`:
```rust
pub(crate) fn build_cc_for_sleigh(
    py: Python<'_>,
    sleigh: &Py<crate::sleigh::PySleigh>,
    cc: &crate::cc::PyCallingConvention,
) -> PyResult<target::BuiltCallingConvention> {
    let sleigh_borrow = sleigh.borrow(py);
    let regs = sleigh_borrow.regs.clone();
    drop(sleigh_borrow);
    cc.inner.build(&regs).map_err(crate::errors::into_lift_err)
}
```

**Migration:** Mechanical.  Each `#[new]` constructor shrinks by 5 LOC.

**Effort:** XS.

---

## #7 (HIGH): Six 5-line zero-sized "pure pass" wrapper structs in strider-py

**Pattern (5 LOC × 6 structs = 30 lines):**
```rust
#[pyclass(name = "<Name>", module = "strider.opt")]
pub struct Py<Name>;
#[pymethods]
impl Py<Name> {
    #[new]
    fn new() -> Self { Self }
}
```

**Sites:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/opt.rs:260-306`
(`PyConstantFold`, `PyKnownBits`, `PyRedundantPhis`, `PyDeadBranchElim`,
`PyFlagCmpCanonicalize`, `PyIfCondInversion`).

**Proposed helper:** A `macro_rules!` macro:
```rust
macro_rules! pure_pass_class {
    ($pyname:literal => $rust:ident) => {
        #[pyclass(name = $pyname, module = "strider.opt")]
        pub struct $rust;
        #[pymethods]
        impl $rust {
            #[new]
            fn new() -> Self { Self }
        }
    };
}
pure_pass_class!("ConstantFold" => PyConstantFold);
// ...
```

**Migration:** Mechanical.  ~30 LOC reduces to ~6 macro invocations.

**Effort:** XS.

---

## #8 (HIGH): `addr.machine_addr.addr` chained accessor pattern repeated 19 times

**Pattern:** `<expr>.machine_addr.addr` to extract a `u64` out of `PcodeInsnAddr`:
```rust
let machine_addr = addr.machine_addr.addr;
```

**Sites (in production code only):**
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/region_builder.rs:223,288,344,554,627,668`
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/dot.rs:65,78`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:386`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/insn/mod.rs:45`
- Plus 9 sites in tests.

**Proposed helper:** Add accessors on `PcodeInsnAddr`:
```rust
impl PcodeInsnAddr {
    pub fn machine_addr_u64(&self) -> u64 { self.machine_addr.addr }
}
```
This dovetails with `round8-simplifications.md` §3.6 (HIGH) which proposes
making `PcodeInsnAddr.addr` and `MachineInsnAddr.addr` `pub(crate)`.  Once
the field is hidden, every reader needs an accessor — the natural shape is
exactly `machine_addr_u64()`.

**Migration:** Mechanical.  ~19 sites switch from
`x.machine_addr.addr` to `x.machine_addr_u64()`.

**Effort:** XS.

---

## #9 (HIGH): "Find unique IndirectBranch placeholder" loop repeated 10+ times

**Pattern (5-7 LOC × 10+ sites):**
```rust
let mut found: Option<NodeId> = None;
for nid in fg.preorder() {
    if matches!(fg.graph.node_kind(nid), NodeKind::IndirectBranch) {
        assert!(found.is_none(), "more than one IndirectBranch");
        found = Some(nid);
    }
}
let placeholder = found.expect("no IndirectBranch placeholder");
```
or the variant returning the captured `target_value` from input slot 2.

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/r1_placeholder.rs:79-86, 90-94`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/indirect_branch.rs:144-150`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/indirect_resolve_in_place_edits.rs:26-31`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/orchestrator_indirect_branch.rs:75-78`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/jump_table_lifting.rs:289-291`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/indirect_resolve_helpers/classify.rs:328-336, 408-416`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:204-210` (test)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:742, 820, 865, 918, 1100, 1188`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs:415-417, 461-...` (production)

**Proposed helper:** Either on `BuiltFunctionGraph` (production-fit):
```rust
pub fn unique_indirect_branch(&self) -> Option<NodeId> {
    let mut found = None;
    for nid in self.preorder() {
        if matches!(self.node_kind(nid), NodeKind::IndirectBranch) {
            if found.is_some() { return None; }
            found = Some(nid);
        }
    }
    found
}
pub fn unique_indirect_branch_anchor(&self) -> Option<ir::Value> {
    self.unique_indirect_branch().and_then(|nid| self.node_inputs(nid).get(2).copied())
}
```
or, more general, a `find_unique_node_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> Option<NodeId>`.

**Migration:** Mechanical at the 10+ sites.

**Effort:** S.

---

## #10 (HIGH): 8 near-identical `get_<op_family>_op` extractors in pattern bindings

**Pattern (7 LOC × 8 sites = 56 lines):**
```rust
pub fn get_int_binary_op(&self, c: Capture, graph: &Graph) -> Option<IntBinaryOp> {
    let node = self.get_node(c)?;
    match graph.node_kind(node) {
        NodeKind::IntBinaryOp(op) => Some(*op),
        _ => None,
    }
}
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:188-198` (`get_int_binary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:202-212` (`get_int_unary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:216-222` (`get_int_cmp_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:226-236` (`get_bool_binary_op`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:240-...` (`get_bool_unary_op`)
- + `get_float_binary_op`, `get_float_unary_op`, `get_float_cmp_op` (8 total).

**Proposed helper:** A private generic helper that takes a closure mapping
`&NodeKind → Option<Op>`:
```rust
fn get_op_with<O>(&self, c: Capture, graph: &Graph, extract: impl FnOnce(&NodeKind) -> Option<O>) -> Option<O> {
    extract(graph.node_kind(self.get_node(c)?))
}
```
or, more directly, use a small macro `impl_get_op!(get_int_binary_op, IntBinaryOp, IntBinaryOp);`.

**Migration:** Mechanical.  Each accessor shrinks to a 1-line macro
invocation or a 2-line helper call.  ~56 LOC reduces to ~16 LOC.

**Effort:** XS.

---

## #11 (HIGH): 4 Layer-C validators repeat the "for node in graph.nodes.keys() { if !reachable.contains(node) { continue; }" reachable-scope skeleton

**Pattern (4 LOC × 4 sites = 16 lines, plus per-site filter):**
```rust
for node in graph.nodes.keys() {
    if !reachable.contains(node) {
        continue;
    }
    // per-validator predicate + push errors
}
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_c.rs:201-208` (`check_layer_c_asm_fingerprints`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_c.rs:261-268` (`check_layer_c_wide_consts`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_c.rs:60-77` (`check_layer_c_control_state` — same shape with `reachable.contains` gating only the `is_empty()` branch)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_c.rs:102-...` (`check_layer_c_phis`)

**Proposed helper:** In `validate/layer_c.rs`:
```rust
fn for_each_reachable_node<F: FnMut(NodeId, &NodeKind)>(
    graph: &Graph, reachable: &NodeIdSet, mut f: F,
) {
    for node in graph.nodes.keys() {
        if !reachable.contains(node) { continue; }
        f(node, graph.node_kind(node));
    }
}
```
This dovetails with `round8-simplifications.md` §6.3 (perf migration) which
proposes adding the `reachable: &NodeIdSet` parameter to all Layer-C
helpers — once that lands, the 4-site skeleton becomes a single helper.

**Migration:** Mechanical, but co-ordinate with §6.3 (`reachable`-scope
parameter addition) so both improvements land together.

**Effort:** S.

---

## #12 (HIGH): 9× `known.get(&x).copied().unwrap_or_default()` in `KnownBits` could be a tiny helper

**Pattern (1-line, 9 sites — same shape):**
```rust
let kb = known.get(&input).copied().unwrap_or_default();
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/mod.rs:137,138,172,213,249,259,272,289`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/jump_table.rs:313`

**Proposed helper:** This is best resolved by `round8-simplifications.md` §6.2
(replacing `FxHashMap<NodeOutputId, Kb>` with `SecondaryMap<NodeOutputId, Kb>`),
which makes the call site `known[input]` directly with no `unwrap_or_default`.
Listed here for completeness as a downstream effect of the perf migration.

**Migration:** Subsumed by §6.2.

**Effort:** Already accounted for in §6.2.

---

## #13 (MED): IR builder boilerplate `b.create_region(); b.set_entry_region(r); b.set_region(r)` repeated 30+ times in tests

**Pattern (3-4 LOC × 30+ sites):**
```rust
let mut b = FunctionBuilder::empty().unwrap();
let r = b.create_region().unwrap();
b.set_entry_region(r).unwrap();
b.set_region(r);
```

**Sites:** widely repeated across the workspace; representative samples:
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/build_validate_roundtrip.rs:41-43, 70-73, 94-97, 116-119, 157-160, 179-182, 205-208, 225-228`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/builder_extended_use.rs:27-30, 65-68`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/call_other_modeled.rs:11-...`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/call_other_classification.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/build_call_with_cc.rs:62-64`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/walk_reachability.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/proptest_graph_invariants.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/dead_branch/tests.rs:21-27, 95-99` (and many similar)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/sp_expr.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/pipeline.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/flag_cmp_canonicalize/tests.rs`

**Proposed helper:** `ir::test_utils::make_empty_fn(|b| -> Result<Value> { ... })`
already exists at `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/test_utils.rs:15-26`.
The 30+ open-coded repetitions show poor adoption.  Some sites need
multiple regions (e.g. `make_if_fn` in `dead_branch/tests.rs:20`); for them
a `make_fn_with_n_regions(n, |b, regions| -> Result<()>)` would generalise.

**Migration:** Mostly mechanical, but each site needs a closure rewrite —
some tests build multiple regions and the existing `make_empty_fn` doesn't
cover that.  Adding `make_fn_with_regions` and `make_if_branch_fn` to
`ir::test_utils` would cover ~80% of the repetitions.

**Effort:** M.

---

## #14 (MED): `rsleigh::Vn { addr_off: ..., addr_space: VnSpace::REGISTER, size: ... }` literals across 31 files

**Pattern (5 LOC each):**
```rust
let v = rsleigh::Vn {
    addr_off: 0x40,
    addr_space: rsleigh::VnSpace::REGISTER,
    size: 4,
};
```

**Sites:** 31 files contain such literals (verified via `grep -rcl`).
The `ir::test_utils::reg_vn(off, size)` helper already exists at
`/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/test_utils.rs:55-61`
and is used in some places — but many tests still inline the struct
literal.  Representative offenders:
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/tests.rs:1276-1280, 1292-1296, 1297-1301, 1314-1318, 1319-1323, 1339-1343, 1344-1348, 1360-1364, 1365-1369, 1370-1374, 1391-...` (12+ literals)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node_signature.rs:662-666`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/tests.rs:1002-1005` (already factored as `unique_vn` helper but only used locally)
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/tests/region_terminator.rs:215-219, 249-253, 286-...` (3 sites)
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/tests/indirect_dispatch.rs:270-274`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/indirect_resolve_helpers/classify.rs:365-369, 370-374` (multiple)

**Proposed helper:** Promote `ir::test_utils::reg_vn` to a workspace-wide
helper and drop the open-coded literals.  Add a sibling
`ir::test_utils::unique_vn(off, size)` (currently only inline at
`builder/tests.rs:1002`).

**Migration:** Mostly mechanical.  Tests already importing `reg_vn` show
the pattern; ~50 LOC saved at 12-15 inline literals in
`ir/src/builder/tests.rs` alone.

**Effort:** S.

---

## #15 (MED): `let regs = arch.probe_regs().unwrap()` / `expect("probe regs")` in 16+ test sites

**Pattern (1 LOC × 16 sites):**
```rust
let regs = arch.probe_regs().unwrap();
```

**Sites:** see #5 above.

**Proposed helper:** A trait extension or test util:
```rust
pub fn probe_or_panic(arch: &SleighArch) -> rsleigh::SleighRegs {
    arch.probe_regs().expect("probe regs")
}
```
or fold into the `strider_for(arch, cc)` helper proposed in #5.

**Migration:** Subsumed by #5.

**Effort:** Subsumed.

---

## #16 (MED): `Sleigh::new(arch.sla_spec, arch.pspec, reader)` repeated 12× verbatim

**Pattern (1 LOC, but with the `expect("sleigh")` rest):**
```rust
let sleigh = Sleigh::new(arch.sla_spec, arch.pspec, reader).expect("sleigh");
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/tests/known_targets.rs:28,65,101,134,142,189` (6 sites)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/benches/scaling.rs:66,72`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/examples/strider.rs:18`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/examples/dump_arch_cmps.rs:325`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:584`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/call_other_precise_abi.rs:12,27,85,101`

**Proposed helper:** Either an inherent constructor on `SleighArch`:
```rust
impl SleighArch {
    pub fn make_sleigh<M: rsleigh::MemReader>(self, reader: M) -> Result<rsleigh::Sleigh<M>> {
        rsleigh::Sleigh::new(self.sla_spec, self.pspec, reader)
    }
}
```
or a thin function `pub fn sleigh_for(arch: &SleighArch, reader: M) -> Result<...>` in
`strider::test_utils`.

**Migration:** Mechanical.  Pattern `Sleigh::new(arch.sla_spec, arch.pspec, reader)`
becomes `arch.make_sleigh(reader)` everywhere.  Dovetails with
`round8-simplifications.md` §3.7 (`SleighArch` fields `pub(crate)`).

**Effort:** XS.

---

## #17 (MED): RunConfig literal-construction with all default-y fields repeated 9 times

**Pattern (8 LOC × 9 sites = 72 lines):**
```rust
let config = RunConfig {
    strider: &strider,
    start_addr: entry,
    sleigh,
    rom: None,
    fn_max_size: None,
    allow_code_before_start_addr: false,
    compact: true,
    per_address_ccs: HashMap::new(),
};
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/bounded_lift_tail_call.rs:59,160,214,257`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/compact.rs:34`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/indirect_resolve_in_place_edits.rs:188`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/orchestrator_indirect_resolution.rs:29` (already in a `make_config` local helper)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/orchestrator_indirect_branch.rs:49`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/per_address_cc_indirect.rs:44`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/per_address_cc.rs:44,99`

**Proposed helper:** A `RunConfigBuilder` (cross-references
`round8-simplifications.md` §1.4 / §6 type-tightening proposals):
```rust
impl<'a, R: rsleigh::MemReader> RunConfig<'a, R> {
    pub fn builder(strider: &'a Strider, start_addr: u64, sleigh: rsleigh::Sleigh<R>) -> RunConfigBuilder<...> { ... }
}
```
Each site collapses to:
```rust
let config = RunConfig::builder(&strider, entry, sleigh).build();
```

**Migration:** Judgement (introducing builder API).  The local
`make_config` helper at `orchestrator_indirect_resolution.rs:24-39`
already shows the pattern is needed.

**Effort:** M.

---

## #18 (MED): The `process_insn` lift-attribution wrapper sets/restores `lift_addr` 4 times

**Pattern (3 LOC × ~4 sites):**
```rust
ir_strider.builder.set_lift_addr(Some(addr.machine_addr.addr));
let res = ir_strider.process_insn(...);
ir_strider.builder.set_lift_addr(None);
```

**Sites (excerpt):**
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:371-377` (insn loop)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:387-388` (terminator handler)
- The `set_lift_addr` calls scattered through `strider/insn/control.rs`

**Proposed helper:** RAII guard or scope-fn:
```rust
fn with_lift_addr<F, R>(builder: &mut FunctionBuilder, addr: u64, f: F) -> R
where F: FnOnce(&mut FunctionBuilder) -> R {
    builder.set_lift_addr(Some(addr));
    let r = f(builder);
    builder.set_lift_addr(None);
    r
}
```

**Migration:** Mechanical, ~3 LOC → 1 LOC at each site.

**Effort:** XS.

---

## #19 (MED): `fn count<F: Fn(&NodeKind) -> bool>(fg, pred) -> usize` re-implemented 4× in opt tests

**Pattern (4 LOC × 4 copies):**
```rust
fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.node_kind(n)))
        .count()
}
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/function_args/tests.rs:13-17`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_store/tests.rs:12-16`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/tests/common/mod.rs:39-43` (the canonical version)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/dead_branch/tests.rs:10-17` (variant `count_cs_with_n_inputs` — same shape)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/mod.rs:236-238` (canonical for system tests).

**Proposed helper:** Promote `count` (and `count_reachable` from #1) into
`opt::test_utils` so white-box `src/<pass>/tests.rs` can use the same
helper as black-box `tests/<file>.rs`.  See #1 — same migration motivates
both.

**Migration:** Subsumed by #1 (same migration plan).

**Effort:** Subsumed by #1.

---

## #20 (MED): `let [out] = graph.node_outputs_exact::<1>(n).expect("...")` repeated in IR-build helpers

**Pattern (3 LOC × 5+ sites):**
```rust
let n = graph.create_node(NodeKind::X(...), [...inputs], [NodeOutputKind::OutputType(ty)]);
graph.extend_asm_fingerprint_from(n, root);
#[allow(clippy::expect_used)]
let [out] = graph.node_outputs_exact::<1>(n).expect("X produces 1 output");
out
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/flag_cmp_canonicalize/mod.rs:149-162` (`build_int_cmp`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/flag_cmp_canonicalize/mod.rs:165-176` (`build_bool_neg`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_load_forward/mod.rs:360-376` (truncate/shr build)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:140-176` (int_const + Call + Return triplet)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/stack_array.rs:475-490, 674-692`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/function_args/mod.rs:190, 319, 339`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_store/detect.rs:38-, 59-`

**Proposed helper:** A `Graph::create_value_node` that combines the three
(create + fingerprint + extract single output):
```rust
impl Graph {
    pub fn create_single_output_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        out_kind: NodeOutputKind,
        fingerprint_from: NodeId,
    ) -> Result<(NodeId, NodeOutputId)> {
        let nid = self.create_node(kind, inputs, [out_kind]);
        self.extend_asm_fingerprint_from(nid, fingerprint_from);
        let [out] = self.node_outputs_exact::<1>(nid)?;
        Ok((nid, out))
    }
}
```

**Migration:** Judgement — passes that need to attribute to a different
fingerprint-source, or that emit nodes with extra fingerprint contributors,
need a slightly different signature.  But the most common case (5+ sites)
is mechanical.

**Effort:** S.

---

## #21 (MED): `for nid in fg.preorder() { if !matches!(fg.graph.node_kind(nid), <K>) { continue; }` skeleton repeated 8+ times

**Pattern (3 LOC × 8 sites):**
```rust
for nid in fg.preorder() {
    if !matches!(fg.graph.node_kind(nid), NodeKind::X(...)) {
        continue;
    }
    // per-site logic
}
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:742-744, 820-822, 865-867, 918-920, 1100-1102, 1188-1190` (6 sites)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:204-208` (test fixture)
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/indirect_resolve_in_place_edits.rs:26-29, 94-97`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/orchestrator_indirect_resolution.rs:50-53, 127-130`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/indirect_resolve_helpers/classify.rs:328-331, 408-411`

**Proposed helper:** Iterator-extension: `preorder_filter_kind(pred)`:
```rust
impl BuiltFunctionGraph {
    pub fn preorder_with_kind<'a, F>(&'a self, pred: F) -> impl Iterator<Item = (NodeId, &'a NodeKind)>
    where F: 'a + Fn(&NodeKind) -> bool {
        self.preorder().filter_map(move |nid| {
            let k = self.graph.node_kind(nid);
            if pred(k) { Some((nid, k)) } else { None }
        })
    }
}
```
Sites become `for (nid, _) in fg.preorder_with_kind(|k| matches!(k, NodeKind::IndirectBranch)) { ... }`.

**Migration:** Mechanical at all 8 sites.

**Effort:** XS-S.

---

## #22 (MED): `fg.node_outputs(node).into_iter().collect()` / `fg.node_inputs(node).into_iter().collect()` boilerplate (15+ sites)

**Pattern (1 LOC × 15+ sites):**
```rust
let outs: Vec<_> = g.node_outputs(call_node).into_iter().collect();
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/benches/validate.rs:63`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/function.rs:219`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/call.rs:139, 330`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/tests.rs:949`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/tests.rs:16`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_a.rs:20`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_c.rs:129`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/tests.rs:606`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/build_call_with_cc.rs:92`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/retain_reachable.rs:35`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/call_other_modeled.rs:85, 86`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/dead_branch/tests.rs:199, 215`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:167`

**Proposed helper:** Add `node_inputs_vec(node) -> Vec<NodeOutputId>` and
`node_outputs_vec(node) -> Vec<NodeOutputId>` directly on `Graph` to make
the intent obvious and avoid the `.into_iter().collect()` clutter.

**Migration:** Mechanical.  ~15 sites × 1 LOC saved.

**Effort:** XS.

---

## #23 (MED): Side-table accessor pair pattern duplicated 4 times in `graph/store.rs`

**Pattern:** Each per-side-table accessor pair has the same shape:
```rust
pub fn <table>(&self, node_id: NodeId) -> &[T] { self.<table>_map[node_id].as_slice() }
pub fn set_<table>(&mut self, node_id: NodeId, value: Vec<T>) { self.<table>_map[node_id] = value; }
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/store.rs:81-93` (`stack_phi_offsets`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/store.rs:99-108` (`call_other_name`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/store.rs:115-125` (`call_clobbered_override`)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/store.rs:132-148` (`asm_fingerprint`, with sort/dedup)

**Proposed helper:** A blanket impl over a sealed marker trait:
```rust
trait SideTable<V> { /* type-level pointer to the SecondaryMap field */ }
impl<V: Default> Graph {
    fn get_side<T: SideTable<V>>(&self, node: NodeId) -> &V { /* ... */ }
}
```
This is **judgement** territory — the side-table accessors are short
enough that hand-writing them may be clearer than a trait abstraction.
The proposal here is more about *naming uniformity* (e.g.
`call_other_name` returns `Option<&str>` while `stack_phi_offsets` returns
`&[i64]` — the asymmetry forces every caller to remember which is which)
than line savings.  The cleanest solution: a single
`SideTableAccess<NodeId, V>` newtype with `.get(node) -> &V` and
`.set(node, V)`, exposing each table as a typed field accessor.

**Migration:** Judgement.  Round8 priority is low — the bare-bones accessors
work fine.

**Effort:** M (bigger refactor than the current LOC suggest).

---

## #24 (LOW): "find unique node by kind" pattern repeated in IR/opt tests

**Pattern (3-5 LOC × 8+ sites):**
```rust
let call_node = g.all_node_ids()
    .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
    .unwrap();
```

**Sites:**
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/build_call_with_cc.rs:38-40, 86-88, 106-108, 154-..., 183-...` (5 sites)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/builder/tests.rs:1646-, 1674-` (2 sites)
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/tests.rs` (1 site)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/tests/wide_const_passthrough.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/tests/get_vn_with_call_override.rs`

**Proposed helper:** As part of `ir::test_utils` (or directly on
`BuiltFunctionGraph`):
```rust
impl BuiltFunctionGraph {
    pub fn find_node_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> Option<NodeId> {
        self.all_node_ids().find(|&n| pred(self.node_kind(n)))
    }
    pub fn find_unique_node_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> Option<NodeId> {
        let mut found = None;
        for n in self.all_node_ids() {
            if pred(self.node_kind(n)) {
                if found.is_some() { return None; }
                found = Some(n);
            }
        }
        found
    }
}
```
Subsumes #9 above (IndirectBranch finder) when called with
`|k| matches!(k, NodeKind::IndirectBranch)`.

**Migration:** Mostly mechanical.  Each site shrinks from
3 LOC to 1 LOC.

**Effort:** S.

---

## Summary table

| # | Severity | Title | Sites | LOC saved | Effort |
|---|----------|-------|-------|-----------|--------|
| 1 | HIGH | `count_reachable` HashSet pattern in opt tests | 14+ | ~70 | S |
| 2 | HIGH | `__scan_ignore_<arch>!` macro family | 16 | ~250 | M |
| 3 | HIGH | `PyMatch` accessor with-graph boilerplate | 11+ | ~30 | S |
| 4 | HIGH | PyO3 pattern builder finalise quartet | 14× | ~100 | S |
| 5 | HIGH | `Strider::new(arch, regs, cc)` boilerplate | 18 | ~36 | S |
| 6 | HIGH | strider-py CC build-from-sleigh boilerplate | 6 | ~24 | XS |
| 7 | HIGH | strider-py zero-sized pure-pass classes | 6 | ~24 | XS |
| 8 | HIGH | `addr.machine_addr.addr` chained accessor | 19 | ~0 (clarity) | XS |
| 9 | HIGH | "Find unique IndirectBranch placeholder" | 10+ | ~50 | S |
| 10 | HIGH | `pattern::Match::get_<op>_op` extractors | 8 | ~40 | XS |
| 11 | HIGH | Layer-C reachable-scope skeleton | 4 | ~12 | S |
| 12 | HIGH | `KnownBits` `unwrap_or_default` | 9 | (subsumed by §6.2) | – |
| 13 | MED | IR-builder `create_region/set_entry/set_region` boilerplate | 30+ | ~60 | M |
| 14 | MED | `rsleigh::Vn { ... }` literals workspace-wide | 31 files | ~50 | S |
| 15 | MED | `arch.probe_regs().unwrap()` repeats | 16+ | (subsumed by #5) | – |
| 16 | MED | `Sleigh::new(arch.sla_spec, arch.pspec, …)` | 12 | ~12 | XS |
| 17 | MED | `RunConfig { ... }` literal-construction | 9 | ~50 | M |
| 18 | MED | `set_lift_addr(Some) ... set_lift_addr(None)` RAII | 4 | ~6 | XS |
| 19 | MED | `count<F>(fg, pred)` re-implementation | 4 | ~12 | – (subsumed) |
| 20 | MED | `create_node + extend_fingerprint + node_outputs_exact::<1>` triplet | 8+ | ~40 | S |
| 21 | MED | `for nid in preorder() { if !matches!(...) { continue; }` | 8+ | ~16 | XS-S |
| 22 | MED | `node_outputs/inputs(...).into_iter().collect()` | 15+ | ~15 | XS |
| 23 | MED | Side-table accessor pair pattern | 4 tables | ~0 (uniformity) | M |
| 24 | LOW | `all_node_ids().find(matches!(NodeKind::X))` | 8+ | ~16 | S |

---

## Highest-leverage proposals

If a single PR can pick three, the highest-leverage are:

1. **#2** — Collapse the 16 per-arch `__scan_ignore_<arch>!` macro
   definitions (~250 LOC) into a single dispatcher with runtime
   ignore-list lookup.  Largest absolute LOC win in the workspace.

2. **#5 + #6 + #7** — One PR collapses three round-of-strider-py /
   strider tests boilerplate clusters: strider construction (~54
   LOC), CC build-from-sleigh (~36 LOC), and pure-pass classes (~30
   LOC).  All three are mechanical and stay inside the strider-py
   crate plus `strider/tests/common`.

3. **#1 + #19** — Promote the `count` / `count_reachable` /
   `return_value` helpers from `opt/tests/common/mod.rs` to a
   feature-gated `opt::test_utils` module so both white-box and
   black-box test files share a single source.  Eliminates ~70 LOC
   of repetition AND fixes the asymmetry where `tests/<file>.rs`
   has rich helpers but `src/<pass>/tests.rs` reinvents them.

The remaining items split into:
- **Quick wins (XS effort):** #6, #7, #8, #10, #16, #18, #21, #22.
- **Coordination items:** #11 + `round8-simplifications.md` §6.3 (Layer-C
  reachable scope), #8 + §3.6 (`PcodeInsnAddr` field encapsulation).
- **Judgement calls (don't ship without scope discussion):** #2 (runtime
  ignore-list), #17 (`RunConfigBuilder`), #23 (side-table trait).
