# Round 8 — Strider Skill Bundle Re-Design

**Branch:** `review/ai2`. Independent audit + forward design.
**Trust note:** existing skills audited from `crates/strider/.claude/skills/`; gap analysis driven by `round8-17-graph-soundness.md`, `round8-correctness-cross-arch.md`, `round8-correctness-invariants.md`, `round8-2C-silent-failures.md`. No round8-summary exists yet.

---

## Audit of existing skills

`crates/strider/.claude/skills/` already contains 8 SKILL.md files implementing the Round-7 design:

| # | Existing skill | Round-7 mapping | Status |
|---|----------------|----------------|--------|
| 1 | `strider-pattern-author` | R7 #1 | KEEP — covers builders, lift-time aliases, `IfCondInversion`, captures, `find_all_requirements`. |
| 2 | `strider-debug-pattern` | R7 #2 | KEEP — covers zero-match diagnostics. |
| 3 | `strider-opt-pass-author` | R7 #3 | KEEP — pipeline placement, fingerprints, parity. |
| 4 | `strider-fingerprint-audit` | R7 #4 | KEEP — Layer-C `validate_with_options`, exempt list, superset contract. |
| 5 | `strider-indirect-shape-author` | R7 #5 | KEEP — classifier + inplace + fixture. |
| 6 | `strider-callother-abi` | R7 #6 | KEEP — `NoOp / NoReturn / Call(CallOtherAbi)`. |
| 7 | `strider-target-arch` | R7 #7 | KEEP **but extend** — see deprecation/merge notes; doesn't cover the orchestrator `for_arch` foot-gun nor the LR-callee-saved tradeoff. |
| 8 | `strider-py-binding` | R7 #8 | KEEP — module routing, error taxonomy. |

No skill currently lives under `.claude/skills/` at the workspace root, under `docs/superpowers/skills/`, or in `crates/strider-py/strider/`. The bundle is centralised under the `strider` crate. `.remember/` only holds session logs.

### Existing skills to **deprecate or merge**

- **None to delete.** All 8 are well-scoped.
- **Merge candidate:** `strider-fingerprint-audit` and the rewrite-finalisation tail of `strider-opt-pass-author` overlap. Resolution: keep both, but add cross-links — `opt-pass-author` already references `fingerprint-audit` in step 8; verified to be correct.

### Round-8-driven gaps the existing skills do **not** cover

1. **Orchestrator `Builder::for_arch` vs `Builder::with_endianness`** (`round8-correctness-cross-arch.md` §1, conf 97). `strider::run` builds the CFG with the wrong `ArchPreset` for every non-x86_64 arch. Existing skills miss this because none of them touch `crates/strider/src/orchestrator.rs:826`. New skill `strider-orchestrator-extend` covers it.
2. **CC link-register-as-callee-saved tradeoff** (`round8-17-graph-soundness.md` B-1/B-2/B-3, conf 82-88). `strider-target-arch` step 3 ("fill callee-saved regs") doesn't flag that AAPCS64/AAPCS/PPC SysV all list LR as caller-saved by spec but strider deliberately pins it as callee-saved to feed the `LinkRegister` indirect-branch arm. New skill `strider-cc-preset-extend` (or extend the target-arch skill) walks the explicit tradeoff and the regression-test requirement.
3. **Fixture authoring across architectures** (`round8-17-graph-soundness.md` E, RT-1..RT-4 + coverage gaps). Multiple skills mention "build a fixture" but none walks `fixtures/Makefile`, the cross-compiler matrix, and the `fixtures/out/<arch>/` layout. New skill `strider-fixture-author`.
4. **Lift-canonicalisation invariants** (`round8-17-graph-soundness.md` §F, RT-4). Pattern + opt skills assume canonicalisations are correct; no skill verifies a real lifted ELF retains them. Folded into `strider-fixture-author`.
5. **Round-tripping a `BuiltFunctionGraph` for visual debugging** (`round8-correctness-invariants.md` H-1/H-2). Fingerprint-audit skill catches Layer-C violations but doesn't walk graph-html-dump-driven debugging. Folded into a lightweight extension of `strider-debug-pattern`.

