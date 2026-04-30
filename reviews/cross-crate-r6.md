# Cross-crate review — round 6 (2026-04-30)

## Summary

- **Crates surveyed:** 12 — 5 cross-cut (`opt`, `pattern`, `strider`, `cfg`, `ir`) + 7 full per-crate (`pcode-lift`, `target`, `reader`, `dot`, `graphwalk`, `entity-utils`, `graphmock`).
- **Findings total: 56** (47 from the surface pass + 9 algorithmic patterns from a focused second read pass).
  - A. Production simplification / generalization: 14
  - B. Test infrastructure consolidation: 8 (+ strategy decision)
  - C. Workspace hygiene: 6
  - D. Per-crate (small crates): 19
  - E. Algorithmic patterns: 9 (3 yes / 3 maybe / 3 no — folded in below; full analysis at [reviews/algorithmic-patterns-r6.md](algorithmic-patterns-r6.md))
- High-confidence findings: 31 (surface) + judgement-based for E.
- Low-risk findings: 36 (surface).

> **⚠️ Critical: P-001 catches a silent regression of opt-r6 F-013.**  The local `replace_output_uses` helper at `crates/opt/src/redundant_phis/mod.rs:11` was deleted in commit `6255d10` ("Applied" per the opt-r6 outcomes table) but **re-introduced by the anyhow merge `ebec8eb`**.  Outcomes-tables from earlier reviews should be re-verified post-anyhow.

The workspace is in good shape post-anyhow / post-Capture / post-strider-restructure. Standout cross-cutting themes:

- **A trivial `pub type Result<T> = anyhow::Result<T>;` lives as its own `error.rs` module file in 8 of 12 crates.** Of those, only `opt::Result` is referenced from outside its own crate (one site, `crates/opt/tests/common/mod.rs:18`). The other seven are a uniform vestigial wart: a 3-line file plus a `pub use error::Result;` line in `lib.rs`. Pattern is the lone exception (carries real sentinel structs).
- **Mock-IR test-helper duplication is significant.** `make_fn` / `make_fn_with_var` exist twice in `opt` alone (`src/test_support.rs` and `tests/common/mod.rs`). `reg_vn` / `sp_vn` are defined in 6+ separate files with subtle stack-pointer-width differences (`opt/tests`, `opt/benches`, `opt/src/{constant_fold,redundant_phis,dead_branch,stack_store,function_args}/tests.rs`, `pattern/tests/matching/support/graph.rs`). Roughly 50 sites manually call `FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)` — the universal "empty function" boilerplate.
- **Stale codename-and-doc drift remains widespread.** ~77 `BUG-N` references still appear in the 5 reviewed crates' tests (mostly in `crates/strider/tests/`); 4 stale `ErrorKind::*` doc-link references survive in `pcode-lift`, `dot`, `target`, `reader`, and `cfg::query` after the anyhow migration; opt's `lib.rs` still cites the deleted `RegionIrCache` type at lines 78 and 116; opt's `pipeline.rs` still mentions "F2's trait refactor" by codename in the `run_on_built` doc.
- **`vn_to_name` is duplicated** between `cfg::dot` and `ir::dot::label` (same logic, different return types). `decode_space_id` is duplicated between `strider::insn::mod` and `pcode-lift::value::mem_load` (identical logic).
- **The `(space.shortcut_raw(), off, size)` sort key still has TWO live sites** (`cfg::indirect_resolve` line 133 and `strider::pipeline::find_all_unique_vns` line 206) — the cfg r6 review's outcome (F-019) was "Applied (doc)", which only added a cross-reference comment. The duplicate code persists.
- **`is_tail_call` is duplicated** in `strider::orchestrator` and `cfg::Builder::is_branch_tail_call_nocheck` — strider's variant carries the comment "Mirrors `cfg::Builder::is_branch_tail_call_nocheck`."
- **`graphmock` is a workspace crate with one consumer** (graphwalk's `dev-dependencies`) and zero production callers anywhere. It's a 283-LOC test-fixture crate that could be `cfg(test)`-private to graphwalk — at the cost of moving its unit tests up.
- **Several `graphwalk` public types are dead** outside the crate's own tests: `PostOrder`, `PostOrderContext`, `entity_postorder`, `WalkPhase`, `TreePreOrder`, `TreePostOrder`, `NopTracker`, `VisitTracker`. The crate's own integration tests (`tests/postorder.rs`) exercise them, but no production code does.
- **`crates/strider/tests/common/mod.rs` is 800+ lines** mostly because of 15 per-arch `__scan_ignore_<arch>!` macros (each ~25 lines, near-identical). 800 LOC of test infra in one file dwarfs most production modules.

## Test consolidation strategy decision

**Recommendation: Option (b) — feature-flagged `test-utils` module on `ir`** (with `target` and `pattern` doing the same in their own narrow scope).

Justification:

- **The duplication concentrates around `ir::FunctionBuilder` boilerplate.** Every duplicated helper either calls `FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)` or wraps it in a one-liner; the `Tb` test-builder DSL in `pattern/tests/matching/support/graph.rs` is the most evolved version. None of the duplication is in patterns/cfg/strider-specific territory (those crates' helpers are genuinely different — `Tb` knows about pattern captures, the strider `Strider`-builder needs a Sleigh probe, the cfg synthetic-binary builder loads bytes).
- **Option (a) — new `ir-test-utils` crate — would be cleanest**, but: (i) the `Tb` builder relies on the unwrap-friendly `expect(...)` paradigm test code uses, which production `ir` doesn't allow; (ii) it adds a 13th workspace member; (iii) several helpers (e.g. the strider `make_strider_x86_64` helpers, the `pattern` `Tb`'s captures) genuinely don't generalise to a single crate. The minimal-shared core is small enough that a feature-gated module wins on weight.
- **Option (c) — leave alone — under-shoots.** The exact-duplicate `make_fn` between `opt/src/test_support.rs` and `opt/tests/common/mod.rs` is in-crate and trivially fixable; the workspace-wide `reg_vn` / `sp_vn` copies (with mismatched widths!) are an active bug surface.
- **The proposed `test-utils` feature on `ir`** would expose: `make_empty_fn`, `make_fn_with_vars`, `reg_vn`, `sp_vn` (with explicit `_x86`/`_x64` suffixes for the differing widths), `return_const`, plus a thin `Tb`-style builder. Crates' `[dev-dependencies]` enable the feature; production code can't reach it because `cfg(feature = "test-utils")` gates the module.
- **Strider-specific helpers stay in `crates/strider/tests/common/mod.rs`** — they need Sleigh, which `ir` doesn't depend on. The `make_strider_x86_64` duplication between `crates/strider/src/orchestrator.rs:687` and `crates/strider/tests/common/mod.rs:124` collapses by having the orchestrator's `cfg(test)` module import from the integration-test common (or by extracting both to a `cfg(test)`-public helper inside strider's `lib.rs`).
- **Pattern-specific `Tb`** stays in `crates/pattern/tests/matching/support/graph.rs` — the captures abstraction is too pattern-specific to live in `ir`.

## Table of contents

### A. Production simplification / generalization
- F-001 `pub type Result<T> = anyhow::Result<T>;` lives in 8 separate crates as a 3-line file (Duplication, multiple)
- F-002 `vn_to_name` is duplicated between `cfg::dot` and `ir::dot::label` (Duplication, `crates/cfg/src/cfg/dot.rs:26-42`, `crates/ir/src/dot/label.rs:19-39`)
- F-003 `decode_space_id` is duplicated between `pcode-lift::mem_load` and `strider::insn::mod` (Duplication, `crates/pcode-lift/src/value/mem_load.rs:18-30`, `crates/strider/src/strider/insn/mod.rs:142-154`)
- F-004 The `(space.shortcut_raw(), off, size)` sort key is duplicated between `cfg::indirect_resolve` and `strider::pipeline` (Duplication, `crates/cfg/src/cfg/builder/indirect_resolve.rs:133`, `crates/strider/src/strider/pipeline.rs:206`)
- F-005 `is_tail_call` is duplicated between `strider::orchestrator` and `cfg::Builder::is_branch_tail_call_nocheck` (Duplication, `crates/strider/src/orchestrator.rs:449-464`, `crates/cfg/src/cfg/builder/region_builder.rs:193-217`)
- F-006 `crates/opt/src/lib.rs` still cites the deleted `RegionIrCache` type in two places (Correctness, `crates/opt/src/lib.rs:78,116`)
- F-007 `opt::pipeline::run_on_built` doc cites "F2's trait refactor" by codename (Readability, `crates/opt/src/pipeline.rs:291`)
- F-008 4 stale `ErrorKind::*` doc references in `pcode-lift`, `dot`, `target`, `reader`, `cfg::query` (Correctness, multiple)
- F-009 ~77 `BUG-N` codename references remain across the 5 reviewed crates' tests (Readability, multiple)
- F-010 `opt`'s re-export `AnchorAddr` has zero callers outside `opt` (Dead code, `crates/opt/src/lib.rs:55-59`)
- F-011 8 of 8 crate-level `pub use error::Result` re-exports outside `opt`+`pattern` are unreferenced (Dead code, multiple)
- F-012 `RegionIndex` (`Vec<Option<T>>`-by-entity-index pattern) sits in strider but the pattern is ad-hoc; `entity-utils` could host an `EntityMap<E, V>` (Generalization, `crates/strider/src/orchestrator.rs:103-134`)
- F-013 `OptimizerOnBuilt` blanket-impls `Optimizer` to wrap `with_built` — pattern is consistent; no other crate's trait family uses the same idea, making it locally clean but architecturally inconsistent (Readability, `crates/opt/src/pipeline.rs:144-165`)
- F-014 The 6-step `let probe = rsleigh::mem_readers::BufMemReader::new(...) → Sleigh::new(...)?.regs()?` boilerplate is in 6 production+test sites (Duplication, multiple)

### B. Test infrastructure consolidation
- F-015 Inventory of mock-IR helpers (overview + duplication map)
- F-016 `make_fn` / `make_fn_with_var` are defined TWICE inside `opt` (Duplication, `crates/opt/src/test_support.rs:8-40`, `crates/opt/tests/common/mod.rs:21-51`)
- F-017 `reg_vn` is defined in 6 different files with identical body (Duplication, multiple)
- F-018 `sp_vn` is defined in 6 different files with conflicting widths (Correctness/Duplication, multiple)
- F-019 `make_strider_x86_64` is duplicated between `strider::orchestrator` test module and `strider::tests::common` (Duplication, `crates/strider/src/orchestrator.rs:687-696`, `crates/strider/tests/common/mod.rs:124-126,131-139`)
- F-020 `crates/strider/tests/common/mod.rs` macros are 15 near-identical per-arch `__scan_ignore_<arch>!` definitions (~400 lines) (Simplification, `crates/strider/tests/common/mod.rs:448-823`)
- F-021 `current_anchor_after_opt` / `anchor_value_input` exist as duplicates inside `crates/strider/tests/common/tier2_helpers/orchestrator.rs` (the strider review noted this as F-028 "Applied" but a partial duplication remains in tests) (Duplication, `crates/strider/tests/common/tier2_helpers/orchestrator.rs`)
- F-022 `~50 sites manually inline `FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)`` (Duplication, multiple)

### C. Workspace hygiene
- F-023 Workspace `Cargo.toml` declares `rustc-hash` as a workspace dep but only `opt`'s `Cargo.toml` references it (Dead code, `Cargo.toml:40`)
- F-024 `paste = "1"` is a workspace dep used only by `crates/strider`'s `[dev-dependencies]` — could collapse to a direct version pin (Simplification, `Cargo.toml:34`)
- F-025 `tempfile` workspace dep is referenced only by `reader`'s `[dev-dependencies]` (Simplification, `Cargo.toml:32`)
- F-026 `opt`'s `criterion` direct version (`0.7`) sits outside the workspace deps even though benches are workspace infrastructure (Readability, `crates/opt/Cargo.toml:13`)
- F-027 `pattern`'s `bitflags = "2"` is a direct dep — only consumer in workspace, but `2` matches no other version constraint (Readability, `crates/pattern/Cargo.toml:11`)
- F-028 CLAUDE.md still references `Strider::new(arch, sleigh_regs, cc)` as the entry point in the per-crate description; the canonical entry is `strider::run(config)` per the latest CLAUDE.md (Readability, `CLAUDE.md` strider section)

### D. Per-crate findings on the 7 small crates

#### pcode-lift
- F-029 `vn_io.rs` doc cites deleted `ErrorKind::UnsupportedVnSpace`/`NoRegisterContainer` (Correctness, `crates/pcode-lift/src/vn_io.rs:124-126`)
- F-030 `vn_io.rs` carries 6+ `BUG-9` references in test names and comments (Readability, `crates/pcode-lift/src/vn_io.rs:380-451`)
- F-031 `float_type_from_vn` mirrors `TryFrom<u32> for NodeOutputType` for the float subset and could delegate (Duplication, `crates/pcode-lift/src/value/float.rs:22-29`)

#### target
- F-032 `vn_for_name` doc cites deleted `ErrorKind::UnknownRegName` (Correctness, `crates/target/src/calling_convention.rs:7,435`)
- F-033 `target/src/calling_convention.rs` test module is 657 lines (`tests` mod within the same file) — would benefit from extraction (Readability, `crates/target/src/calling_convention.rs:466-1121`)
- F-034 `target::error::Result<T>` is a trivial alias with zero external callers (Dead code, `crates/target/src/error.rs`)

#### reader
- F-035 `MemRegion::new`'s documented overflow-check rejects `start_addr + data.len() > u64::MAX` but `data.len() as u64` is `usize as u64` — sound on 64-bit, silently UB-adjacent on hypothetical 128-bit (Correctness/low confidence, `crates/reader/src/lib.rs:82-88`)
- F-036 `tests/elf_converters.rs:281-346` doc comments still cite `ErrorKind::Object(_)` / `ErrorKind::RegionOverflow` (Correctness, `crates/reader/tests/elf_converters.rs:281-346`)
- F-037 `reader::error::Result<T>` has zero external callers (Dead code, `crates/reader/src/error.rs`)

#### dot
- F-038 `dump_as_html` doc cites deleted `ErrorKind::DotDumpError` and `ErrorKind::IoError` (Correctness, `crates/dot/src/lib.rs:454-455`)
- F-039 `dot::error::Result<T>` has zero external callers (Dead code, `crates/dot/src/error.rs`)

#### graphwalk
- F-040 `PostOrder` and friends (`PostOrderContext`, `entity_postorder`, `WalkPhase`, `TreePreOrder`, `TreePostOrder`, `NopTracker`, `VisitTracker`) are pub but used only by graphwalk's own tests + `graphmock` test-fixture crate (Dead code, `crates/graphwalk/src/lib.rs`)
- F-041 `entity_preorder` / `PreOrder` are the only graphwalk types reachable from production code (`ir::walk`); the rest of the public surface is "tested but unreached" (Dead code, `crates/graphwalk/src/lib.rs`)

#### entity-utils
- F-042 No findings; the crate is small, well-tested, well-documented, and has no obvious duplications (—)

#### graphmock
- F-043 `graphmock` is a 283-LOC workspace crate whose only consumer is `graphwalk`'s `[dev-dependencies]`; production code never reaches it (Simplification, `crates/graphmock/Cargo.toml`)
- F-044 `graphmock::graph` deliberately panics on malformed input rather than returning `Result` (line 119); the crate-level allow is honoured but the test panic is undocumented in the lib.rs intro (Readability, `crates/graphmock/src/lib.rs:96-119`)
- F-045 `graphmock`'s own `mod tests` includes 7 distinct test cases covering the DSL itself; this is reasonable in-tree coverage but the crate name suggests a fixture rather than a module — a `pub fn graph` that only graphwalk uses with extensive self-tests is unusual structure (Readability, `crates/graphmock/src/lib.rs`)

#### Cross-cutting on the 7 small crates
- F-046 4 of 7 small crates carry a 3-line `error.rs` module file holding only `pub type Result<T> = anyhow::Result<T>;` (`pcode-lift`, `target`, `reader`, `dot`) — see F-001 (Duplication, multiple)
- F-047 6 of 7 small crates' `lib.rs` opens with the same `#![cfg_attr(test, allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable))]` block (`pcode-lift`, `target`, `reader`, `dot`, `entity-utils`, `graphmock`, `ir`, `opt`, `pattern`, `strider`) — could move to workspace-level `[lints]` (Readability, multiple)

## A. Production simplification / generalization

### F-001 `pub type Result<T> = anyhow::Result<T>;` lives in 8 separate crates as a 3-line file

- **Category:** Duplication & unification
- **Crate(s):** `cfg`, `dot`, `ir`, `opt`, `pcode-lift`, `reader`, `target`, plus `pattern` (which has real types alongside)
- **Location:** `crates/cfg/src/error.rs:1-3`, `crates/dot/src/error.rs:1-3`, `crates/ir/src/error.rs:1-8`, `crates/opt/src/error.rs:1-3`, `crates/pcode-lift/src/error.rs:1-3`, `crates/reader/src/error.rs:1-3`, `crates/target/src/error.rs:1-3`
- **What:** Identical contents across 7 crates:
  ```rust
  //! Error type for the `<crate>` crate.

  pub type Result<T> = anyhow::Result<T>;
  ```
  All 7 crates also `pub use error::Result;` from `lib.rs`. Workspace-wide search shows only ONE external reference: `crates/opt/tests/common/mod.rs:18:use opt::Result;`. The other six re-exports (`cfg::Result`, `dot::Result`, `ir::Result`, `pcode_lift::Result`, `reader::Result`, `target::Result`) have zero external callers.
- **Why:** Post-anyhow-migration, the alias was retained "in case" but never picked up new external use. Each `error.rs` is a friction tax with no value (a `crate::Result` works inside the crate just as well as in a separate file). The strider review's F-014 already deleted strider's identical 3-line file; the same logic applies here.
- **Proposed change:** Pick one of:
  - (a) Delete each crate's `error.rs` and the corresponding `pub use` line; put `type Result<T> = anyhow::Result<T>;` directly in `lib.rs` (matches strider).
  - (b) Move `Result` to `lib.rs` as a private alias; drop the `pub use`. The single `opt::Result` external callsite in opt's own tests can `use opt::Result` if `opt::lib.rs` exposes it.
  - In `pattern`, keep `error.rs` (real sentinel types).
  - In `ir`, keep `error.rs` (the explanatory doc about `ValidationErrors`-via-downcast warrants a dedicated module).
- **Confidence:** high
- **Risk if applied:** low (mechanical; affects only re-export structure)

### F-002 `vn_to_name` is duplicated between `cfg::dot` and `ir::dot::label`

- **Category:** Duplication & unification
- **Crate(s):** `cfg`, `ir`
- **Location:** `crates/cfg/src/cfg/dot.rs:26-42`, `crates/ir/src/dot/label.rs:19-39`
- **What:** Both files implement varnode-to-display-name dispatch over `rsleigh::VnSpace`:
  ```rust
  // cfg/cfg/dot.rs (returns Result, takes Option<&SleighRegs>):
  fn vn_to_name(regs: Option<&rsleigh::SleighRegs>, vn: &rsleigh::Vn) -> Result<String> {
      match vn.addr.space {
          rsleigh::VnSpace::REGISTER => { ... regs.vn_to_name(*vn) ... }
          rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
          rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
          rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
          ...
      }
  }

  // ir/dot/label.rs (returns io::Result, method on a Sleigh-borrowing struct):
  fn vn_to_name(&self, vn: &rsleigh::Vn) -> io::Result<String> {
      match vn.addr.space {
          rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
          rsleigh::VnSpace::REGISTER => { ... regs.vn_to_name(*vn) ... }
          rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
          rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
          s if s == self.sleigh.default_code_space() => Ok(format!("ram[{offset:#x}]:{size}")),
          ...
      }
  }
  ```
- **Why:** Same problem (render a varnode for a DOT label), same logic, two minor differences: (i) cfg's version errors on REGISTER without `regs`, ir's version always has Sleigh; (ii) ir's version checks `default_code_space()` (treats e.g. ARM's "ram" alias correctly). The error-type mismatch is an artefact of `dot`'s `GraphDotDumper::Error: Display` — both crates pick a different concrete Error.
- **Proposed change:** Extract a shared `vn_to_display_name(sleigh: &Sleigh, vn: &Vn) -> anyhow::Result<String>` into `dot` (the renderer crate) or `target` (since varnode display is target-data territory). The cfg-side reg-check-with-Option-Sleigh is special: cfg's caller has `&Cfg<R>` which holds a Sleigh, so the `Option` path is dead in production (only kept for the test_api forwarder).
- **Confidence:** medium (test_api forwarder needs adjustment)
- **Risk if applied:** medium (touches test wiring + DotDumper trait Error type)

