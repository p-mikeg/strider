# Round 8 / 3B — Stale-comment sweep across the strider workspace

Branch: `feature/ai`. Findings derived directly from current source —
no prior round's review files were consulted.

Severity legend: HIGH = user-facing pub-fn doc misleading or doc points
at deleted/non-existent symbol. MED = internal contract drift / wrong
internal path. LOW = cosmetic / historical artefact still readable.

## HIGH

### `Graph::asm_fingerprints` doc lists ghost `IfCase` exempt member
- **Severity:** HIGH (user-facing field doc is wrong about the validator's exempt set).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/mod.rs:80-98`
- **Comment:** `Region nodes (\`ControlState\`, phis, \`Entry\`, \`InitialMemory\`, \`InitialVar\`, \`FunctionArg\`, \`IfCase\`) legitimately stay empty;`
- **Code reality:** `IfCase` is **not** a `NodeKind` (verified by reading `crates/ir/src/node/kind.rs:25-260` — no `IfCase` variant). The real exempt list, in `crates/ir/src/validate/layer_c.rs:178-191`, is `Entry | InitialMemory | InitialVar(_) | FunctionArg{..} | ControlState | MemPhi | VarPhi(_) | ValuePhi | StackStorePhi{..}`. The doc also omits `MemPhi`, `VarPhi`, `ValuePhi`, and `StackStorePhi` while inserting non-existent `IfCase`.
- **Fix:** Replace the parenthetical with `(NodeKind::Entry, InitialMemory, InitialVar, FunctionArg, ControlState, MemPhi, VarPhi, ValuePhi, StackStorePhi)` — matching `asm_fingerprint_exempt` exactly.

### IR README "control flow encoded by `ControlState` / `If` / `IfCase`"
- **Severity:** HIGH (front-page architecture doc references non-existent node kind).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/README.md:43-46`
- **Comment:** `there are no basic blocks at the node level — control flow is encoded by \`ControlState\` / \`If\` / \`IfCase\` nodes whose \`Control\` outputs feed downstream nodes' control inputs, …`
- **Code reality:** `IfCase` is not a `NodeKind`. It is a CFG-level edge label (`cfg::RegionEdgeKind::IfCaseTrue` / `IfCaseFalse`). The IR encodes branch arms via the two `Control` outputs of an `If` node directly.
- **Fix:** Drop `IfCase` from the list — describe `If`'s two `Control` outputs as the branch arms.

### `Strider::build_optimizer_pipeline` doc omits 3 of 6 passes
- **Severity:** HIGH (the canonical pipeline-builder's contract is wrong about what it builds — users reading this miss `FlagCmpCanonicalize`, `IfCondInversion`, and `StackLoadForward`).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:178-188`
- **Comment:**
  ```
  /// 1. All passes from [`opt::default_pipeline`] (constant folding,
  ///    known-bits, redundant-phi, dead-branch).
  /// 2. [`opt::StackStoreDetect`] inside the fixed-point loop, ...
  /// 3. [`opt::CallStackArgCollect`] as a post-pass ...
  /// 4. [`opt::FunctionArgDetect`] as a post-pass ...
  ```
- **Code reality:** `opt::default_pipeline()` (`crates/opt/src/lib.rs:185-194`) actually contains six passes in order: `ConstantFold`, `KnownBits`, **`FlagCmpCanonicalize`**, **`IfCondInversion`**, `RedundantPhis`, `DeadBranchElimination`. Step (1) doc lists only four. Additionally, the impl on line 195 calls `p.add(opt::StackLoadForward::from_convention(...))` between StackStoreDetect and the post passes — **not in the doc list at all**.
- **Fix:** Update step (1) to enumerate all six default-pipeline passes, and insert a new step between (2) and (3) for `StackLoadForward` (or merge it with StackStoreDetect into "convention-aware stack passes").

### `Strider::build_stable_optimizer_pipeline` doc omits FlagCmpCanonicalize + IfCondInversion
- **Severity:** HIGH (sister method to the one above; same omission of canonicalisation passes that are actually run).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:208-218`
- **Comment:**
  ```
  /// Composed of passes whose rewrites survive a later iteration that
  /// adds new phi inputs: `ConstantFold`, `KnownBits`,
  /// `StackStoreDetect`, `StackLoadForward`, and the
  /// `FunctionArgDetect` post-pass.
  ```
- **Code reality:** `opt::stable_default_pipeline()` (`crates/opt/src/lib.rs:106-126`) contains four passes — `ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion` — and `build_stable_optimizer_pipeline` then layers `StackStoreDetect` + `StackLoadForward` + the `FunctionArgDetect` post-pass on top. The doc misses `FlagCmpCanonicalize` and `IfCondInversion` entirely. CLAUDE.md (line "build_stable_optimizer_pipeline — passes whose rewrites survive a later iteration adding new phi inputs (ConstantFold, KnownBits, FlagCmpCanonicalize, IfCondInversion, StackStoreDetect, StackLoadForward …)") matches the impl, not the in-source doc.
- **Fix:** Insert `FlagCmpCanonicalize` and `IfCondInversion` between `KnownBits` and `StackStoreDetect` in the prose.

### `pcode-lift` lib doc says cfg integration is "(planned)"
- **Severity:** HIGH (front-page module doc on a published library claiming a feature is unfinished, when it has shipped).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/lib.rs:23-25`
- **Comment:** `* \`cfg\`, which uses it (planned) to build a stand-alone single-block mini-IR for resolving the targets of indirect branches.`
- **Code reality:** `cfg::indirect_resolve_test_api::resolve_indirect_target_for_test` plus the internal `crates/cfg/src/cfg/builder/indirect_resolve.rs` (line 148, 162) actively build a `pcode_lift::ValueLifter` and lift a single-region mini-IR for indirect-branch resolution. CLAUDE.md confirms this is wired (`pcode-lift … Both \`strider\` (per-region IR translation) and \`cfg\` (single-block mini-IR for indirect-branch resolution) reuse it`).
- **Fix:** Drop the `(planned)` qualifier; describe it as the working second consumer.

### `strider::indirect_resolve` module doc references non-existent `cfg::indirect_resolve` module
- **Severity:** HIGH (the doclink does not resolve — `cfg::indirect_resolve` is private; the public re-export is `cfg::indirect_resolve_test_api`).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/indirect_resolve/mod.rs:1-3`
- **Comment:** `//! IR-level (post-IR) resolver for \`BranchIndirect\` placeholders that //! the cfg-time mini-graph resolver (in \`cfg::indirect_resolve\`) couldn't //! classify.`
- **Code reality:** `cfg::indirect_resolve` does not exist as a public path. The cfg-builder-internal module is `cfg/src/cfg/builder/indirect_resolve.rs` (private — `mod indirect_resolve;`); the only externally-visible name is `cfg::indirect_resolve_test_api` (`crates/cfg/src/cfg/mod.rs:19`).
- **Fix:** `(in \`cfg::indirect_resolve_test_api\`)` — or describe it without a module path: `the cfg-time mini-graph resolver inside the \`cfg\` builder couldn't classify`.

### `strider/examples/strider.rs` looks up a non-existent symbol in a non-existent fixture
- **Severity:** HIGH (the runnable canonical example fails on first execution; the inline expectation message also lies about the symbol it failed on).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/examples/strider.rs:11,30-32`
- **Comment / Code:**
  ```rust
  let binary_path = "fixtures/out/x86/test.elf";
  ...
  .symbol_by_name("struct_test")
  .ok_or("'fib' symbol not found in binary")?
  ```
- **Code reality:** `fixtures/out/x86/test.elf` does not exist (no such file on disk; `ls fixtures/out/x86/` contains `abi.elf`, `arithmetic.elf`, `calls.elf`, `memory.elf`, …). `crates/strider-py/tests/python/conftest.py:62-63` even calls this out: *"This replaces the plan's reference to a non-existent `test.elf` / `struct_test` symbol."* The example also panics with `'fib' symbol not found` when the lookup target was in fact `struct_test`. The Bash-level invocation `cargo run -p strider --example strider` documented in CLAUDE.md will fail.
- **Fix:** Repoint `binary_path` to one of `fixtures/out/x86/{memory,arithmetic,calls}.elf`, repoint `symbol_by_name` to a symbol that exists in that fixture (e.g. `array_sum` in `memory.elf`), and update the error message to name the same symbol the lookup uses.

### Python `PhiPat` builder doc claims it covers VarPhi / MemPhi / ValuePhi
- **Severity:** HIGH (Python-visible class docstring lies about what the class matches — users will write `phi()` expecting it to also match memory/value phis).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:580-583`
- **Comment:** `/// Typed builder for \`VarPhi\` / \`MemPhi\` / \`ValuePhi\` patterns. /// Chain \`.for_vn(vn)\` to constrain the matched VarPhi to a specific /// varnode, and \`.input(idx, p)\` to constrain the value arriving /// from the given predecessor slot.`
- **Code reality:** `PyPhiPat::finalise` (line 597-602) calls `pattern::phi()` (or `phi_for(vn)`), both of which build a **VarPhi-only** `PhiPat`. Separate Python classes `PyMemPhiPat` (line 649) and `PyValuePhiPat` (line 693) exist for the other phi kinds, with their own `mem_phi()` / `value_phi()` constructors (lines 687, 731).
- **Fix:** `Typed builder for VarPhi patterns. (For MemPhi / ValuePhi, use mem_phi() / value_phi() respectively.)`

### Python `Matcher::vn` (== `match.vn`) overstates the producer kinds it covers
- **Severity:** HIGH (Python-visible docstring claims it returns the Vn for `VarPhi` and `FunctionArg` — neither is supported by the Rust impl).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:183-187`
- **Comment:** `/// Recover the matched varnode from \`c\`.  Returns the \`Vn\` /// associated with the captured \`InitialVar\` / \`VarPhi\` / /// \`FunctionArg\` node, or \`None\` when \`c\` doesn't bind such a /// node.`
- **Code reality:** `Match::get_vn` (`crates/pattern/src/matcher/match_result.rs:187-232`) only returns Some for `InitialVar(vn)` and `Call`/`CallOther` clobber slots. There is no `VarPhi` arm, no `FunctionArg` arm. A Python user who calls `match.vn(c)` after `c` was bound by `phi()` or `function_arg(...)` will get `None` despite the docstring's promise.
- **Fix:** Replace with `Returns the Vn for InitialVar bindings, plus Vns for the i-th clobber slot of Call / CallOther bindings; None otherwise.` (Match the Rust doc on `Match::get_vn`.)

## MED

### `is_commutative_int_cmp_op` doc names `LessEqual` / `SlessEqual` as if they were variants
- **Severity:** MED (internal helper — doc references variants that don't exist on `IntCmpOp`).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/commutativity.rs:20-26`
- **Comment:** `/// \`Less\` / \`LessEqual\` / \`Sless\` / \`SlessEqual\` are directional, and \`Sborrow\` encodes signed /// subtraction overflow — all non-commutative, and intentionally excluded.`
- **Code reality:** `IntCmpOp` (`crates/ir/src/ops/op_kinds.rs:30-53`) has only `Equal`, `Sless`, `Less`, `Carry`, `Scarry`, `Sborrow`. `LessEqual` and `SlessEqual` were never variants in the lifted IR (they are lowered to `BoolNeg(Less(_,_))`). The "intentionally excluded" wording suggests these were once variants and got dropped from the commutative set; in fact they were never in the enum at all.
- **Fix:** Replace `Less / LessEqual / Sless / SlessEqual` with `Less / Sless` and drop the now-stale "and \`Sborrow\` encodes signed subtraction overflow" tail-clause (or keep just `Sborrow` as the third non-commutative).

### `opt::indirect_branch_resolve` `Optimizer` impl docstring "running stable pipeline locally"
- **Severity:** MED — small terminology drift.
- **Where:** Multiple, e.g. `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs:255` (`// jump-table and stack-array arms used to call analyze_known_bits`)
- **Comment:** History narrative about "used to" call `analyze_known_bits`.
- **Code reality:** Acceptable historical artefact — flagged as informational, no fix required. (Listed for completeness; do not act.)

### `crates/opt/src/lib.rs` mentions `CallOtherElide` (deleted pass) in pipeline docs
- **Severity:** MED (intentional historical note — calls out the deletion explicitly so callers grepping for the old symbol are redirected to `target::call_other_abi::classify`).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/lib.rs:148-151,181-183` and `/mnt/c/Users/mikeg/Documents/strider/crates/opt/README.md:89-91`
- **Comment:** `/// CallOther no-op handling is now done at construction time in /// \`target::call_other_abi::classify\` — the pre-existing \`CallOtherElide\` /// pass is gone.`
- **Code reality:** Still useful breadcrumb; keep. (Listed only because the task specifically asked to grep for `CallOtherElide`.) — **No fix recommended**, this is good rot-prevention rather than rot.

### `strider-indirect-shape-author` skill cites `cfg::indirect_resolve` (private path)
- **Severity:** MED (skill doc — would mislead a Claude agent following the procedure).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/.claude/skills/strider-indirect-shape-author/SKILL.md:26`
- **Comment:** `2. Confirm tier-1 doesn't classify it. The cfg-time mini-graph in \`cfg::indirect_resolve\` runs the opt pipeline locally;`
- **Code reality:** `cfg::indirect_resolve` is not a path. Real internal path is `crates/cfg/src/cfg/builder/indirect_resolve.rs` (file path) or `cfg::indirect_resolve_test_api` (public test re-export at `crates/cfg/src/test_api.rs:16`).
- **Fix:** `The cfg-time mini-graph in \`crates/cfg/src/cfg/builder/indirect_resolve.rs\` runs the opt pipeline locally;`

### `Strider::run` mentioned in `lib.rs` as the orchestrator entry
- **Severity:** LOW (already correct — verified `pub use orchestrator::{run, RunConfig}` at `crates/strider/src/lib.rs:43`. No issue.)

### `FunctionBuilder::new` docstring in example mentions wrong arity
- **Severity:** MED (developer-visible inline comment in the canonical example).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/examples/graph_creator.rs:31-34`
- **Comment:** `// FunctionBuilder::new takes // (all_vars, arg_regs, callee_saved, ret_regs, stack_ptr_vn, ret_stack_pop). // build_entry() is called automatically inside new().`
- **Code reality:** The example actually calls `FunctionBuilder::new_raw(...)` (line 34), not `FunctionBuilder::new`. The arity comment may match `new_raw`, but the *header* names a different function. Minor — but a reader writing fresh code from this snippet might call `new` with that signature and fail.
- **Fix:** Either change the comment header to `FunctionBuilder::new_raw takes …`, or align the example with whatever the public preferred entry is.

### `ir::lib.rs` doclinks `node::Graph` (does not resolve)
- **Severity:** MED — broken `rustdoc` link in front-page module doc.
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/lib.rs:18,31`
- **Comment:** `//! cached inside [\`node::Graph\`].` and `//! - [\`node::Graph\`] — raw node/edge store`.
- **Code reality:** `Graph` is at `crate::graph::Graph` (re-exported as `ir::Graph` via `pub use graph::Graph` on line 46). `node::` does not export a `Graph`.
- **Fix:** `[\`Graph\`]` (which resolves to the crate-root re-export) or `[\`graph::Graph\`]`.

### `ir/README.md` `NodeOutputType` enum claim omits `U80` / `F80`
- **Severity:** MED (front-page README under-specifies the enum, which the lib.rs doc lists correctly).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/README.md:50`
- **Comment:** `\`NodeOutputType\` (\`Bool\` / \`U8\`–\`U256\` / \`F32\` / \`F64\`).`
- **Code reality:** `NodeOutputType` (`crates/ir/src/node/output_type.rs:19-38`) contains `U80` and `F80` for x87 80-bit extended precision. `lib.rs:36-37` already lists them; README does not.
- **Fix:** Mention `U80` / `F80` (and align with lib.rs).

## LOW

### `strider/src/strider/insn/control.rs` analyzer-era comment
- **Severity:** LOW (old internal-name carry-over).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/output_type.rs:349`
- **Comment:** `register width that the analyzer needs in order to handle x86 floats without erroring at \`analyze_cfg\` setup.`
- **Code reality:** "the analyzer" pre-dates the `strider` rename. Acceptable, but slightly disorienting. Same wording recurs in `crates/ir/src/builder/mod.rs:223`, `crates/ir/src/builder/tests.rs:999`.
- **Fix:** Replace "the analyzer" with "strider" / "the lifter" wherever it appears (`grep -rn "the analyzer" crates/ir/src` gives ~5 hits).

### `pattern/src/pat/mod.rs` "previous overloading on Capture vs typed-Var is gone with the typed Vars themselves"
- **Severity:** LOW (historical context — explicitly explains a removed feature; readers grepping for "Var" are guided correctly).
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/mod.rs:46-48`
- **Comment:** breadcrumb mentioning the removed typed-Var system.
- **Fix:** Keep — actively prevents rot. **No action.**

### Skill doc "tier 1" / "tier 2" terminology
- **Severity:** LOW (architectural shorthand still accurate at a high level — tier 1 = cfg-time mini-graph, tier 2 = post-IR resolver — and matches the design spec referenced in the doc). Used in tests too (`crates/strider/tests/indirect_branch.rs`, `indirect_resolve_classify.rs`). Not stale.
- **Where:** Multiple (already enumerated in the search).
- **Fix:** Keep — terminology survives the round 7 rename and is still correct.

## File-level summary

| File | Findings | Highest severity |
| --- | --- | --- |
| `crates/ir/src/graph/mod.rs` | 1 | HIGH (ghost `IfCase`, missing `MemPhi`/`VarPhi`/`ValuePhi`/`StackStorePhi`) |
| `crates/ir/README.md` | 2 | HIGH (`IfCase` NodeKind), MED (`U80`/`F80` omission) |
| `crates/ir/src/lib.rs` | 1 | MED (broken `node::Graph` doclink ×2) |
| `crates/ir/examples/graph_creator.rs` | 1 | MED (wrong fn name in comment) |
| `crates/strider/src/strider/pipeline.rs` | 2 | HIGH (×2: omitted passes in two pipeline docs) |
| `crates/strider/src/indirect_resolve/mod.rs` | 1 | HIGH (`cfg::indirect_resolve` not a path) |
| `crates/strider/examples/strider.rs` | 1 | HIGH (non-existent fixture + symbol; wrong error message) |
| `crates/pcode-lift/src/lib.rs` | 1 | HIGH ("(planned)" lie) |
| `crates/strider-py/src/pattern.rs` | 1 | HIGH (PhiPat scope overstated) |
| `crates/strider-py/src/matcher.rs` | 1 | HIGH (`.vn(c)` scope overstated) |
| `crates/pattern/src/matcher/commutativity.rs` | 1 | MED (non-existent enum variants in doc) |
| `crates/strider/.claude/skills/strider-indirect-shape-author/SKILL.md` | 1 | MED (private path in skill) |
| `crates/opt/src/lib.rs` | 0 | informational only — `CallOtherElide` mention is intentional historical breadcrumb |
| `crates/opt/README.md` | 0 | same as above |
| `crates/ir/src/node/output_type.rs` and friends | 1 (LOW) | LOW (`the analyzer` legacy terminology) |

**Total HIGH:** 8 distinct items.
**Total MED:** 6 items (one of which is the same root cause as a HIGH item, but in a different file).
**Total LOW / no-action:** 3 items.

## Notes on what was checked but found clean

- `from_graph_and_entry` rename to `from_graph_and_entry_for_rewrite` is consistent across all sites (rs + tests + docs). The MED/HIGH set has no occurrences of the old name.
- `pattern::if_node()` doc is correctly "direct layout only" (`crates/pattern/src/pat/ctor/control.rs:138-150`) — round 7 fix held.
- `pattern::phi()` doc is correctly "VarPhi only" (`crates/pattern/src/pat/ctor/control.rs:39-46`) — round 7 fix held.
- `pattern::float_cmp_any` doc correctly disclaims `NotEqual` / `LessEqual` are not primitives (`crates/pattern/src/pat/ctor/variant_agnostic.rs:194-198`) — round 7 fix held.
- The Python `PhiPat` doc above is the analogue of the Rust `phi()` doc, and was *not* fixed in round 7.
- `TODO(Task17)` markers in three files are valid: they reference an open plan (`docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`, status "plan only. Not implemented.").
- `tier 1` / `tier 2` terminology in skill docs and tests still maps correctly to `cfg`-time vs orchestrator-time resolver layers.
- `TypedVar` / `NodeVar` legacy types: `grep` finds zero occurrences anywhere in the workspace — round 6/7 cleanup was thorough.
