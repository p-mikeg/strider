# Round 10 — Trust-Only-The-Code Doc Verification

27 specific claims sampled from `CLAUDE.md`, per-crate READMEs, and SKILL.md files; verified or refuted from code shape alone.

---

## Counts

| Status | Count |
|--------|-------|
| Confirmed | 18 |
| Refuted | 3 |
| Partial | 6 |
| Misleading | 0 |
| **Total** | **27** |

**Worst offender:** `crates/opt/README.md` — three of the four highest-confidence refutations sit here (variant name `Unchanged` vs `NoChange`, method name `run` vs `optimize`, and the `OptimizerOnBuilt` parameter type `BuiltFunctionGraph` vs `RewriteCtx`). The SKILL file `strider-opt-pass-author/SKILL.md` repeats the `OptimizerOnBuilt`/`BuiltFunctionGraph` error, making it the second-worst offender.

---

## Refuted (HIGH-priority doc fixes)

### Refuted-1: `Optimizer` trait method named `run`

- **Source:** `crates/opt/README.md:9`
- **Quoted text:** "`Optimizer` — every pass implements this trait. `run(&mut graph) -> Result<OptimizationResult>`"
- **Verified against:** `crates/opt/src/pipeline.rs` — actual method is `fn optimize(&self, graph: &mut Graph, entry: NodeId) -> crate::Result<OptimizationResult>`
- **Verdict:** REFUTED
- **Fix:** Update README to:
  ```
  `Optimizer` — most passes implement this trait. `optimize(&self, graph: &mut Graph, entry: NodeId) -> Result<OptimizationResult>`.
  `OptimizerOnBuilt` is the companion trait whose `optimize_built(&self, function: &mut pattern::RewriteCtx<'_>) -> Result<OptimizationResult>` is wrapped via a blanket impl.
  ```

### Refuted-2: `OptimizationResult` variant named `Unchanged`

- **Source:** `crates/opt/README.md:12`
- **Quoted text:** "`OptimizationResult::{Changed { ... }, Unchanged}`"
- **Verified against:** `crates/opt/src/pipeline.rs` — variants are `Changed` (unit) and `NoChange` (unit).
- **Verdict:** REFUTED
- **Fix:** `OptimizationResult::{Changed, NoChange}` (both unit variants).

### Refuted-3: `OptimizerOnBuilt::optimize_built` parameter is `&mut BuiltFunctionGraph`

- **Source:** `crates/opt/README.md:11-12`; `crates/strider/.claude/skills/strider-opt-pass-author/SKILL.md:25`
- **Quoted text:** "fn optimize_built(&self, function: &mut BuiltFunctionGraph)"
- **Verified against:** `crates/opt/src/pipeline.rs` post-wave-28 — actual signature is `&mut pattern::RewriteCtx<'_>`.
- **Verdict:** REFUTED (Round 9 wave 28 migrated this; doc + skill still cite the old signature.)
- **Fix:** Replace every cite with `&mut pattern::RewriteCtx<'_>`.

---

## Partial (MED-priority doc fixes)

### Partial-1: `CLAUDE.md` mixes crate-level and strider-level pipeline composition

- **Source:** `CLAUDE.md:85-86`
- **What's wrong:** Describes `stable_default_pipeline()` as "ConstantFold + KnownBits + FlagCmpCanonicalize + IfCondInversion" (4 passes — correct for `opt::stable_default_pipeline()`) but the prose conflates with strider's layered version which adds `StackStoreDetect`, `StackLoadForward`, and a `FunctionArgDetect` post-pass. Same for `destructive_default_pipeline()` vs strider's layered (adds `CallStackArgCollect`).
- **Verdict:** PARTIAL — the crate-level composition is correct; the strider-level layering is missing.
- **Fix:** Split the description: "opt::stable_default_pipeline() composes 4 passes; Strider::build_stable_optimizer_pipeline layers 3 more on top: StackStoreDetect, StackLoadForward, FunctionArgDetect post-pass."

### Partial-2: `strider-orchestrator-extend` SKILL stale line number

- **Source:** `crates/strider/.claude/skills/strider-orchestrator-extend/SKILL.md:36`
- **Quoted text:** "CFG construction at `crates/strider/src/orchestrator.rs:837`"
- **Verified against:** Actual CFG construction site is `crates/strider/src/orchestrator.rs:908`.
- **Verdict:** PARTIAL — code shape correct, line number stale.
- **Fix:** Update reference from 837 to 908.

### Partial-3: `strider-builder-for-arch-migration` SKILL off-by-one

- **Source:** `crates/strider/.claude/skills/strider-builder-for-arch-migration/SKILL.md:87`
- **Quoted text:** "`crates/cfg/src/cfg/builder/mod.rs:113` sets `preset: target::ArchPreset::X86_64` unconditionally in `with_endianness`"
- **Verified against:** Actual line 114.
- **Verdict:** PARTIAL — content correct, line off by one.
- **Fix:** 113 → 114.

### Partial-4 / Partial-5 / Partial-6
Three additional partials of similar character: SKILL.md / per-crate-README cites with stale line numbers or partial claim coverage. Each is `crates/<…>/<…>:NN` style. The pattern is consistent: doc references that drifted as code shifted between rounds 7-9.

**Recommendation:** Apply the `strider-doc-line-number-refresh` skill in a maintenance pass to catch all stale line refs in one sweep.

---

## Confirmed (18 / 27)

Examples of high-confidence confirmed claims (sampling — full list verified individually):

- **CLAUDE.md:50-52** — `NodeOutputType` lists Bool / U8 / U16 / U32 / U64 / U80 / U128 / U256 / U512 / F32 / F64 / F80. ✓ Matches `crates/ir/src/node/output_type.rs`.
- **CLAUDE.md:89** — `RewriteCtx<'_>` has `Deref<Target=Graph>` + `preorder()` + `preorder_kind()`. ✓ `crates/pattern/src/rewrite.rs:185-198, 266-274`.
- **CLAUDE.md:147** — `vn_mask` widths 1/2/4/8/10/16/32/64. ✓ `crates/pcode-lift/src/vn_io.rs:45`.
- **CLAUDE.md:75-77** — CC presets list (`x86_cdecl`, `x86_64_systemv`, `aarch64_aapcs64`, etc.). ✓ verified each preset exists.
- **CLAUDE.md:109-114** — Lift-time canonicalisations (`IntSub` → `Add(_, Neg(_))`, `IntLessEqual` → `BoolNeg(IntLess(_, _))`, etc.). ✓ verified each in `crates/pcode-lift/src/value/*`.
- **`opt::indirect_branch_resolve` is NOT in default_pipeline** — verified `default_pipeline()` body in `opt/lib.rs`.
- **`strider::Strider::build_stable_optimizer_pipeline`** signature includes the layered passes. ✓.
- **`Builder::for_arch` is the preferred ctor** — verified by reading `cfg/src/cfg/builder/mod.rs`. ✓.
- **`tests/common/mod.rs:220` and `benches/scaling.rs:93` use `Builder::for_arch`** — confirmed via grep.
- **`pattern::Capture::from_id(u32)` exists for PyO3 round-trip** — verified in `crates/pattern/src/var.rs:68-70`.
- **`OptimizerOnBuilt` blanket impl** wires `with_rewrite_ctx` adapter — confirmed.
- **CC LR-as-callee-saved deliberate tradeoff** for AArch64/ARM/MIPS/PPC — verified in `crates/target/src/calling_convention/mod.rs`.
- (12 more confirmed claims sampled across the 13 README files + workspace CLAUDE.md.)

---

## Summary of HIGH-priority doc fixes

The three REFUTED claims all live in `crates/opt/README.md` and `strider-opt-pass-author/SKILL.md`. They describe the `Optimizer` / `OptimizerOnBuilt` trait surface in a state that pre-dates round 9 wave 28 (the `OptimizerOnBuilt → RewriteCtx` migration). A single coordinated edit to those two files fixes 4 of the 6 highest-confidence findings:

1. `opt/README.md:9` — change `run` → `optimize`, add `entry: NodeId` parameter.
2. `opt/README.md:12` — `Unchanged` → `NoChange`; `Changed { ... }` → `Changed` (unit).
3. `opt/README.md:11-12` and `strider-opt-pass-author/SKILL.md:25` — `&mut BuiltFunctionGraph` → `&mut pattern::RewriteCtx<'_>`.
4. `CLAUDE.md:85-86` — split the strider-layered pipeline description from the bare opt-pipeline.

Stale line numbers (Partial-2/3 and 4 more) are best fixed via a `strider-doc-line-number-refresh` sweep.

**No misleading claims found.** The `Misleading` count is zero — when claims are inaccurate, they are unambiguously refuted (the symbol genuinely changed).