### F-003 `decode_space_id` is duplicated between `pcode-lift::mem_load` and `strider::insn::mod`

- **Category:** Duplication & unification
- **Crate(s):** `pcode-lift`, `strider`
- **Location:** `crates/pcode-lift/src/value/mem_load.rs:18-30`, `crates/strider/src/strider/insn/mod.rs:142-154`
- **What:** Both files implement an identical helper that decodes the target address-space of a p-code LOAD/STORE:
  ```rust
  // pcode-lift's version (handles LOAD):
  fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
      let space_id_vn = *insn.inputs.first().ok_or_else(|| anyhow!(...))?;
      if space_id_vn.addr.space != rsleigh::VnSpace::CONST { bail!(...); }
      Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
  }

  // strider's version (handles STORE):
  fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
      let space_id_vn = *first_input_or_err(insn)?;
      if space_id_vn.addr.space != rsleigh::VnSpace::CONST { bail!(...); }
      Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
  }
  ```
  Strider's variant uses the local `first_input_or_err` helper (extracted in the strider review); pcode-lift's variant inlines the `inputs.first().ok_or_else(...)`.
- **Why:** strider's `handle_store` is in strider because Store advances the memory chain (a strider concern); pcode-lift's `handle_load` is in pcode-lift because Load is value-producing. But the **address-space decoding logic is identical** and orthogonal to which side of the chain.
- **Proposed change:** Lift `decode_space_id` and `first_input_or_err` to `pcode-lift::lib.rs` (the natural lifter-side home). Strider imports `pcode_lift::decode_space_id` for `handle_store`.
- **Confidence:** high
- **Risk if applied:** low

### F-004 The `(space.shortcut_raw(), off, size)` sort key is duplicated between `cfg::indirect_resolve` and `strider::pipeline`

- **Category:** Duplication & unification
- **Crate(s):** `cfg`, `strider`
- **Location:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:133`, `crates/strider/src/strider/pipeline.rs:206`
- **What:**
  ```rust
  // cfg:
  all_vns.sort_unstable_by_key(|vn| (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size));

  // strider:
  vns.sort_unstable_by_key(|vn| (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size));
  ```
  The cfg site has a comment that says: "This sort key is duplicated in `strider::pipeline::find_all_unique_vns` and must stay in lockstep — both downstream IRs key VarId off the same order."
- **Why:** The cfg r6 review's F-019 outcome was "Applied (doc): Comment now points at strider's twin sort key explicitly". The doc was added; the duplicate code remains. Both sites do the same thing — sort varnodes for stable VarId numbering — and the explicit "must stay in lockstep" comment is a smell that the abstraction is missing. Either crate could expose a helper `pub fn vn_sort_key(vn: &Vn) -> (i32, u64, u32)` so both call sites stay automatically in sync.
- **Proposed change:** Add `vn_sort_key(vn) -> (...)` (or `vn_sort_unstable(&mut Vec<Vn>)`) to `target` (since `Vn`-handling is target territory) or to a new `pcode-lift` helper. Both sites call it; the lockstep comment becomes "see `target::vn_sort_key`".
- **Confidence:** high
- **Risk if applied:** low (single function lift)

### F-005 `is_tail_call` is duplicated between `strider::orchestrator` and `cfg::Builder::is_branch_tail_call_nocheck`

- **Category:** Duplication & unification
- **Crate(s):** `cfg`, `strider`
- **Location:** `crates/strider/src/orchestrator.rs:449-464`, `crates/cfg/src/cfg/builder/region_builder.rs:193-217`
- **What:** Both files implement the same address-bounds tail-call test:
  ```rust
  // strider:
  fn is_tail_call(target: u64, opts: &RunOpts<'_>) -> bool {
      if target < opts.start_addr && !opts.allow_code_before_start_addr { return true; }
      if let Some(fn_max_size) = opts.fn_max_size {
          let end_exclusive = opts.start_addr.saturating_add(fn_max_size);
          if end_exclusive <= target { return true; }
      }
      false
  }
  // Comment: `Mirrors cfg::Builder::is_branch_tail_call_nocheck.`

  // cfg:
  pub(super) fn is_branch_tail_call_nocheck(&self, branch_target_addr: PcodeInsnAddr) -> bool {
      let addr = branch_target_addr.machine_addr;
      if addr < self.builder.start_addr && !self.builder.options.allow_code_before_start_addr { return true; }
      if let Some(fn_max_size) = self.builder.options.fn_max_size {
          let end_exclusive = self.builder.start_addr.addr.saturating_add(fn_max_size);
          if addr.addr >= end_exclusive { return true; }
      }
      false
  }
  ```
  Strider's version takes `RunOpts`; cfg's takes `&self`. Otherwise identical.
- **Why:** The strider orchestrator is making tail-call decisions on a `Single(K)` resolution AFTER the cfg has already classified branch targets. Both layers need the same predicate; the orchestrator can't reach into the cfg's `Builder` (which is consumed at this point), so it ports the function. The comment "Mirrors `cfg::Builder::is_branch_tail_call_nocheck`" is a lockstep smell.
- **Proposed change:** Move `is_tail_call(start_addr, fn_max_size, allow_code_before_start_addr, target) -> bool` to `cfg::query` or to a new `cfg::tail_call_check` module as a pure function. Both `Builder::is_branch_tail_call_nocheck` (forwarder) and `strider::orchestrator::is_tail_call` (forwarder) delegate to it.
- **Confidence:** high
- **Risk if applied:** low

### F-006 `crates/opt/src/lib.rs` still cites the deleted `RegionIrCache` type in two places

- **Category:** Correctness (docs)
- **Crate(s):** `opt`
- **Location:** `crates/opt/src/lib.rs:78`, `crates/opt/src/lib.rs:116`; also `crates/opt/tests/pipeline_subsets.rs:9,32`
- **What:**
  ```rust
  // opt/src/lib.rs:78 (in stable_default_pipeline doc):
  /// in place but never *removes* phi / `ControlState` / `If` nodes that
  /// the strider [`RegionIrCache`] pins by `NodeId`.

  // opt/src/lib.rs:116 (in destructive_default_pipeline doc):
  /// Running these passes mid-iteration would invalidate the
  /// [`RegionIrCache`] because the cache's pinned phi `NodeId`s and
  ```
  Per CLAUDE.md and the strider review (F-005 / F-013 / F-027 "Obviated | Cache gone"), `RegionIrCache` was deleted in the strider restructure. The post-restructure structure is `RegionIndex` (per-iteration, in `crates/strider/src/orchestrator.rs:103`) which serves the same role.
- **Why:** ir review F-009 caught these in the ir crate; opt didn't get the matching review pass.
- **Proposed change:** Replace `RegionIrCache` with "the orchestrator's per-iteration `RegionIndex`" (or just "the strider orchestrator's per-iteration index"). Apply to both opt sites + the two opt test sites that mention it.
- **Confidence:** high
- **Risk if applied:** low (docs only)

### F-007 `opt::pipeline::run_on_built` doc cites "F2's trait refactor" by codename

- **Category:** Readability
- **Crate(s):** `opt`
- **Location:** `crates/opt/src/pipeline.rs:291`, `crates/opt/src/pipeline.rs:330`
- **What:**
  ```rust
  /// `BuiltFunctionGraph` keep working unchanged through F2's trait
  /// refactor; new code is encouraged to call [`Self::run`] directly with
  ```
  And in the test: `// drop-in replacement" contract.`. The codename `F2` survives.