---

## New / extended skills proposed (6)

Six new SKILL.md files. All live alongside the existing eight under `crates/strider/.claude/skills/<name>/SKILL.md` so the bundle stays discoverable.

### 1. `strider-orchestrator-extend`

**Trigger phrases**
- "Add a new step to `strider::run`."
- "The orchestrator passes wrong `ArchPreset` for `<arch>`."
- "Make the indirect-branch fixed-point recognise <new placeholder shape>."
- "Wire a new optimizer pipeline into `LoopState`."

**When to invoke**
- Touching `crates/strider/src/orchestrator.rs`, `LoopState`, `Decision`, or the strider `Strider::build_*_pipeline` factories.
- After `cargo test --package strider` shows a `UnknownCallOtherError` for a non-x86_64 arch on a previously-passing user-op (arch-preset delivery bug, see `round8-correctness-cross-arch.md` §1).

**When NOT to invoke**
- Adding an opt pass with no orchestrator interaction → `strider-opt-pass-author`.
- Adding a new shape to the indirect-branch resolver only → `strider-indirect-shape-author`.

**Files to operate on**
- `crates/strider/src/orchestrator.rs` (single file, ~900 LOC).
- `crates/strider/src/strider/pipeline.rs` (only if changing pipeline composition).
- `crates/strider/tests/common/mod.rs` and `crates/strider/benches/scaling.rs` — these duplicate the `Builder::with_endianness` pattern; bug fixes must propagate.
- `crates/cfg/src/cfg/builder/mod.rs` (only if extending the `for_arch` constructor surface).

**Procedure**
1. Identify the call site. The orchestrator constructs CFGs at `orchestrator.rs:826` (production), `tests/common/mod.rs:215` (tests), `benches/scaling.rs:89` (benches). All three should use `Builder::for_arch(opts.strider.arch(), ...)` — never `Builder::with_endianness` directly. The `with_endianness` constructor hardcodes `preset: ArchPreset::X86_64` and is the source of the cross-arch CallOther-classification bug.
2. If extending `LoopState` → make every new `Decision` variant exhaustive in `LoopState::step`, `LoopState::run`, and the convergence check. Use `match` not `if let`.
3. If extending the pipeline → choose stable / destructive / post-pass placement. The orchestrator runs destructive passes **only** at the fixed-point exit; running them mid-iteration invalidates `RegionIndex` `NodeId` pins.
4. Mirror new public APIs into `crates/strider-py/src/strider_cls.rs` and `run.rs`.

**Verification**
- `cargo test --package strider --workspace` (focus on `test_orchestrator_*` and `test_run_*`).
- For arch-preset fixes specifically: `cargo test --package strider arm` and `cargo test --package strider aarch64` — confirm no `UnknownCallOtherError` for `swi`/`CallHyperVisor`/`CallSecureMonitor`.
- `uv run pytest crates/strider-py/tests/python/test_smoke.py`.

**Exit criteria**
- All three orchestrator-equivalent CFG construction sites (`orchestrator.rs`, `tests/common`, `benches/scaling`) use a single canonical constructor.
- New `Decision` variants exhaustively handled.
- `cargo clippy --workspace -- -D warnings` clean.

---

### 2. `strider-cc-preset-extend`

**Trigger phrases**
- "Add a new calling convention on `<existing arch>`."
- "AAPCS64 `callee_saved_regs` includes x30 — is that a bug?"
- "Document the LR-as-callee-saved tradeoff."
- "Add `mips_n32` / `riscv_lp64d` / `<new ABI>`."

**When to invoke**
- Adding a CC preset on an already-supported `SleighArch`.
- Auditing or modifying `callee_saved_regs` on an arch with a link register.