- **Why:** Per the opt r6 review F-037, codename labels were stripped — but `pipeline.rs` slipped through.
- **Proposed change:** Replace "F2's trait refactor" with "the `Optimizer` / `OptimizerOnBuilt` split" (the actual refactor name in code).
- **Confidence:** high
- **Risk if applied:** low (docs only)

### F-008 4 stale `ErrorKind::*` doc references in `pcode-lift`, `dot`, `target`, `reader`, `cfg::query`

- **Category:** Correctness (docs)
- **Crate(s):** `cfg`, `dot`, `pcode-lift`, `reader`, `target`
- **Location:**
  - `crates/pcode-lift/src/vn_io.rs:124-126` (`ErrorKind::UnsupportedVnSpace`, `ErrorKind::NoRegisterContainer`)
  - `crates/dot/src/lib.rs:454-455` (`ErrorKind::DotDumpError`, `ErrorKind::IoError`)
  - `crates/target/src/calling_convention.rs:7,435` (`ErrorKind::UnknownRegName`)
  - `crates/reader/tests/elf_converters.rs:281,344,346` (`ErrorKind::Object(_)`, `ErrorKind::RegionOverflow`)
  - `crates/cfg/src/cfg/query.rs:67` (`ErrorKind::DuplicateEdgeKind`)
  - `crates/ir/src/graph/access.rs:43,78,121` (`ErrorKind::WrongOutputCount`, `ErrorKind::WrongInputCount`, `ErrorKind::InputIndexOutOfBounds`)
- **What:** All these doc strings reference `ErrorKind::Foo` variants that no longer exist post-anyhow. ir's review (F-008) caught 97 sites in ir; cfg's review (F-003) caught 14 sites in cfg. The remaining sites are in the un-reviewed crates plus three stragglers in `ir::graph::access.rs` (despite the ir review claiming "Stripped 97 ErrorKind:: doc links across the crate").
- **Why:** The anyhow migration didn't reach docstrings outside the per-crate-reviewed set.
- **Proposed change:** Replace each `[`ErrorKind::X`]` with prose describing the actual error condition. Drop the `[…]` brackets so they don't render as broken doc links.
- **Confidence:** high
- **Risk if applied:** low

### F-009 ~77 `BUG-N` codename references remain across the 5 reviewed crates' tests

- **Category:** Readability
- **Crate(s):** `cfg`, `opt`, `pattern`, `strider`, `ir`
- **Location:** `crates/strider/tests/{control,arithmetic,builtins,floats,calling_convention,read_reg_vn_truncate,common_smoke,...}.rs` (most), plus `crates/cfg/tests/region_builder_decode.rs:238`, `crates/strider/tests/calling_convention.rs:10,24,167,183,200,218`, others
- **What:** Per-crate reviews stripped codenames from production prose but tests are full of `BUG-1`, `BUG-2`, `BUG-3`, `BUG-9`, `BUG-22`, `BUG-23`, `BUG-28`, `BUG-29`, etc. References:
  ```
  crates/strider/tests/floats.rs:11://   1. BUG-9 write_reg_vn mask positioning fix (aarch64 D0/Q0).
  crates/strider/tests/control.rs:35:// BUG-22 fixed: count *control paths into Return* (sum of ControlState fan-in
  crates/strider/tests/floats.rs:25:    ArmBe: "BUG-29: arm_be VFP regs descending-offset; ..."
  ```
- **Why:** Tests are not "documentation"-grade prose, but the BUG-N codenames are opaque to a future reader without the bug ledger.
- **Proposed change:** Either (a) leave alone — tests are internal-grade and removing the codenames loses traceability to the original investigation; (b) attach a sentence-fragment of context per codename so a reader without the ledger still understands what the test pins. The strider review's F-026 / opt's F-037 did this for production prose. Defer per-test stripping to a future per-crate test-cleanup pass; just **don't introduce new BUG-N codenames** is the realistic policy.
- **Confidence:** high (the references exist)
- **Risk if applied:** medium (stripping tests has low value; just ack the policy)

### F-010 `opt`'s re-export `AnchorAddr` has zero callers outside `opt`

- **Category:** Dead code
- **Crate(s):** `opt`
- **Location:** `crates/opt/src/lib.rs:55-59`
- **What:**
  ```rust
  pub use indirect_branch_resolve::{
      AnchorAddr, AnchorCallingContext, IndirectBranchResolve, ResolvedTargets,
      apply_link_register, apply_tail_call, classify_anchor, classify_anchor_with_rom,
      classify_anchor_with_rom_and_sp, find_placeholder_return_for_anchor,
  };
  ```
  Workspace-wide grep for `opt::AnchorAddr`: zero hits outside `opt/src/`. Other re-exports here ARE used externally (`opt::AnchorCallingContext`, `opt::classify_anchor*`, `opt::apply_*`, `opt::ResolvedTargets`, `opt::find_placeholder_return_for_anchor`).
- **Why:** Per the opt r6 review F-007 ("dead re-exports dropped"), `opt::lib.rs`'s re-export list was scrubbed; `AnchorAddr` slipped through.
- **Proposed change:** Drop `AnchorAddr` from the `pub use`. If it's still needed by opt-internal modules, those modules can import `crate::indirect_branch_resolve::AnchorAddr` directly.
- **Confidence:** high
- **Risk if applied:** low

### F-011 8 of 8 crate-level `pub use error::Result` re-exports outside `opt`+`pattern` are unreferenced

- **Category:** Dead code
- **Crate(s):** `cfg`, `dot`, `ir`, `pcode-lift`, `reader`, `target`
- **Location:** `crates/cfg/src/lib.rs:22`, `crates/dot/src/lib.rs:44`, `crates/ir/src/lib.rs:55`, `crates/opt/src/lib.rs:37`, `crates/pcode-lift/src/lib.rs:31`, `crates/reader/src/lib.rs:19`, `crates/target/src/lib.rs:23`
- **What:** Each crate exposes `pub use error::Result` despite having zero external users (verified by `grep -rn "<crate>::Result" crates/ --include="*.rs"`). Cross-references with F-001.
- **Why:** Same root cause as F-001: anyhow migration collapsed the original `ErrorKind`-typed `Result` to a trivial alias, and the re-export wasn't audited. The single live external consumer (`opt::Result` in opt's own tests) is internal to opt.
- **Proposed change:** See F-001's diff sketch (a) — drop `pub use error::Result` along with deletion of the `error.rs` file in 6 crates. Keep `ir::Result` since `ir::error.rs` will stay (validation-error commentary).
- **Confidence:** high
- **Risk if applied:** low (mechanical)

### F-012 `RegionIndex` (`Vec<Option<T>>`-by-entity-index pattern) sits in strider; `entity-utils` could host `EntityMap<E, V>`

- **Category:** Generalization
- **Crate(s):** `strider` (current home), `entity-utils` (proposed home)
- **Location:** `crates/strider/src/orchestrator.rs:103-134`
- **What:** strider's `RegionIndex` is a `HashMap<NodeOutputId, RegionExitInfo>` keyed by exit-control output id. The original strider review (F-036) called for `Vec<Option<ir::RegionId>>` indexed by `RegionId.index()`. The current `RegionIndex` is `HashMap`-backed, but the pattern of "sparse entity-indexed table" recurs:
  ```rust
  struct RegionIndex {
      by_exit_control: HashMap<NodeOutputId, RegionExitInfo>,
  }
  ```
- **Why:** The user's brief explicitly asked: "`Vec<Option<T>>` indexed by entity index (strider's `RegionIndex`). Could `entity-utils` provide a shared helper?"
  Looking at the `entity-utils` crate (`crates/entity-utils/src/{set,worklist}.rs`), it has `DenseEntitySet<E>` and `Worklist<E>` but no `EntityMap<E, V>`. The strider F-036 outcome notes the change was actually applied as `Vec<Option<RegionId>>` keyed by region index; the **current** `orchestrator.rs` `RegionIndex` is a different `HashMap` after the post-restructure work and the pattern became per-call. So the abstraction is more "every iteration we build a HashMap from a Vec of handles" than a stable `EntityMap` type.