**When NOT to invoke**
- Adding a brand-new arch → `strider-target-arch` (this skill is its CC-only sibling).
- Modifying CallOther classification for an arch → `strider-callother-abi`.

**Files to operate on**
- `crates/target/src/calling_convention/mod.rs` (CC presets).
- `crates/target/src/calling_convention/tests.rs` (preset assertions).
- `crates/strider-py/src/cc.rs` (Python parity).
- A regression fixture under `fixtures/cases/` if behavioural change is observable.

**Procedure**
1. Pick the closest existing preset and copy its block. Preserve field order: SP reg, integer-arg regs, float-arg regs, integer-return regs, float-return regs, `callee_saved_regs`, `stack_arg_offsets`, `ret_stack_pop`, link-register Vn name, optional `syscall_number_reg_name`, optional `no_memory_clobber`.
2. **Link-register vs callee-saved tradeoff** (round-8 finding B-1/B-2/B-3). On AAPCS64 / AAPCS / PPC SysV / PPC ELFv1 / PPC ELFv2, the spec marks LR as caller-saved. Strider intentionally lists it under `callee_saved_regs` so `InitialVar(LR)` propagates through call sites and the `LinkRegister` indirect-branch arm classifies returns. Document the deviation in a code comment at the preset definition. Trade-off recorded in spec/comment, not silent.
3. If the new CC needs LR honestly modelled (e.g. an ABI where LR really is caller-clobbered and you don't need indirect-branch return resolution): add `link_register_preserved_by_convention: bool` to the CC struct (round-8 RT-1 fix), default `true`, set `false` for the new preset. Threading this through `pcode_lift::ValueLifter::clobbered_outputs` is required.
4. `ret_stack_pop` traps: `8` on x86_64 (callee pops return address), `4` on x86 cdecl, `0` on every link-register arch. Wrong value silently breaks `CallStackArgCollect`.
5. Verify register names against rsleigh's spec table; `CallingConvention::build` errors at runtime, not at compile time, on a typo.
6. Python parity: factory in `cc.rs`, mirror name+args exactly.

**Verification**
- `cargo test --package target` (CC build + name resolution).
- `cargo test --package strider <fixture-name>` (e.g. `test_fib_recursive` if LR semantics changed).
- New regression test: lift a tail-call shim that overwrites LR (e.g. AAPCS64 `mov x30, x1; br x1`) and assert post-call `x30` is **not** `InitialVar(x30)` — covers RT-1 from `round8-17-graph-soundness.md`.

**Exit criteria**
- Preset compiles and runs validate-clean on at least one fixture.
- LR tradeoff documented in code if applicable.
- Python factory exists.

---

### 3. `strider-fixture-author`

**Trigger phrases**
- "Add a fixture for `<arch>` exercising `<feature>`."
- "I need a `__fentry__`-instrumented kernel binary fixture."
- "Cover AArch64 jump-table dispatch with a fixture."
- "Add a real-ELF test that asserts `Add(_, Neg(_))` on a `sub` instruction."

**When to invoke**
- Round-8 surfaced four explicit fixture gaps (`round8-17-graph-soundness.md` summary): no `__fentry__`, no AArch64 jump table, no 128-bit return, no real-ELF canonicalisation shape. Any of these justify the skill.
- Any new feature that needs cross-arch verification.
- New optimization passes whose unit tests use `graphmock` and need a real-ELF complement.

**When NOT to invoke**
- The feature can be tested with `graphmock` and unit tests fully — fixture overhead unjustified.
- The fixture already exists — extend the existing test rather than adding a parallel binary.

**Files to operate on**
- `fixtures/Makefile` (adds a build rule).
- `fixtures/cases/<feature>.c` or `fixtures/arch/<arch>/<feature>.s`.
- `fixtures/out/<arch>/<name>.elf` is **generated** — do not commit unless gitignore policy says otherwise; check `.gitignore`.
- `crates/strider/tests/<feature>.rs` or `crates/strider-py/tests/python/test_<feature>.py`.
- Sometimes `fixtures/kernels/` for kernel/instrumented fixtures.

**Procedure**
1. Check whether a similar fixture exists. `fixtures/cases/` holds C source; `fixtures/arch/<arch>/` holds hand-written assembly when C-front-end can't express the shape (e.g. `__fentry__` instrumentation, hand-rolled jump table).
2. Pick a cross-compiler. The Makefile already invokes `aarch64-linux-gnu-gcc`, `arm-linux-gnueabi-gcc`, `mips-linux-gnu-gcc`, `mipsel-linux-gnu-gcc`, `powerpc-linux-gnu-gcc`. RISC-V or new arches need a new toolchain entry.
3. Keep the fixture **minimal**: one function exhibiting one shape, ideally inline-no-stack-frame to keep the IR readable. Use `__attribute__((noinline))` to prevent fold-into-`main`.
4. Write the test alongside. Naming: `test_<feature>_<arch>` so the workspace test grep finds it. Lift via `strider::run` and assert IR shape using `Matcher::find_all` patterns from `pattern`. If the fixture targets a specific canonicalisation (e.g. RT-4: `Add(_, Neg(_))` from a `sub` insn), the assertion shape **must** use the lift-time-canonicalised pattern (`pattern::sub` is the alias; the underlying IR is `Add(_, Neg(_))`).
5. Asm-fingerprint coverage. Capture a value node in the test, call `match.asm_fingerprint(c, &graph)`, and assert the resulting addresses match the fixture's disassembly.

**Verification**
- `make -C fixtures` (or `make <name>` for a single fixture) — must build cleanly.
- `cargo test --package strider <test_name>` (or pytest equivalent).
- `cargo clippy --workspace -- -D warnings`.

**Exit criteria**
- Fixture builds in CI on all platforms.
- Test asserts the *expected IR shape* (not just "lifting succeeds").
- Asm-fingerprint assertion ties matched nodes back to fixture disassembly addresses.

---

### 4. `strider-flagcmp-rule-author`

**Trigger phrases**
- "Add a `FlagCmpCanonicalize` rule for `<flag-tree shape>`."
- "PowerPC CR-bit conditional branches don't canonicalise to `IntCmpOp`."
- "Indirect dispatch fails on `<arch>` because the bound walker can't see the `IntCmp`."

**When to invoke**
- A new arch (or a new shape on an existing arch) emits flag-bit reads that survive past `ConstantFold` — typically appears as `BoolBinaryOp` over individual flag bits feeding an `If`.
- Round-8 finding `round8-correctness-cross-arch.md` §2 (PPC CR canonicalisation gap, conf 80) is the canonical case.

**When NOT to invoke**
- The shape is already an `IntCmpOp` — no canonicalisation needed.
- The shape is unique to one fixture and the rule wouldn't generalise — write a targeted fold in `ConstantFold` instead.

**Files to operate on**
- `crates/opt/src/flag_cmp_canonicalize/mod.rs` (rule registration).
- `crates/opt/src/flag_cmp_canonicalize/rules/<arch>.rs` (rule body).
- `crates/opt/src/flag_cmp_canonicalize/tests.rs` (graphmock unit test).
- A real-ELF fixture via `strider-fixture-author` if the shape exists in real binaries.

**Procedure**
1. Identify the source shape. Lift a small example, dump `graph-opt.html`, locate the flag-bit producers (typical: `Truncate` of a `Load(register)`, or a `BoolBinaryOp::And` of two flag-bit reads).
2. Express it as a `pattern::Pat` matching the LHS. Use Captures for the operands the rule needs to forward.
3. Write the RHS builder. **`rhs_capture` and `lhs_capture` differ** — the round-8 silent-failure audit (`round8-2C-silent-failures.md` H1) flagged the `unwrap_or(a)` pattern on `rhs_capture` as load-bearing. Always `.expect("rhs_capture must bind")` — never `.unwrap_or(a)`.
4. Add the rule to the per-arch list. Each rule carries an `arch_filter` (`ArchPreset` set) — keep PowerPC rules off AArch64 lookup tables.
5. Asm-fingerprint propagation. The rule's `build_rhs` must call `extend_asm_fingerprint_from(new_node, source)` for every replaced source node — `BoolNeg` and intermediate flag-bit nodes count.
6. Test with a graphmock LHS shape and assert the RHS structure via pattern queries.

**Verification**
- `cargo test --package opt flag_cmp_canonicalize`.
- `cargo test --package opt validate_with_options` (Layer C: every reachable non-exempt node carries a fingerprint).
- If real-ELF fixture exists: `cargo test --package strider <fixture>`.

**Exit criteria**
- Rule fires on the target shape and only the target shape (negative tests: no false positives on adjacent shapes).
- Fingerprint absorption verified.
- `IndirectBranchResolve`'s bound walker can now compute the table size on an indirect dispatch using this canonicalisation.

---

### 5. `strider-cli-runner`

**Trigger phrases**
- "Run the strider CLI on `<binary>`."
- "Dump `cfg.html` / `graph.html` / `graph-opt.html` for `<entry>`."
- "Lift this binary and show me the IR."
- "Visualise the IR for the function at `0x<addr>`."

**When to invoke**
- User has a binary in hand and wants visual debugging without writing Rust.
- Triage step before invoking `strider-debug-pattern` or any opt-pass debugging.
- Bug-report reproducer requested.

**When NOT to invoke**
- User wants to write a pattern → `strider-pattern-author` directly.
- User wants to add a new feature → development skill.

**Files to operate on**
- `crates/strider/examples/strider.rs` (the example binary; only edit when adding flags).
- `fixtures/out/<arch>/<binary>.elf` for the input.
- Output: `cfg.html`, `graph.html`, `graph-opt.html` in workspace root.

**Procedure**
1. Build fixture if not present: `(cd fixtures && make <target>)`.
2. Run example: `cargo run -p strider --example strider`. Edit `examples/strider.rs` to point at the user's binary if needed (entry address, arch, CC).
3. Open the three HTMLs in a browser. `cfg.html` shows basic blocks; `graph.html` is unoptimised IR; `graph-opt.html` is post-pipeline.
4. For Python side: `uv run python -c "import strider; ..."` reproducer template.
5. Asm-fingerprint readout: for any node of interest in the IR dump, call `graph.asm_fingerprint(node_id)` (Rust) or `match.asm_fingerprint(c)` (Python) to recover contributing machine-instruction addresses.

**Verification**
- All three HTMLs generated; visual inspection matches user's expectation.
- If `validate` fails, the example surfaces it via `anyhow::Error`.

**Exit criteria**
- User has a reproducible CLI invocation.
- IR dump exists for the target function.
- Hand-off to a debugging skill (`strider-debug-pattern` / `superpowers:systematic-debugging`) if the dump shows a problem.

---

### 6. `strider-validation-invariant-extend`

**Trigger phrases**
- "Add a new validate Layer-C check."
- "Detect zombie `InitialVar` nodes wired into post-stable-pipeline reads."
- "Strengthen `validate_with_options`."
- "How do I assert <new IR invariant> at validate time?"

**When to invoke**
- A round-8-style invariant violation (`round8-correctness-invariants.md` M-1: zombie-`InitialVar` resurrection in `apply_in_place_edits`) deserves a static check rather than ad-hoc test.
- Adding an attribution / propagation invariant beyond fingerprints.

**When NOT to invoke**
- The check is per-pass-internal — keep it inside the pass, not in `validate`.
- The check is specific to one fixture — write a unit test, not a global validator.

**Files to operate on**
- `crates/ir/src/validate/mod.rs` (entry point, `ValidateOptions`).
- `crates/ir/src/validate/layer_c.rs` (graph-level invariants).
- `crates/ir/src/validate/errors.rs` (new `ValidationError` variants).
- `crates/ir/src/validate/tests.rs`.

**Procedure**
1. Decide layer. Layer A = local typing (`expected_signature`); Layer B = bidirectional use-list; Layer C = global invariants. New check almost always lives in Layer C.
2. Add an opt-in flag on `ValidateOptions` rather than making the check mandatory — preserves backward compatibility with mock-graph tests that don't set up the invariant.
3. Walk reachable nodes via `walk::walk_graph` — Layer C runs in reachability-scoped fashion to avoid false positives on optimization-pass zombies.
4. Add a `ValidationError` variant. Aggregate, don't fail-fast — keep the existing `ValidationErrors` bundle semantics.
5. Test with a deliberately-malformed graphmock case + at least one passing fixture-derived case.

**Verification**
- `cargo test --package ir validate`.
- `cargo test --workspace` — confirm no existing test trips the new check unexpectedly.
- Run `validate_with_options(graph, entry, ValidateOptions { check_<new>: true })` on one of the canonical orchestrator-output fixtures.

**Exit criteria**
- Check is opt-in, documented, and covers at least one round-8-class regression.
- New error variant wired through the `ValidationErrors` bundle.
- Aggregating semantics preserved (no fail-fast).

---

## Skill discovery & ordering hints

Recommended invocation sequence for cross-cutting tasks (so the orchestrator skill can hand off):

- "Add support for arch X" → `strider-target-arch` → `strider-callother-abi` (per user-op) → `strider-cc-preset-extend` → `strider-orchestrator-extend` (for the `for_arch` handoff) → `strider-fixture-author` → `strider-py-binding`.
- "My pattern is wrong" → `strider-cli-runner` → `strider-debug-pattern` → optionally `strider-pattern-author` if rewrite needed.
- "Indirect branch unresolved on arch X" → `strider-cli-runner` → `strider-flagcmp-rule-author` (if PPC/CR-style flag tree) → `strider-indirect-shape-author` (if new shape) → `strider-fixture-author` (regression).
- "New opt pass" → `strider-opt-pass-author` → `strider-fingerprint-audit` → `strider-fixture-author` (real-ELF complement) → `strider-validation-invariant-extend` (if the pass introduces a new invariant).

---

## Summary

- **8 existing skills** cover the Round-7 design and remain valid; none are deprecated.
- **6 new skills** address Round-8-surfaced gaps:
  1. `strider-orchestrator-extend` (cross-arch finding §1; `for_arch` foot-gun)
  2. `strider-cc-preset-extend` (graph-soundness B-1/B-2/B-3; LR tradeoff)
  3. `strider-fixture-author` (RT-1..RT-4, four explicit coverage gaps)
  4. `strider-flagcmp-rule-author` (cross-arch §2 PPC CR; silent-failures H1)
  5. `strider-cli-runner` (debugging triage; precedes `strider-debug-pattern`)
  6. `strider-validation-invariant-extend` (invariants M-1)
- **Total bundle**: 14 skills, all under `crates/strider/.claude/skills/<name>/SKILL.md`.

Reference files for authoring (absolute paths):
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/.claude/skills/` — bundle root.
- `/mnt/c/Users/mikeg/Documents/strider/reviews/round8-17-graph-soundness.md` — drives skills 2, 3.
- `/mnt/c/Users/mikeg/Documents/strider/reviews/round8-correctness-cross-arch.md` — drives skills 1, 4.
- `/mnt/c/Users/mikeg/Documents/strider/reviews/round8-correctness-invariants.md` — drives skills 4, 6.
- `/mnt/c/Users/mikeg/Documents/strider/reviews/round8-2C-silent-failures.md` — drives skill 4 (H1 `unwrap_or(a)` trap).