- **Proposed change:** Add `EntityMap<E: EntityRef, V>` to `entity-utils::map` mirroring `DenseEntitySet`. Three operations: `insert(e, v)`, `get(e) -> Option<&V>`, `iter()`. Strider's `RegionIndex` doesn't directly benefit (it's keyed by `NodeOutputId`, which is dense but not the natural key for this lookup). Defer until a real consumer appears — currently speculative.
- **Confidence:** low
- **Risk if applied:** low (additive)

### F-013 `OptimizerOnBuilt` blanket-impl pattern is locally clean but architecturally inconsistent

- **Category:** Readability / generalization
- **Crate(s):** `opt`
- **Location:** `crates/opt/src/pipeline.rs:144-165`
- **What:** opt has TWO traits — `Optimizer` (low-level, takes `&mut ir::Graph + entry`) and `OptimizerOnBuilt` (high-level, takes `&mut BuiltFunctionGraph`) — with a blanket `impl<T: OptimizerOnBuilt> Optimizer for T` that wraps `with_built` per call.
  ```rust
  impl<T: OptimizerOnBuilt> Optimizer for T {
      fn optimize(&self, graph: &mut ir::Graph, entry: ir::node::NodeId) -> crate::Result<OptimizationResult> {
          with_built(graph, entry, |function| self.optimize_built(function))
      }
  }
  ```
  No other crate uses this pattern. cfg's `Builder` family doesn't have a sibling on `Cfg`; `pattern` builders don't have a `dyn Pat` boundary; `ir::FunctionBuilder` is a single concrete type.
- **Why:** The user's brief asks whether trait-pair patterns across crates are consistent in shape. They're not — but each crate's choice is correct for its domain: opt has heterogeneous passes that legitimately split between low-level (operate on `&mut Graph`) and high-level (BuiltFunctionGraph) inputs; cfg's Builder is monomorphic. No workspace-level convention would emerge.
- **Proposed change:** None. Document the rationale (the comment block above `OptimizerOnBuilt` already does this). Flagged only because the user explicitly asked for the audit — confirm "no shared abstraction warranted".
- **Confidence:** high (the analysis itself; not a fix)
- **Risk if applied:** —

### F-014 The 6-step `let probe = rsleigh::mem_readers::BufMemReader::new(...) → Sleigh::new(...)?.regs()?` boilerplate is in 6 production+test sites

- **Category:** Duplication & unification
- **Crate(s):** `ir`, `strider`, `target`
- **Location:** `crates/ir/src/dot/tests.rs:14`, `crates/target/src/calling_convention.rs:469`, `crates/strider/tests/indirect_branch.rs:66`, `crates/strider/tests/common/mod.rs:131-138`, `crates/strider/src/strider/pipeline.rs` (`Strider::new` call sites), `crates/strider/src/orchestrator.rs:687-693`
- **What:** Same 4-line boilerplate everywhere:
  ```rust
  let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
  let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)
      .expect("probe sleigh")
      .regs()
      .expect("probe regs");
  ```
- **Why:** Every test that needs a `SleighRegs` for arch-X but doesn't decode any code from a real binary writes this. The `target` crate already owns `SleighArch::*()` presets — a natural extension is `SleighArch::probe_regs() -> SleighRegs` that hides the empty-buffer Sleigh dance.
- **Proposed change:** Add `pub fn probe_regs(self) -> Result<rsleigh::SleighRegs>` to `target::SleighArch`. Test sites collapse to `let regs = arch.probe_regs()?;`. Saves ~4 lines × 6 sites = ~24 LOC of test/production boilerplate.
- **Confidence:** high
- **Risk if applied:** low (additive helper; existing call sites can adopt incrementally)

## B. Test infrastructure consolidation

### Inventory of mock-IR helpers

| Helper | File | Line | Body | Sites that call it |
| --- | --- | --- | --- | --- |
| `make_fn` | `crates/opt/src/test_support.rs:8` | 12 | `FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)` → set entry → return | opt's white-box `mod tests` only (3 modules) |
| `make_fn` | `crates/opt/tests/common/mod.rs:21` | 12 | **Identical body** | opt's integration tests (5 files) |
| `make_fn_with_var` | `crates/opt/src/test_support.rs:25` | 16 | Same + tracks 1 var via `read_variable` | opt's white-box `mod tests` only |
| `make_fn_with_var` | `crates/opt/tests/common/mod.rs:36` | 16 | **Identical body** | opt's integration tests |
| `make_fn_with_var` | `crates/opt/src/known_bits/tests.rs:262` | 27 | **Different shape** — variant that takes 1-byte var; defined locally | known_bits/tests.rs only |
| `return_const` | `crates/ir/tests/common/mod.rs:17` | 9 | `IntConst → return` shape | ir's integration tests |
| `return_binop` | `crates/ir/tests/common/mod.rs:28` | 18 | `IntConst op IntConst → return` | ir's integration tests |
| `return_int_cmp` | `crates/ir/tests/common/mod.rs:49` | 18 | `IntConst cmp IntConst → return` | ir's integration tests |
| `Tb` (test builder DSL) | `crates/pattern/tests/matching/support/graph.rs:40` | 411 | Full fluent builder over `FunctionBuilder` | every pattern test (~14 files) |
| `make_builder` (cfg) | `crates/cfg/tests/common/synthetic.rs:49` | 4 | Wraps `Builder::new` with empty Sleigh | cfg's integration tests |
| `make_builder_opts` | `crates/cfg/tests/common/synthetic.rs:54` | 4 | Like make_builder + custom options | cfg's integration tests |
| `make_builder_with_bytes` | `crates/cfg/tests/common/synthetic.rs:59` | 6 | Wraps `Builder::new` with byte-backed Sleigh | cfg's integration tests |
| `make_sleigh` | `crates/cfg/tests/common/synthetic.rs:26` | 9 | Empty-buffer x86 Sleigh | cfg's integration tests |
| `make_sleigh` | `crates/pcode-lift/tests/value_lifter.rs:27` | 10 | Empty-buffer x86 Sleigh — **near-identical to cfg's** | pcode-lift's only integration-test file |
| `strider_x86_64` | `crates/strider/tests/common/mod.rs:124` | 3 | `strider_for(Arch::X64)` | strider tests (~4 files) |
| `strider_for` | `crates/strider/tests/common/mod.rs:131` | 9 | `Sleigh probe → regs() → Strider::new` | strider tests + via `strider_x86_64` |
| `make_strider_x86_64` | `crates/strider/src/orchestrator.rs:687` | 10 | Same body as `strider_for(Arch::X64)` — **duplicate** | orchestrator's `mod tests` only |
| `reg_vn` | `crates/opt/tests/common/mod.rs:89` | 9 | `Vn { addr, size }` | opt integration tests |
| `reg_vn` | `crates/opt/src/dead_branch/tests.rs:?` | 9 | **Identical body** | dead_branch's white-box tests |
| `reg_vn` | `crates/opt/src/redundant_phis/tests.rs:?` | 9 | **Identical body** | redundant_phis tests |
| `reg_vn` | `crates/opt/src/stack_store/tests.rs:?` | 9 | **Identical body** | stack_store tests |
| `reg_vn` | `crates/opt/src/function_args/tests.rs:?` | 9 | **Identical body** | function_args tests |
| `reg_vn` | `crates/opt/benches/stack_store.rs:?` | 9 | **Identical body** | stack_store bench |
| `reg_vn` | `crates/pattern/tests/matching/support/graph.rs:19` | 9 | **Identical body** | pattern tests |
| `reg_vn` | `crates/ir/src/builder/tests.rs:?` | 9 | **Identical body** | ir's white-box builder tests |
| `sp_vn` | `crates/opt/tests/common/mod.rs:100` | 3 | `reg_vn(0x20, 4)` (x86 width) | opt integration |
| `sp_vn` | `crates/pattern/tests/matching/support/graph.rs:32` | 3 | `reg_vn(0x20, 8)` (**x86_64 width!**) | pattern tests |
| `sp_vn` | `crates/opt/benches/stack_store.rs` | 6 | inline `Vn { off: 0x20, size: 4 }` (x86) | benches |
| `sp_vn` | `crates/opt/src/stack_store/tests.rs` | 9 | inline `Vn { off: 0x20, size: 4 }` (x86) | stack_store tests |
| `sp_vn` | `crates/opt/src/redundant_phis/tests.rs` | 9 | inline `Vn { off: 0x20, size: ?}` | redundant_phis tests |
| `fake_reg_vn` | `crates/strider/src/indirect_resolve_tier2/classify.rs:83` | 9 | Same body as `reg_vn` but different name | strider classify tests |

### F-015 (overview, no fix-action)

- **Category:** Inventory; cross-references F-016 through F-022.
- The duplication centres around:
  1. **`make_fn` / `make_fn_with_var`** — two file copies inside opt alone.
  2. **`reg_vn`** — six identical bodies.
  3. **`sp_vn`** — six bodies with **two different stack-pointer widths** (x86 4-byte vs x86_64 8-byte). This is a correctness hazard: a stack_store test that copies the `sp_vn()` from `opt/tests/common` (4-byte) and then checks against `pattern`'s 8-byte assumption would silently misclassify which sub-register an offset lands in.
  4. **`FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)`** — ~50 sites manually inline this 6-arg "empty function" boilerplate.
- The proposed `ir`-with-`test-utils`-feature module would expose: `make_empty_fn(f) -> BuiltFunctionGraph`, `make_fn_with_var(vn, f) -> ...`, `reg_vn(off, size) -> Vn`, `sp_vn_x86()` / `sp_vn_x64()` (named by width to prevent the silent-width-bug), `return_const`, `return_binop`, `return_int_cmp`. Total surface ~10 functions, ~80 lines.

### F-016 `make_fn` / `make_fn_with_var` are defined TWICE inside `opt`

- **Category:** Duplication & unification
- **Crate(s):** `opt`
- **Location:** `crates/opt/src/test_support.rs:8-40`, `crates/opt/tests/common/mod.rs:21-51`
- **What:** Identical functions in two files. The `src/test_support.rs` copy is `pub(crate)` (used by `mod tests` modules); the `tests/common/mod.rs` copy is `pub` (used by integration tests).
  ```rust
  // src/test_support.rs:
  pub(crate) fn make_fn<F>(f: F) -> Result<BuiltFunctionGraph>
  where F: FnOnce(&mut FunctionBuilder) -> Result<Value> {
      let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
      let region = b.create_region()?;
      b.set_entry_region(region)?;
      b.set_region(region);
      let val = f(&mut b)?;
      b.build_return(Some(val), &[])?;
      b.build()
  }

  // tests/common/mod.rs (identical body):
  pub fn make_fn<F>(f: F) -> Result<BuiltFunctionGraph>
  where F: FnOnce(&mut FunctionBuilder) -> Result<Value> {
      // ... same body ...
  }
  ```
- **Why:** Rust's per-crate compilation model: `tests/` are separate crates and can't import `pub(crate)` from `src/`. The pragmatic workaround — duplicate — is the standard pattern.
- **Proposed change:** With Option (b) test-consolidation strategy, hoist these to `ir`'s `test-utils` feature. Both `opt/src/test_support.rs` and `opt/tests/common/mod.rs` import them. The local-version drift between `make_fn` and `make_fn_with_var` (the latter has a 1-byte-var variant in `crates/opt/src/known_bits/tests.rs:262`) goes away because there's one canonical implementation.
- **Confidence:** high
- **Risk if applied:** low (mechanical move once the test-utils feature is in place)

### F-017 `reg_vn` is defined in 6 different files with identical body

- **Category:** Duplication & unification
- **Crate(s):** `opt`, `pattern`, `ir`
- **Location:** `crates/opt/tests/common/mod.rs:89`, `crates/opt/src/dead_branch/tests.rs`, `crates/opt/src/redundant_phis/tests.rs`, `crates/opt/src/stack_store/tests.rs`, `crates/opt/src/function_args/tests.rs`, `crates/opt/benches/stack_store.rs`, `crates/pattern/tests/matching/support/graph.rs:19`, `crates/ir/src/builder/tests.rs`
- **What:** Same 5-line body:
  ```rust
  fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
      rsleigh::Vn {
          size,
          addr: rsleigh::VnAddr {
              off,
              space: rsleigh::VnSpace::REGISTER,
          },
      }
  }
  ```
- **Why:** Same per-crate-test-compilation rule as F-016, plus opt has multiple `mod tests` modules in different `src/<pass>/tests.rs` files that can't share `test_support.rs`-level helpers across `mod tests` boundaries. (Actually they CAN via `crate::test_support::reg_vn` if `test_support` exposes it; it currently doesn't.)
- **Proposed change:** Lift to `ir::test_utils::reg_vn` (feature-gated). Drop the 6 local copies.
- **Confidence:** high
- **Risk if applied:** low

### F-018 `sp_vn` is defined in 6 different files with conflicting widths

- **Category:** Correctness / Duplication & unification
- **Crate(s):** `opt`, `pattern`
- **Location:** `crates/opt/tests/common/mod.rs:100` (size = 4), `crates/pattern/tests/matching/support/graph.rs:32` (size = 8), `crates/opt/benches/stack_store.rs` (size = 4), `crates/opt/src/stack_store/tests.rs` (size = 4), `crates/opt/src/redundant_phis/tests.rs` (size = 4 implied)
- **What:** Same name, two widths:
  ```rust
  // opt's: 4-byte (x86 ESP)
  pub fn sp_vn() -> rsleigh::Vn { reg_vn(0x20, 4) }

  // pattern's: 8-byte (x86_64 RSP)
  pub fn sp_vn() -> rsleigh::Vn { reg_vn(0x20, 8) }
  ```
- **Why:** Each test author picked the width matching their test's arch. The same offset (0x20) is used regardless. **Safety hazard:** a refactor that copy-pastes a stack_store test from `opt` (4-byte sp) to `pattern` (8-byte sp) silently changes the sub-register a byte-offset lands in. With the current per-file copy of `sp_vn`, a tester can grep for `sp_vn()` to verify width assumption, but a centralized helper would make the choice explicit.
- **Proposed change:** Replace `sp_vn()` with `sp_vn_x86()` (4-byte) and `sp_vn_x64()` (8-byte) in `ir::test_utils`. Force every callsite to choose explicitly. The 6 copies all become a one-line `use`.
- **Confidence:** high
- **Risk if applied:** low (rename-and-pick-width)

### F-019 `make_strider_x86_64` is duplicated between `strider::orchestrator` test module and `strider::tests::common`

- **Category:** Duplication & unification
- **Crate(s):** `strider`
- **Location:** `crates/strider/src/orchestrator.rs:687-696`, `crates/strider/tests/common/mod.rs:124-126,131-139`
- **What:** Same function body, two homes:
  ```rust
  // src/orchestrator.rs (test module):
  fn make_strider_x86_64() -> Strider {
      let arch = crate::SleighArch::x86_64();
      let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
      let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe).expect("probe sleigh").regs().expect("probe regs");
      Strider::new(arch, regs, crate::CallingConvention::x86_64_systemv_abi()).expect("strider")
  }

  // tests/common/mod.rs:
  pub fn strider_x86_64() -> strider::Strider { strider_for(Arch::X64) }
  pub fn strider_for(arch: Arch) -> strider::Strider {
      // ... same body, parameterized by arch ...
  }
  ```
  Function names differ by `make_` prefix; the strider review's F-029 outcome ("Applied: `tests::common::strider_x86_64()` / `strider_for(arch)` added") added the test side; the orchestrator-side `make_strider_x86_64` was left in.
- **Why:** Inside-the-crate `mod tests` can't reach `tests/common/mod.rs` (separate compilation unit) but COULD reach a `pub(crate)` helper exposed inside the strider lib's own `cfg(test)` module.
- **Proposed change:** Add a `pub(crate) fn make_strider_x86_64() -> Strider` to `crates/strider/src/lib.rs` under `#[cfg(test)]` and have `tests/common/mod.rs` import via `use strider::make_strider_x86_64;` — wait, that still needs feature-gating since `tests/common` is a separate crate. Cleaner: keep the duplication but mark the orchestrator test version as a re-export of `tests/common`'s logic. Or leave alone; the duplication is contained to one file each.
- **Confidence:** medium (depends on the test-utils-feature decision)
- **Risk if applied:** low

### F-020 `crates/strider/tests/common/mod.rs` is 800+ LOC, mostly 15 per-arch `__scan_ignore_<arch>!` macros

- **Category:** Simplification
- **Crate(s):** `strider`
- **Location:** `crates/strider/tests/common/mod.rs:448-823`
- **What:** 15 nearly-identical `macro_rules! __scan_ignore_<arch>!` definitions, each ~25 lines. Snippet:
  ```rust
  #[doc(hidden)]
  #[macro_export]
  macro_rules! __scan_ignore_x86 {
      ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
       { X86: $reason:literal $(, $($_rest:tt)*)? }) => {
          #[test] #[ignore = $reason]
          fn $fn() {
              let g = $crate::common::analyze($crate::common::Arch::X86, $case, $fn_name);
              $assert(&g);
          }
      };
      ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident,
       { $_skip:ident: $_r:literal $(, $($rest:tt)*)? }) => {
          $crate::__scan_ignore_x86!($fn:ident, $case, $fn_name, $assert, { $($($rest)*)? });
      };
      ($fn:ident : ident, $case:literal, $fn_name:literal, $assert:ident, { $(,)? }) => {
          #[test]
          fn $fn() {
              let g = $crate::common::analyze($crate::common::Arch::X86, $case, $fn_name);
              $assert(&g);
          }
      };
  }
  ```
  Repeated 15 times for X86, X64, Aarch64, Aarch64Be, Arm, ArmBe, ArmThumb, Mips32le, Mips32be, Mips64le, Mips64be, Ppc32be, Ppc32le, Ppc64be, Ppc64le.
- **Why:** The comment in the file admits this: "Defining a helper macro that takes these as arguments would deepen the macro recursion (and hit the recursion limit on long-chain ignore lists). Since the per-arch pattern is mechanical, we keep it explicit per arch — same shape as the existing ones." So the duplication is deliberate. But 400 LOC of macro boilerplate to support per-arch ignore lists is a heavy abstraction for what is fundamentally "skip this test on these archs".
- **Proposed change:** Two paths:
  - (a) Replace the macros with a single `Vec<Arch>`-of-skips parameter on `per_arch_test!`. The loop emits one `#[test]` per arch and decides at runtime whether to call `panic!("ignored")`. Trade-off: lose `#[ignore]` semantics and `cargo test --include-ignored`.
  - (b) Use a `proc_macro` (compile-time) that takes the ignore list as a literal map. Heavyweight but eliminates the 15-arch boilerplate.
  - (c) Leave alone — the boilerplate is mechanical, not buggy, and the comment explains the design constraint.
  Recommendation: (c). The cost-benefit of either replacement is bad.
- **Confidence:** high (the analysis)
- **Risk if applied:** N/A — recommend leaving alone

### F-021 `current_anchor_after_opt` / `anchor_value_input` near-duplicates inside `crates/strider/tests/common/tier2_helpers/orchestrator.rs`

- **Category:** Duplication & unification
- **Crate(s):** `strider`
- **Location:** `crates/strider/tests/common/tier2_helpers/orchestrator.rs`
- **What:** The strider review's Summary (line 84) noted "Two helpers (`current_anchor_after_opt` / `anchor_value_input`) in `tests/common/tier2_helpers/orchestrator.rs` have the same body; one is `pub(super)`, the other `pub` — the comment says 'local copy because the existing `current_anchor_after_opt` is private.'" The strider review's F-028 outcome was "Applied: `current_anchor_after_opt` merged into `anchor_value_input`". A subsequent search of the worktree confirms this was applied — only `anchor_value_input` remains. **No new finding here; flagged for completeness.**
- **Why:** Already addressed; this entry is a cross-reference for the report's own consistency.
- **Proposed change:** None. Verify the strider review's apply outcome.
- **Confidence:** high
- **Risk if applied:** —

### F-022 ~50 sites manually inline `FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)`

- **Category:** Duplication & unification
- **Crate(s):** `cfg`, `ir`, `opt`, `pattern`, `pcode-lift`, `strider`
- **Location:** 22 distinct `.rs` files (per `grep -rln`)
- **What:** Same 6-argument call, ~48 occurrences:
  ```rust
  FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
  ```
- **Why:** This is "the empty function": no tracked vars, no calling convention plumbing, no stack pointer, no ret-stack-pop. It's the most common shape in tests. Some sites add `?` or `.unwrap()`; some `expect("FunctionBuilder::new_raw")`. Logically all the same.
- **Proposed change:** Add `FunctionBuilder::empty() -> Self` (or `FunctionBuilder::new_empty() -> Result<Self>`) as a public method on `FunctionBuilder` in `ir`. Saves 6 args × 48 sites = ~288 LOC of test boilerplate. This is genuinely a public API improvement, not just test infra.
- **Confidence:** high
- **Risk if applied:** low (additive; existing call sites can adopt incrementally)

## C. Workspace hygiene

### F-023 Workspace `Cargo.toml` declares `rustc-hash` as a workspace dep but only `opt`'s `Cargo.toml` references it

- **Category:** Dead code (deps)
- **Crate(s):** workspace
- **Location:** `Cargo.toml:40`, `crates/opt/Cargo.toml:10`
- **What:**
  ```toml
  # Cargo.toml workspace deps:
  rustc-hash = "2"

  # opt/Cargo.toml uses it:
  rustc-hash = { workspace = true }
  ```
  The strider review's F-016 outcome was "Applied: `rustc-hash` removed from `Cargo.toml`" — but only from `crates/strider/Cargo.toml`, not the workspace. opt is the only consumer.
- **Why:** Workspace deps are harmless when only one crate consumes them; cost is the global lock entry. Question is whether to keep the workspace pinning or move to direct.
- **Proposed change:** Either (a) Move to `opt/Cargo.toml` direct: `rustc-hash = "2"` (drop the workspace entry); or (b) Leave the workspace entry — future crates that need a fast hasher pick up the same version automatically. (a) is cleaner; (b) is forward-looking.
- **Confidence:** high
- **Risk if applied:** low (cosmetic)

### F-024 `paste = "1"` is a workspace dep used only by `crates/strider`'s `[dev-dependencies]`

- **Category:** Simplification
- **Crate(s):** workspace
- **Location:** `Cargo.toml:34`, `crates/strider/Cargo.toml:23`
- **What:** Same shape as F-023: workspace declares `paste = "1"`; only strider's dev-deps reference it.
- **Proposed change:** Move to `strider/Cargo.toml` direct dev-dep: `paste = "1"`. Drop the workspace entry.
- **Confidence:** high
- **Risk if applied:** low

### F-025 `tempfile` workspace dep is referenced only by `reader`'s `[dev-dependencies]`

- **Category:** Simplification
- **Crate(s):** workspace
- **Location:** `Cargo.toml:32`, `crates/reader/Cargo.toml:14`
- **What:** Same shape; only reader's dev-deps consume it.
- **Proposed change:** Move to direct dev-dep, drop workspace entry. Future consumers can re-add to workspace.
- **Confidence:** high
- **Risk if applied:** low

### F-026 `opt`'s `criterion` direct version (`0.7`) sits outside the workspace deps even though benches are workspace infrastructure

- **Category:** Readability
- **Crate(s):** workspace, `opt`
- **Location:** `crates/opt/Cargo.toml:13`
- **What:**
  ```toml
  [dev-dependencies]
  criterion = { version = "0.7", features = ["html_reports"] }
  ```
  No corresponding `[workspace.dependencies]` entry. opt is the only crate with benches.
- **Proposed change:** Either (a) leave alone (single consumer) or (b) move to workspace deps proactively for future crates that add benches. (a) is consistent with F-023/F-024/F-025 — single-consumer deps don't pay for the workspace indirection.
- **Confidence:** medium
- **Risk if applied:** low (cosmetic)

### F-027 `pattern`'s `bitflags = "2"` is a direct dep — only consumer in workspace

- **Category:** Readability
- **Crate(s):** `pattern`
- **Location:** `crates/pattern/Cargo.toml:11`
- **What:**
  ```toml
  bitflags = "2"
  ```
  No workspace entry. Direct dep with no version-pinning across the workspace.
- **Proposed change:** Same as F-026 — leave as direct dep until a second consumer appears.
- **Confidence:** medium
- **Risk if applied:** —

### F-028 CLAUDE.md describes `Strider::new(arch, sleigh_regs, cc)` as the entry point in the per-crate description; canonical entry is `strider::run(config)`

- **Category:** Readability (docs drift)
- **Crate(s):** workspace
- **Location:** `CLAUDE.md` strider section
- **What:** From the system reminder context: "**`strider`** — Translates a `Cfg` to a `BuiltFunctionGraph` and drives the indirect-branch fixed-point. `Strider::new(arch, sleigh_regs, cc)` takes a `target::SleighArch`, ..." But the latest CLAUDE.md (also visible) lists `strider::run(config) -> Result<BuiltFunctionGraph>` as the top-level entry. Both descriptions are valid (`Strider::new` is a per-iteration handle; `run` is the top-level orchestrator), but the prose order matters for new readers.
- **Why:** The user noted CLAUDE.md was refreshed twice this session; per-crate `lib.rs` docs may not match. `crates/strider/src/lib.rs:32-34` says: `[`run`] — top-level orchestrator: builds the CFG, lifts to IR, runs the optimiser pipeline, and resolves indirect branches via the tier-2 fixed-point loop`. So strider's `lib.rs` IS up to date. The drift is in CLAUDE.md's prose flow (which describes `Strider::new` first, then `run`).
- **Proposed change:** Reorder CLAUDE.md's strider section to lead with `strider::run(config) -> Result<BuiltFunctionGraph>` as the canonical entry, then describe `Strider::new` as the per-iteration lift driver. Mostly editorial.
- **Confidence:** medium
- **Risk if applied:** low (docs only)

## D. Per-crate findings on the 7 small crates

### pcode-lift

The `pcode-lift` crate factored out the value-producing pcode→IR lifter from `strider`, including the register-aliasing logic in `vn_io.rs`. Public surface: `ValueLifter::new` / `lift`, `Result`, plus the `pub mod {error, value, vn_io}`. Most files are clean and recent. Three issues surface.

#### F-029 `vn_io.rs` doc cites deleted `ErrorKind::UnsupportedVnSpace` / `NoRegisterContainer`

- **Category:** Correctness (docs)
- **Crate(s):** `pcode-lift`
- **Location:** `crates/pcode-lift/src/vn_io.rs:124-126`
- **What:**
  ```rust
  /// # Errors
  ///
  /// Returns [`ErrorKind::UnsupportedVnSpace`] if `reg` is not in a
  /// fixed-offset space (REGISTER or UNIQUE).  Returns
  /// [`ErrorKind::NoRegisterContainer`] if no variable in the builder
  /// covers `reg`'s byte range — this should never happen because every
  /// varnode at least contains itself.
  ```
  Both `ErrorKind` variants are deleted post-anyhow.
- **Proposed change:** Replace doc-link `[…]` brackets with prose: "Returns an error if `reg` is not in a fixed-offset space (REGISTER or UNIQUE), or if no variable in the builder covers `reg`'s byte range."
- **Confidence:** high
- **Risk if applied:** low

#### F-030 `vn_io.rs` carries 6+ `BUG-9` references

- **Category:** Readability
- **Crate(s):** `pcode-lift`
- **Location:** `crates/pcode-lift/src/vn_io.rs:380-451`
- **What:**
  ```rust
  // ── BUG-9 regression tests for positioned reg-mask ────────────────────────────
  ...
  let v0_upper8 = reg_at(8, 8); // upper 8 bytes (the BUG-9 hot spot)
  ...
  "BUG-9 regression check: container_mask must NOT be the upper-half mask"
  ```
- **Proposed change:** Either prefix the references with descriptive context ("AArch64 SIMD upper-half write regression") or strip the codename. The first is preferred — keeps test traceability without requiring the bug ledger.
- **Confidence:** high
- **Risk if applied:** low

#### F-031 `float_type_from_vn` mirrors the `TryFrom<u32> for NodeOutputType` integer branch for floats

- **Category:** Duplication
- **Crate(s):** `pcode-lift`, `ir`
- **Location:** `crates/pcode-lift/src/value/float.rs:22-29`, `crates/ir/src/node/output_type.rs:208-223`
- **What:**
  ```rust
  // pcode-lift's float-only switch:
  pub(super) fn float_type_from_vn(vn: &rsleigh::Vn) -> Result<NodeOutputType> {
      match vn.size {
          4 => Ok(NodeOutputType::F32),
          8 => Ok(NodeOutputType::F64),
          10 => Ok(NodeOutputType::F80),
          n => Err(anyhow!("unsupported float varnode size {n} bytes (expected 4 or 8)")),
      }
  }

  // ir's TryFrom<u32> for the integer side:
  fn try_from(value: u32) -> crate::error::Result<Self> {
      match value {
          1 => Ok(Self::U8),
          2 => Ok(Self::U16),
          4 => Ok(Self::U32),
          8 => Ok(Self::U64),
          ...
      }
  }
  ```
- **Why:** ir owns the `NodeOutputType` enum; pcode-lift owns the float-by-byte-size mapping. The float side could live in `ir::NodeOutputType::float_for_byte_size(n: u32) -> Result<Self>`. The integer side already lives there as `TryFrom<u32>`. Mirror them.
- **Proposed change:** Add `NodeOutputType::float_for_byte_size(n: u32) -> Result<Self>` in `ir/src/node/output_type.rs`. pcode-lift's `float_type_from_vn` becomes a one-liner.
- **Confidence:** medium (low priority cleanup)
- **Risk if applied:** low

### target

The `target` crate is pure data: architecture descriptors and calling conventions. Public surface: `SleighArch`, `Endianness`, `CallingConvention`, `BuiltCallingConvention`, plus the 9 `SleighArch::*()` and 7 `CallingConvention::*()` presets. Tests at `crates/target/tests/arch_smoke.rs` (small) plus a 657-line `mod tests` in `calling_convention.rs` (large).

#### F-032 `vn_for_name` doc cites deleted `ErrorKind::UnknownRegName`

- **Category:** Correctness (docs)
- **Location:** `crates/target/src/calling_convention.rs:7,435`
- **What:**
  ```rust
  /// Resolves a single Sleigh register name to its [`rsleigh::Vn`], or returns
  /// [`ErrorKind::UnknownRegName`] if the name is not known.
  ...
  /// Returns [`ErrorKind::UnknownRegName`] if any register name listed in
  /// this convention (including the stack pointer) does not resolve against
  /// `sleigh_regs`.
  ```
- **Proposed change:** Strip `[…]` brackets; replace with prose ("Returns an error if the name is not known.").
- **Confidence:** high
- **Risk if applied:** low

#### F-033 `target/src/calling_convention.rs` contains a 657-line `mod tests`

- **Category:** Readability
- **Location:** `crates/target/src/calling_convention.rs:466-1121`
- **What:** 657 lines of tests live in the same file as the production code (~465 lines). The tests-to-prod ratio is healthy but the file is now ~1100 LOC. Two ignored diagnostic tests (`dump_mips_register_names`, `probe_float_regs`) padd the module further.
- **Proposed change:** Extract `mod tests` to `crates/target/src/calling_convention/tests.rs` (and rename `calling_convention.rs` to `calling_convention/mod.rs`). Standard `cfg(test)`-mod-in-separate-file pattern. Reduces in-file noise.
- **Confidence:** medium (cosmetic)
- **Risk if applied:** low

#### F-034 `target::error::Result<T>` has zero external callers (cross-ref F-001)

- See F-001.

### reader

The `reader` crate provides ELF loading + `MemRegion` / `MemRegionsLookupTable` / `ReadOnlyMemory`. Public surface: `load_elf`, `ElfFileMemReader`, `ReadOnlyMemory`, `MemRegion`, `MemRegionsLookupTable`, plus 6 ELF segment/section converters. Tests: `crates/reader/tests/{elf_smoke,elf_reader,elf_converters,load_elf,mem_region}.rs` plus `tests/common/{elf_fixture,reader_contract,mod}.rs`.

#### F-035 `MemRegion::new`'s overflow check uses `data.len() as u64`

- **Category:** Correctness (low confidence)
- **Location:** `crates/reader/src/lib.rs:82-88`
- **What:**
  ```rust
  pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
      let len = data.len() as u64;
      start_addr
          .checked_add(len)
          .ok_or_else(|| anyhow::anyhow!("region at {start_addr:#x} with length {len} would overflow u64"))?;
      Ok(Self { start_addr, data })
  }
  ```
  `data.len()` is `usize`; cast to `u64` is safe on 64-bit but UB on 128-bit (theoretical). Probably a non-issue in practice.
- **Proposed change:** `let len = u64::try_from(data.len()).map_err(...)`. Defensive, low-priority.
- **Confidence:** low
- **Risk if applied:** low

#### F-036 `tests/elf_converters.rs:281-346` doc comments cite deleted `ErrorKind::Object(_)` / `ErrorKind::RegionOverflow`

- **Category:** Correctness (docs)
- **Location:** `crates/reader/tests/elf_converters.rs:281,344,346`
- See F-008 — bundled with the broader stale-`ErrorKind` cleanup.

#### F-037 `reader::error::Result<T>` has zero external callers (cross-ref F-001)

- See F-001.

### dot

The `dot` crate is the smallest production-only crate — 470 LOC of `DotEmitter`, `DotStyle`, `GraphDotDumper` trait, `GraphDot` wrapper. Public surface is small and clean.

#### F-038 `dump_as_html` doc cites deleted `ErrorKind::DotDumpError` and `ErrorKind::IoError`

- **Category:** Correctness (docs)
- **Location:** `crates/dot/src/lib.rs:454-455`
- **What:**
  ```rust
  /// # Errors
  /// - [`ErrorKind::DotDumpError`] propagated from the dumper.
  /// - [`ErrorKind::IoError`] if writing `out_path` fails.
  pub fn dump_as_html(&self, out_path: impl AsRef<Path>) -> anyhow::Result<()> {
  ```
- See F-008.

#### F-039 `dot::error::Result<T>` has zero external callers (cross-ref F-001)

- See F-001.

### graphwalk

The `graphwalk` crate provides `GraphRef` / `PredGraphRef` traits and `PreOrder` / `PostOrder` walks. `no_std` compatible. Public surface: 11 types/traits (`GraphRef`, `PredGraphRef`, `VisitTracker`, `NopTracker`, `WalkPhase`, `PreOrderContext`, `PreOrder`, `PostOrderContext`, `PostOrder`, `TreePreOrder`, `TreePostOrder`) + 2 free functions (`entity_preorder`, `entity_postorder`).

#### F-040 `PostOrder` and friends are pub but used only by graphwalk's own tests + `graphmock`

- **Category:** Dead code
- **Crate(s):** `graphwalk`
- **Location:** `crates/graphwalk/src/lib.rs`
- **What:** Workspace-wide grep for `PostOrder`, `entity_postorder`, `WalkPhase`, `TreePreOrder`, `TreePostOrder`, `NopTracker`, `VisitTracker` outside `crates/graphwalk/` and `crates/graphmock/`: zero hits. Production code (`crates/ir/src/walk.rs`) only uses `PreOrder` + `entity_preorder`.
- **Why:** The crate's API surface anticipated future post-order users that never materialised. The `graphmock` test crate exercises `GraphRef`/`PredGraphRef` for graphwalk's own tests but doesn't hand them to production code.
- **Proposed change:** Either (a) demote `PostOrder` / `TreePreOrder` / `TreePostOrder` / `NopTracker` to `pub(crate)` (graphwalk's own integration tests stay in the same crate, so they can keep using them via `graphwalk::` paths through `mod` exports — but `pub(crate)` would break the integration tests since `tests/` are separate crates); or (b) leave alone — the API is small, the unused surface is quiet, and removing it is purely future-proofing.
- **Confidence:** high (analysis)
- **Risk if applied:** medium (the demote breaks integration tests)
- **Recommendation:** leave alone, document the analysis.

#### F-041 `entity_preorder` / `PreOrder` are the only graphwalk types reachable from production code

- **Category:** Dead code (analysis)
- **Crate(s):** `graphwalk`, `ir`
- **Location:** `crates/ir/src/walk.rs:43-114`
- **What:** ir's `walk.rs` uses `graphwalk::PreOrder` + `graphwalk::entity_preorder` and exports type aliases. Search shows no other production uses.
- **Why:** Cross-references F-040. The crate's "small + clean" feel is partly because the half that's actually used is small.
- **Proposed change:** None. Document that `graphwalk` is on a "stable and feature-complete" trajectory.
- **Confidence:** high
- **Risk if applied:** —

### entity-utils

The `entity-utils` crate provides `DenseEntitySet<E>` and `Worklist<E>` over `cranelift_entity::EntityRef`. `no_std` compatible. Public surface: 2 types + 1 iter helper. Both extensively tested in-crate.

#### F-042 No findings; the crate is small, well-tested, well-documented

The crate has no obvious duplications or dead code. Workspace-wide grep shows healthy use:
- `DenseEntitySet`: used by `graphwalk`, `ir`, `pattern`, `opt`.
- `Worklist`: used by `opt::worklist::WorkSet` (a thin wrapper).

The `Iter::FusedIterator` impl is non-trivial and pinned by tests. The `FromIterator<E>` and `Extend<E>` impls match standard library conventions. No findings.

### graphmock

The `graphmock` crate is 283 LOC: a single `Graph` type backed by a `PrimaryMap` + name lookup, plus the `graph(input: &str)` DSL parser. Used only by `graphwalk`'s integration tests.

#### F-043 `graphmock` is a 283-LOC workspace crate with one consumer

- **Category:** Simplification (workspace structure)
- **Crate(s):** workspace, `graphmock`, `graphwalk`
- **Location:** `crates/graphmock/`
- **What:** `graphmock` is a workspace member with `[dependencies] graphwalk = ...`. It's referenced by exactly one file outside its own `src/`: `graphwalk/Cargo.toml`'s `[dev-dependencies]`. Production code never reaches it.
- **Why:** The original split (graphwalk + graphmock) was done so graphwalk's own integration tests could live in `tests/` (a separate crate) and import a `Graph` impl. The DSL parser (`graph("a -> b\nb -> c")`) is a clean abstraction that's quoted in 6 tests.
- **Proposed change:** Two paths:
  - (a) Keep as-is. The boundary is clean and the workspace member overhead is minimal.
  - (b) Move graphmock's contents to `crates/graphwalk/tests/common/graph.rs` (a `mod common;` shared test module). Drop the workspace member. graphwalk's integration tests import `mod common; use common::{Graph, graph};`. **Cost:** loses the standalone `cargo test -p graphmock` ability + the panic-on-malformed-input contract becomes a graphwalk test artefact.
  - Recommendation: (a). graphmock as a workspace member is mildly over-structured but harmless and the API is clean.
- **Confidence:** high (analysis)
- **Risk if applied:** medium (b reorganises a test fixture)

#### F-044 `graphmock::graph` deliberately panics on malformed input

- **Category:** Readability
- **Location:** `crates/graphmock/src/lib.rs:96-119`
- **What:**
  ```rust
  /// # Panics
  ///
  /// Panics if a non-blank line does not contain exactly one `->` separator. This
  /// helper is test-only; the input is a hard-coded literal in callers, so a
  /// malformed line is a programmer error rather than a runtime condition.
  #[must_use]
  pub fn graph(input: &str) -> Graph {
      ...
      #[allow(clippy::panic)]
      let (preds, succs) = line
          .split_once("->")
          .unwrap_or_else(|| panic!("graphmock: line missing `->`: {line:?}"));
      ...
  }
  ```
  Per the workspace's `clippy::panic = "deny"`, the panic is explicitly opted-out via `#[allow(clippy::panic)]`. The lib.rs intro doesn't say "this crate panics" — only the function-level doc does.
- **Proposed change:** Add to lib.rs intro: "**This is a test-only crate**; `graph` panics on malformed input rather than returning a `Result`." Editorial.
- **Confidence:** medium
- **Risk if applied:** low

#### F-045 `graphmock` includes 7 self-tests of its own DSL

- **Category:** Readability
- **Location:** `crates/graphmock/src/lib.rs:170-283`
- **What:** The `mod tests` covers `simple_graph`, `diamond`, `loop_graph`, `fan_out_and_fan_in`, `self_loop`, `name_recurrence_resolves_to_same_id`, plus 3 panic tests (`empty_succ_token_panics`, `empty_pred_token_panics`, `trailing_comma_panics`).
- **Why:** The crate's name suggests "fixture" but it carries 113 LOC of self-tests covering its own DSL. Reasonable: the DSL is parsed input, and parse correctness deserves coverage. Cross-references F-043's recommendation to keep as-is.
- **Proposed change:** None.
- **Confidence:** high
- **Risk if applied:** —

### Cross-cutting on the 7 small crates

#### F-046 4 of 7 small crates carry a 3-line `error.rs` (cross-ref F-001)

- See F-001.

#### F-047 6 of 7 small crates' `lib.rs` opens with the same `#![cfg_attr(test, allow(...))]` block

- **Category:** Readability
- **Crate(s):** `pcode-lift`, `target`, `reader`, `dot`, `entity-utils`, `graphmock`, `ir`, `opt`, `pattern`, `strider` (10 of 12 crates)
- **Location:** Each crate's `src/lib.rs` first 9 lines
- **What:** Identical block:
  ```rust
  #![cfg_attr(
      test,
      allow(
          clippy::panic,
          clippy::unwrap_used,
          clippy::expect_used,
          clippy::unreachable
      )
  )]
  ```
- **Why:** Tests legitimately need `unwrap`, etc. The workspace's `[lints]` table doesn't have a built-in way to relax lints in test cfg. The cfg_attr block is the standard workaround.
- **Proposed change:** Could move to a workspace-level `[lints.clippy]` with each lint as `level = "deny", check-cfg = ...` — but `lints.clippy` doesn't support `cfg(test)` selectivity. Realistic answer: leave the boilerplate alone. The 9-line block is mechanical and not a maintenance hazard. Document the pattern in `CLAUDE.md` as the workspace convention so new crates know to copy it.
- **Confidence:** high (analysis)
- **Risk if applied:** —
- **Recommendation:** leave alone; document.

## E. Algorithmic-pattern analysis (focused second pass)

The first cross-crate read pass focused on textual / surface duplication.  A second focused read pass deep-mined the workspace for *algorithmically* similar code expressed differently across sites.  Full analysis at [reviews/algorithmic-patterns-r6.md](algorithmic-patterns-r6.md) (752 lines).  Headlines folded in here so the cross-crate review is the single source of truth for the apply pass.

The deep pass surveyed 9 patterns: 3 "yes — clear win", 3 "maybe — needs more thought", 3 "looks-like-duplication-but-isn't".  IDs use `P-NNN` (Pattern) to distinguish from `F-NNN` findings.

### Critical finding caught by this pass

**P-001 surfaces a silent regression of opt-r6 F-013.**  The `replace_output_uses` local helper at `crates/opt/src/redundant_phis/mod.rs:11-23` was deleted in commit `6255d10` ("opt-crate-r6 batch A cleanup", marked Applied in the opt-r6 outcomes table) but was re-introduced by the anyhow-conversion merge `ebec8eb`.  The opt-r6 outcomes table claims F-013 was applied; current code does NOT match.  Verified by grep — three call sites in `redundant_phis/mod.rs` still reference `replace_output_uses(...)`.

A second site at `crates/opt/src/call_other_elide/mod.rs:131-142` inlines two back-to-back `output_use_cursor` loops that should call `BuiltFunctionGraph::replace_all_uses` directly.

**Implication beyond P-001:** earlier review outcomes-tables should be re-verified post-anyhow-merge, since silent reverts are possible when destructive merges land on top of cleanup work.

### Yes — clear wins (3 patterns)

#### P-001 — Manual `output_use_cursor` loops re-implementing `replace_all_uses`
**Sites:** `crates/opt/src/redundant_phis/mod.rs:11-23` (3 callers) + `crates/opt/src/call_other_elide/mod.rs:131-142` (inlined ctrl + mem).
**Fix:** drop `replace_output_uses`; route all 5 sites through the existing `Graph::replace_all_uses`.  Mechanical.

#### P-002 — Memory-chain walker (3-way)
**Sites:** `crates/opt/src/function_args/mod.rs::mem_chain_is_dirty`, `crates/opt/src/stack_load_forward/mod.rs::probe`, `crates/opt/src/stack_load_forward/mod.rs::find_stack_stored_value_at_offset`.
**Status:** the same finding as opt's deferred F-014 (the `step_through_*` helpers in `sp_expr.rs` are what landed; the three top-level walks still differ).  Real sharable backbone if you're willing to accept a per-site reduction callback.

#### P-005 — `preorder().filter(matches!(NodeKind == X))` (~15 sites)
**Sites:** every pass that indexes by kind: `call_other_elide`, `function_args`, `stack_store::call_args`, `redundant_phis`, plus 7 test-helper `count`-style filters.
**Fix:** add `Graph::preorder_kind<P>(P)` / `BuiltFunctionGraph::preorder_kind` accepting `impl Fn(&NodeKind) -> bool`.  Each call site shrinks to one line.  ~15 LOC saved across the workspace + consistent vocabulary.

### Maybe — needs more thought (3 patterns)

#### P-003 — Cycle-guarded recursive walks
**Sites:** `crates/opt/src/sp_expr.rs::{decompose_sp, decompose_sp_inner, decompose_sp_phi}` thread `visiting: &mut FxHashSet<NodeId>`; `crates/opt/src/indirect_branch_resolve/jump_table.rs::walk_control_for_if_bound` threads its own `visited: &mut HashSet<NodeId>`.
**Variation:** "current path" semantics in sp_expr (insert-before-recurse, remove-after) vs full visited set in jump_table.  Different invariants prevent a single abstraction.
**Recommendation:** introduce a small `PathGuard` RAII handle that auto-removes on drop for the sp_expr family (3 sites, all in one file).  Don't try to unify with jump_table's visited-set semantics.

#### P-006 — Find-unique-by-kind
**Sites:** `crates/opt/src/indirect_branch_resolve/mod.rs::find_placeholder_return_for_anchor`, `crates/cfg/src/cfg/builder/indirect_resolve.rs::find_unique_return`.
**Variation:** Option-vs-Result return shapes; uniqueness check vs first-match.
**Recommendation:** a generic `ir::walk::find_first_by_kind<F>(graph, entry, predicate)` covers both — but caller still has to wrap with uniqueness check.  Marginal LOC savings.  Defer unless touching either site.

#### P-009 — "Stand up sub-IR + run an opt pipeline"
**Sites:** `crates/cfg/src/cfg/builder/indirect_resolve.rs::resolve_indirect_target` builds an `OptimizerPipeline` and runs it against a freshly-built single-region IR.  Strider's `LoopState::run_stable_only` does similar but for the full function.
**Variation:** sub-IR vs whole-IR scope; constant-fold-only vs full pipeline; different consumers of the result.
**Recommendation:** no immediate consolidation.  The cfg side already lifts the pipeline construction (the cfg review's F-022 fixed it).  Leaving the strider side alone respects the layer boundary.

### Looks-like-duplication-but-isn't (3 patterns)

#### P-004 — Two-phase walks ("probe then realize")
**Sites:** `stack_load_forward::probe`+`realize` use the pattern intentionally to prevent partial-realization on backtrack.  `RedundantPhis::remove_phis` does NOT have an equivalent (it's single-phase).  The intermediate `ResolveShape` is intrinsic to its algorithm.
**Recommendation:** no abstraction.  The pattern is a good practice but the data types are problem-specific.

#### P-007 — Fixed-point loop scaffolding
**Sites:** `OptimizerPipeline::run` (pass-list iteration with `Changed` accumulator), `strider::run`'s `LoopState::step` (multi-way Decision enum), `KnownBits::analyze` (worklist drain).
**Recommendation:** no shared abstraction.  The three loops have semantically distinct convergence checks; sharing the iteration counter wouldn't pay back.  `WorkSet` is already the right helper for the worklist case.

#### P-008 — Phi-predecessor iteration `inputs[1..]`
**Sites:** `RedundantPhis::remove_phis`, `cfg::region::link_region_variables`, `sp_expr::decompose_sp_phi`.
**Recommendation:** no abstraction.  The shape `inputs.skip(1)` is one line; per-pred reductions are too divergent to share.

### Cross-pattern observation

The "yes — clear win" patterns are concentrated in `opt`.  `pattern`, `strider`, `cfg` host fewer algorithmic dups (because they already had per-crate r6 deduplication passes).  The deep pass therefore confirms the per-crate reviews were thorough at their own granularity; the remaining duplication is genuinely cross-cutting.

## Files reviewed

| File | Status | Notes |
| --- | --- | --- |
| `crates/cfg/src/lib.rs` | Read | Re-exports + test_api hidden |
| `crates/cfg/src/error.rs` | Read | Trivial alias (F-001/F-011) |
| `crates/cfg/src/cfg/dot.rs` | Read | `vn_to_name` (F-002) |
| `crates/cfg/src/cfg/builder/region_builder.rs` | Read | `is_branch_tail_call_nocheck` (F-005) |
| `crates/cfg/src/cfg/builder/indirect_resolve.rs` | Read | sort key (F-004) |
| `crates/cfg/src/cfg/types.rs` | Read | RegionTerminator |
| `crates/cfg/src/cfg/query.rs` | Read | stale ErrorKind ref |
| `crates/cfg/tests/common/mod.rs` | Read | Test scaffolding |
| `crates/cfg/tests/common/synthetic.rs` | Read | `make_builder` family |
| `crates/cfg/tests/common/real_binary.rs` | Read | Binary loader |
| `crates/cfg/tests/common/assertions.rs` | Read | CFG-shape assertions |
| `crates/dot/src/lib.rs` | Read | DotStyle, DotEmitter, GraphDot |
| `crates/dot/src/error.rs` | Read | Trivial alias |
| `crates/dot/tests/style.rs` | Read | DotStyle pinning |
| `crates/dot/tests/emitter.rs` | Skim | Emitter pinning |
| `crates/entity-utils/src/lib.rs` | Read | re-exports |
| `crates/entity-utils/src/set.rs` | Read | DenseEntitySet |
| `crates/entity-utils/src/worklist.rs` | Read | Worklist |
| `crates/graphmock/src/lib.rs` | Read | Mock graph + DSL |
| `crates/graphwalk/src/lib.rs` | Read | Pre/PostOrder |
| `crates/graphwalk/tests/preorder.rs` | Read | PreOrder fixture tests |
| `crates/graphwalk/tests/postorder.rs` | Read | PostOrder fixture tests |
| `crates/ir/src/lib.rs` | Read | Re-exports |
| `crates/ir/src/error.rs` | Read | Documented alias |
| `crates/ir/src/dot/label.rs` | Read | `vn_to_name` (F-002) |
| `crates/ir/src/dot/tests.rs` | Read | probe_sleigh helper (F-014) |
| `crates/ir/src/node/output_type.rs` | Read | `TryFrom<u32>` |
| `crates/ir/src/node_signature.rs` | Skim | Slot constants |
| `crates/ir/tests/common/mod.rs` | Read | `return_const`/`return_binop` |
| `crates/opt/src/lib.rs` | Read | RegionIrCache stale (F-006) |
| `crates/opt/src/error.rs` | Read | Trivial alias (F-001) |
| `crates/opt/src/pipeline.rs` | Read | OptimizerPipeline + traits (F-013) |
| `crates/opt/src/test_support.rs` | Read | `make_fn`/`make_fn_with_var` (F-016) |
| `crates/opt/src/indirect_branch_resolve/mod.rs` | Skim | ResolvedTargets export |
| `crates/opt/tests/common/mod.rs` | Read | `make_fn`/`reg_vn`/`sp_vn` (F-016/17/18) |
| `crates/opt/benches/stack_store.rs` | Skim | reg_vn/sp_vn dup (F-017/18) |
| `crates/pattern/src/lib.rs` | Read | Re-exports |
| `crates/pattern/src/error.rs` | Read | Sentinel structs |
| `crates/pattern/src/macros.rs` | Skim | Internal macros |
| `crates/pattern/tests/matching/support/mod.rs` | Read | Re-exports |
| `crates/pattern/tests/matching/support/graph.rs` | Read | Tb DSL (F-018: sp_vn width) |
| `crates/pattern/tests/matching/support/shapes.rs` | Skim | Pre-built fixtures |
| `crates/pcode-lift/src/lib.rs` | Read | ValueLifter |
| `crates/pcode-lift/src/error.rs` | Read | Trivial alias |
| `crates/pcode-lift/src/vn_io.rs` | Read | Register aliasing (F-029/30) |
| `crates/pcode-lift/src/value/mod.rs` | Read | Opcode dispatch |
| `crates/pcode-lift/src/value/arithmetic.rs` | Read | Int arith |
| `crates/pcode-lift/src/value/boolean.rs` | Read | Bool ops |
| `crates/pcode-lift/src/value/cast.rs` | Read | Cast/slice |
| `crates/pcode-lift/src/value/float.rs` | Read | Float ops + `float_type_from_vn` (F-031) |
| `crates/pcode-lift/src/value/integer.rs` | Read | Copy/extend |
| `crates/pcode-lift/src/value/mem_load.rs` | Read | `decode_space_id` (F-003) |
| `crates/pcode-lift/src/value/misc_value.rs` | Read | Opaque ops |
| `crates/pcode-lift/tests/value_lifter.rs` | Skim | E2E tests |
| `crates/reader/src/lib.rs` | Read | MemRegion + ReadOnlyMemory |
| `crates/reader/src/error.rs` | Read | Trivial alias |
| `crates/reader/src/elf.rs` | Read | ELF backend |
| `crates/reader/tests/common/mod.rs` | Read | Test re-exports |
| `crates/reader/tests/common/elf_fixture.rs` | Read | Synthetic ELF builder |
| `crates/reader/tests/common/reader_contract.rs` | Read | Backend-agnostic asserts |
| `crates/strider/src/lib.rs` | Read | Re-exports |
| `crates/strider/src/orchestrator.rs` | Read | run + LoopState (F-005/19) |
| `crates/strider/src/strider/mod.rs` | Read | IrStrider |
| `crates/strider/src/strider/pipeline.rs` | Read | sort key (F-004) |
| `crates/strider/src/strider/insn/mod.rs` | Read | `decode_space_id` (F-003) |
| `crates/strider/src/strider/insn/control.rs` | Skim | Switch ladder |
| `crates/strider/src/strider/vn_io.rs` | Read | ValueLifter wrapper |
| `crates/strider/src/indirect_resolve_tier2/mod.rs` | Read | Re-exports |
| `crates/strider/src/indirect_resolve_tier2/classify.rs` | Read | Classifier shim |
| `crates/strider/src/indirect_resolve_tier2/inplace.rs` | Read | apply_* shim |
| `crates/strider/src/rewrite.rs` | Read | GraphRewriter |
| `crates/strider/tests/common/mod.rs` | Read | per_arch_test! macros (F-019/20) |
| `crates/strider/tests/common/tier2_helpers/*.rs` | Skim | Tier-2 fixtures (F-021) |
| `crates/target/src/lib.rs` | Read | Re-exports |
| `crates/target/src/error.rs` | Read | Trivial alias |
| `crates/target/src/arch.rs` | Read | SleighArch presets |
| `crates/target/src/calling_convention.rs` | Read | CC presets + tests (F-032/33) |
| `Cargo.toml` (workspace) | Read | (F-023/24/25) |
| `crates/*/Cargo.toml` | Read | All 12 |
| `CLAUDE.md` | Read | (F-028) |

## Out-of-scope items observed

- **Performance items.** Several deferred from per-crate r6 (opt's F-016 through F-021 batch-4 perf, ir's F-034/35/36 cache-key alloc, cfg's F-035/36 indirect-resolve allocation patterns) are still observable in the code; intentionally not re-flagged here per the plan's out-of-scope rule.
- **Error-handling shape.** The pattern crate's sentinel-struct approach (RewriteSkip / NotBuildable / MissingBinding) is uniquely good for downcast-based test assertions; no need to homogenize against the other crates' bare-anyhow style.
- **Per-crate r6 territory.** Several pcode-lift / target / reader / dot / entity-utils items would benefit from a full per-crate review pass; this report stops at the per-crate-light bar the plan defined.
- **The `RegionTerminator::Switch::target_value` field is always `None`** from the cfg builder (cfg review F-030 outcome was "Skipped — orchestrator already supports the field's None case. Reworded the docstring instead"). Field still exists; still unused. Out of scope here because already-flagged-and-deferred.
- **`crates/strider/tests/common/mod.rs:124-139` and `crates/strider/src/orchestrator.rs:687-696`** test scaffolding duplication discussed in F-019; the proposed fix depends on the test-utils strategy decision.
- **`per_arch_test!` macro**: 800-line scaffolding that's mechanical but not buggy. Recommendation in F-020 is leave alone.

## Outcomes (2026-04-29 — apply pass on `review/cross-crate-r6`)

Branch: `review/cross-crate-r6` (8 commits on top of `feature/ai`).

Final test status: `cargo test --workspace` passes; `cargo clippy --workspace --all-targets` clean.

| ID | Outcome | Notes |
| --- | --- | --- |
| F-001 | Applied | error.rs dropped from cfg/dot/pcode-lift/reader/target; replaced with `pub type Result<T> = anyhow::Result<T>;` directly in lib.rs. opt/ir/pattern keep theirs (real consumer / real types / explanatory doc). |
| F-002 | Applied | `vn_to_display_name` now lives in `ir::dot::label`; cfg + GraphDotDumper delegate. |
| F-003 | Applied | `decode_space_id` + `first_input_or_err` lifted to `pcode_lift`; strider's handle_store + handle_call_other delegate. |
| F-004 | Applied | `pcode_lift::vn_sort_key` shared by cfg::indirect_resolve and strider::pipeline. |
| F-005 | Applied | `cfg::is_addr_tail_call` shared by cfg::Builder and strider::orchestrator. |
| F-006 | Applied | RegionIrCache cites in opt/lib.rs replaced with "strider orchestrator's per-iteration RegionIndex". |
| F-007 | Applied | "F2's trait refactor" → "the Optimizer / OptimizerOnBuilt split" in opt/pipeline.rs. |
| F-008 | Applied | Stale `ErrorKind::*` doc links scrubbed in ir/graph/access.rs (3 sites), pcode-lift/vn_io.rs, dot/lib.rs, target/calling_convention.rs (2 sites), cfg/cfg/query.rs. |
| F-009 | Skipped | ~77 BUG-N codenames in tests; per cross-crate-r6 analysis the cleanup costs more than it saves. Policy: don't introduce new BUG-N. |
| F-010 | Obviated | `opt::AnchorAddr` IS used externally — by 2 sites in `opt/tests/indirect_branch_resolve.rs`. The original analysis missed the integration tests. |
| F-011 | Applied | See F-001. |
| F-012 | Skipped | `EntityMap<E,V>` for entity-utils — premature; current `RegionIndex` doesn't directly benefit (keyed by `NodeOutputId`). Defer until a real consumer appears. |
| F-013 | Skipped | OptimizerOnBuilt blanket-impl pattern is locally clean and architecturally consistent for opt. Per the user-confirmed skip list. |
| F-014 | Applied | `target::SleighArch::probe_regs()` added; 6 sites collapsed (orchestrator test mod, tests/common, tier2 helpers, jump_table_lifting, r1_placeholder, graph_rewriter, pipeline display test). |
| F-015 | N/A | Inventory section, no fix-action. |
| F-016 | Applied | `make_fn` / `make_fn_with_var` consolidated into `ir::test_utils` (feature `test-utils`). Both opt copies replaced with re-exports. |
| F-017 | Applied | `reg_vn` consolidated into `ir::test_utils::reg_vn`. 6 inline copies replaced with `use ir::test_utils::reg_vn`. |
| F-018 | Applied | `sp_vn` split by width: `sp_vn_x86()` (4-byte) and `sp_vn_x86_64()` (8-byte). 6 inline copies updated to choose the explicit width. |
| F-019 | Partial | `make_strider_x86_64` / `strider_for(arch)` both reduced to ~3 lines via `probe_regs()`. The remaining duplication is too small to justify reaching across the test compile-unit boundary. |
| F-020 | Skipped | Per the cross-crate-r6 recommendation: 15 per-arch `__scan_ignore_<arch>!` macros are mechanical, not buggy; either replacement (proc-macro or runtime-skip) costs more than it saves. |
| F-021 | Obviated | Already applied per strider-r6 F-028; only `anchor_value_input` remains in tier2_helpers. |
| F-022 | Applied | `FunctionBuilder::empty()` added as a public IR API. ~77 inline `new_raw(vec![],&[],&[],&[],None,0)` calls converted via sed. |
| F-023 | Applied | `rustc-hash` workspace dep moved to opt's direct dep. |
| F-024 | Applied | `paste` workspace dep moved to strider's direct dev-dep. |
| F-025 | Applied | `tempfile` workspace dep moved to reader's direct dev-dep. |
| F-026 | Skipped | criterion direct dep on opt — single consumer, leave alone. |
| F-027 | Skipped | bitflags direct dep on pattern — single consumer, leave alone. |
| F-028 | Applied | CLAUDE.md strider section reordered: leads with `strider::run(config)` as canonical entry. |
| F-029 | Applied | pcode-lift/vn_io.rs ErrorKind:: doc refs replaced with prose. |
| F-030 | Applied | BUG-9 codename refs in pcode-lift/vn_io.rs replaced with "AArch64 SIMD upper-half hot spot" context. |
| F-031 | Applied | `NodeOutputType::float_for_byte_size` added; pcode-lift's `float_type_from_vn` is now a one-liner delegate. |
| F-032 | Applied | target/calling_convention.rs ErrorKind::UnknownRegName doc refs (2 sites) replaced with prose. |
| F-033 | Applied | calling_convention.rs's 657-line `mod tests` extracted to `target/src/calling_convention/tests.rs`; calling_convention.rs becomes calling_convention/mod.rs. |
| F-034 | Applied | See F-001. |
| F-035 | Skipped | MemRegion::new overflow check on hypothetical 128-bit. Per the user-confirmed skip list. |
| F-036 | Applied | reader/tests/elf_converters.rs ErrorKind doc refs replaced with prose. |
| F-037 | Applied | See F-001. |
| F-038 | Applied | dot/lib.rs ErrorKind doc refs replaced with prose. |
| F-039 | Applied | See F-001. |
| F-040 | Skipped | graphwalk PostOrder/etc demote-to-pub(crate) breaks integration tests; the unused public surface is harmless. Per the cross-crate-r6 recommendation. |
| F-041 | N/A | Analysis-only finding (no fix-action). |
| F-042 | N/A | No findings. |
| F-043 | Skipped | graphmock consolidation. Per the user-confirmed skip list. |
| F-044 | Skipped | graphmock panic doc — minor editorial; deferred. |
| F-045 | N/A | Analysis-only finding (no fix-action). |
| F-046 | Applied | See F-001. |
| F-047 | Skipped | `cfg_attr(test, allow(...))` block — workspace `[lints.clippy]` doesn't support cfg(test) selectivity. Boilerplate is mechanical and not a hazard. Per the cross-crate-r6 recommendation. |

### Algorithmic patterns

| ID | Outcome | Notes |
| --- | --- | --- |
| P-001 | Applied | **Silent regression caught.** `replace_output_uses` was deleted in opt-r6 commit 6255d10 but resurrected by the anyhow-merge ebec8eb. 3 call sites in redundant_phis + 2 inlined cursors in call_other_elide all routed through `Graph::replace_all_uses`. |
| P-002 | Skipped | Memory-chain walker generic backbone — non-trivial refactor; per-site reductions are too divergent in practice to share without a tedious closure-callback API. Documented in cross-crate-r6.md and skipped. |
| P-003 | Skipped | PathGuard RAII for sp_expr.rs — only ONE site actually pairs `visiting.insert/.remove` (`decompose_sp` itself; `decompose_sp_inner` and `decompose_sp_phi` forward the set). RAII is overkill for a single site. |
| P-004 | Skipped | Per the user-confirmed skip list (looks-like-duplication-but-isn't). |
| P-005 | Applied | `BuiltFunctionGraph::preorder_kind<P>(P)` added; 4 production sites converted (call_other_elide, stack_load_forward, function_args, cfg::find_unique_return). |
| P-006 | Partial | Addressed via `preorder_kind`; the find-unique-by-kind shape collapses to `iter.next()` with a uniqueness check. |
| P-007 | Skipped | Per the user-confirmed skip list. |
| P-008 | Skipped | Per the user-confirmed skip list. |
| P-009 | Skipped | Per the user-confirmed skip list. |

### Silent regressions caught (besides P-001)

None. The opt-r6 outcomes-table re-verification scanned F-007/F-008/F-009/F-010/F-026/F-029/F-037 — all clean.

### Surprising findings

- **F-010 was wrong**: `opt::AnchorAddr` IS used externally by `opt/tests/indirect_branch_resolve.rs`. Integration tests are separate compile units, so they count as external. The cross-crate analysis's grep missed them.
- **F-019's collapse turns out to be cosmetic-only post-`probe_regs()`**: with the boilerplate down to 3 lines per copy, the duplication is too small to justify reaching across the `cfg(test)` ↔ `tests/common` compile-unit boundary.
- **P-003 had only one actual `visiting.insert/.remove` pair**, not the 3 sites the analysis suggested. The other two sites in sp_expr forward the same set without owning its lifecycle, so RAII would be overkill.
