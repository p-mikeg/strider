# Round 11 — 6: skill audit

Trust model: I did NOT read any prior round audit (round7/8/9/10). All
verdicts derive from reading each `SKILL.md` against the live tree at
`/mnt/c/Users/mikeg/Documents/strider/`.

## Per-skill verdicts

### strider-builder-for-arch-migration — KEEP-AS-IS

Drifts: none of significance. The skill cites `cfg::Builder::with_endianness`
and `cfg::Builder::for_arch` correctly:

- `with_endianness` confirmed at `crates/cfg/src/cfg/builder/mod.rs:122`
  with `preset: target::ArchPreset::X86_64` hard-coded at line 133.
- `for_arch` confirmed at `crates/cfg/src/cfg/builder/mod.rs:149` reading
  `arch.preset` and `arch.endianness`.
- The Round-9 phase-A migration sites referenced
  (`tests/indirect_branch.rs`, `crates/cfg/tests/known_targets.rs`,
  `cfg/tests/indirect_dispatch.rs`) all exist; `Builder::for_arch` is
  the production-orchestrator constructor at
  `crates/strider/src/orchestrator.rs:940`.

Procedure is sound; no edits required.

### strider-callother-abi — NEEDS-UPDATE-MINOR

Drifts:

1. **Step 1 cites the dispatch as "in `cfg::region_builder::process_insn`"**
   — actual function is `process_new_insn` at
   `crates/cfg/src/cfg/builder/region_builder.rs:238` and the
   `Opcode::CallOther` arm is at line 392. `process_insn` exists but
   delegates to `process_new_insn`.
2. **Step 3** cites `crates/target/src/call_other_abi.rs:12` for
   `CallOtherAbi`. Verified: the struct `pub struct CallOtherAbi` lives
   at line 12 — correct.
3. The `classify(preset, name)` symbol is at line 61, `classify_arch_specific`
   at line 76, `classify_arch_independent` at line 246 — the
   skill paraphrases these correctly.

Proposed edits:

- Step 1: change "consulted in `cfg::region_builder::process_insn` at
  the `Opcode::CallOther` arm" to "consulted in
  `cfg::builder::region_builder::process_new_insn`'s
  `Opcode::CallOther` arm (~line 392)".

### strider-cc-preset-extend — NEEDS-UPDATE-MAJOR

Drifts:

1. **Step 1 cites preset locations at `mod.rs ~line 460` (mips_n64).**
   Actual lines: `mips_n64` is at `crates/target/src/calling_convention/mod.rs:573`,
   not 460. Other quoted locations are not strictly cited but the skill
   does say "Locate by symbol name rather than line number" so this is
   actually a positive design choice — only the worked-example line
   number drifted.
2. **Step Background paragraph (line 124) cites
   `pcode_lift::ValueLifter::clobbered_outputs`** as the function that
   "uses `callee_saved_vns` to decide which registers survive a
   `Call`". This function does NOT exist in `crates/pcode-lift/src/`
   — neither `clobbered_outputs` nor any per-call clobber projection
   lives in the pcode-lift crate. Verified by `grep -rn "fn clobbered\|callee_saved_vns"
   crates/pcode-lift/src/` returning zero hits. The actual call-clobber
   plumbing lives in `crates/strider/src/orchestrator.rs:764`'s
   `build_anchor_calling_context` (and, per
   `crates/strider/src/strider/insn/control.rs:228` `handle_call`,
   the per-Call construction). Pitfall section repeats this with
   "thread a new `link_register_preserved_by_convention` knob through
   `pcode_lift::ValueLifter::clobbered_outputs`" — same error.
3. **Step 5 (`ret_stack_pop` traps)**: the "8 on x86_64 SysV" claim
   is correct per `crates/target/src/calling_convention/mod.rs:387`'s
   `x86_64_systemv()` block.
4. **Step 9 referencing `call_other_abi.rs:822`** — this line is
   not verified against the current code; round-8 line numbers have
   drifted. Today the file ends near line 800-ish and `mfence`/
   `sfence`/`lfence` should be enumerated against the current file.

Proposed edits:

- Replace every reference to `pcode_lift::ValueLifter::clobbered_outputs`
  with the correct location: the call-clobber projection lives in
  `crates/strider/src/orchestrator.rs::build_anchor_calling_context`
  (line 764) and `crates/strider/src/strider/insn/control.rs::handle_call`
  (line 228). The strider crate computes `clobbered_kinds` from the
  CC's `callee_saved_set`.
- Re-grep for `call_other_abi.rs:822` and update or remove the line
  citation.
- Step 1 worked example: bump `mips_n64` from `~line 460` to actual
  `~line 573` (or drop the line number in favour of "grep by symbol",
  which the surrounding paragraph already endorses).

### strider-cli-runner — NEEDS-UPDATE-MINOR

Drifts:

1. **Step 1 cites `crates/strider/examples/strider.rs:14-15`** for
   `binary_path` / `symbol`. Actual lines: `binary_path` at line 16,
   `symbol` at line 17 (line 14 is a comment, line 15 is blank-ish).
2. **Step 3 cites `crates/strider/examples/strider.rs:21-27`** for the
   `arch` and `CallingConvention` lines. Actual: `arch =
   strider::SleighArch::x86();` at line 23, `CallingConvention::x86_cdecl()`
   at line 28.
3. **Step 1 procedure says `cargo run -p strider --example strider`
   uses `Builder::new(...)`**. Confirmed by reading the example —
   `Builder::new(sleigh, addr, cfg_options).build()` is what the
   example does (line ~40). The example has `#![allow(deprecated)]`
   for that reason.
4. **Step 1 in Rust dump uses `dot::write_html`** — this function
   does not exist in `crates/dot/src/lib.rs`. The actual API is
   `GraphRenderer::dump_as_html(&self, out_path)` at
   `crates/dot/src/lib.rs:463`. (The skill `strider-debug-pattern`
   has the same error — see below.)

Proposed edits:

- Bump line ranges in Steps 1 and 3 to match the current example file
  (16-17 for `binary_path`/`symbol`; 23-28 for arch+CC).
- The `dot::write_html` reference appears in `strider-debug-pattern`,
  not this skill — leaving alone here.

### strider-debug-pattern — NEEDS-UPDATE-MINOR

Drifts:

1. **Step 1: `dot::write_html(&graph, "/tmp/dump.html")?;` (see
   `crates/dot/src/lib.rs`)**. There is no `write_html` in
   `crates/dot/src/lib.rs`. The actual API is on `GraphRenderer`
   (line 335) with method `dump_as_html` at line 463 (and
   `as_html_from_svg` at line 432 for an in-memory string). The
   correct snippet is something like
   `dot::GraphRenderer::new(&graph, dot::DotStyle::dark()).dump_as_html("/tmp/dump.html")?;`.
2. **Step 5: `m.asm_fingerprint(c, &graph)` (see `crates/ir/src/graph/store.rs`)**.
   The accessor `pub fn asm_fingerprint(&self, node_id: NodeId) -> &[u64]`
   is at `store.rs:132` — confirmed. But `m.asm_fingerprint(c, &graph)`
   is a `Match` accessor (in the `pattern` crate), not a `Graph`
   accessor — the skill is conflating two APIs. The `crates/ir`
   path is wrong; the `Match::asm_fingerprint` accessor lives in
   `crates/pattern/src/matcher/`.

Proposed edits:

- Step 1: replace `dot::write_html(&graph, "/tmp/dump.html")?;` with
  `dot::GraphRenderer::new(&graph, dot::DotStyle::dark()).dump_as_html("/tmp/dump.html")?;`
  and update the file pointer (or drop it).
- Step 5: replace "see `crates/ir/src/graph/store.rs`" with "see
  `Match::asm_fingerprint` in the pattern crate" — `Graph::asm_fingerprint`
  is the underlying side-table accessor, but `m.asm_fingerprint(c, &graph)`
  is a matcher-side method.

### strider-doc-line-number-refresh — KEEP-AS-IS

This is a meta-skill about refreshing line numbers; the cited
example drifts from round-9 phase A are historical (it cites them as
applied — `:24`, `:30`, `:50`, `:49`, `:77`). I confirmed
`store.rs:132` (asm_fingerprint) and `:184` (extend_asm_fingerprint_from)
match the skill's reference. The skill is self-consistent and its
own footer correctly notes that historical line numbers should be
updated only when they drift again — we are not at that point yet.

Anti-patterns section ("don't pad with wide ranges") is good guidance
that other skills should follow.

### strider-fingerprint-audit — KEEP-AS-IS

Drifts: none significant.

- Step 1 cites `crates/ir/src/graph/store.rs:132-184` — confirmed:
  `asm_fingerprint` at line 132, `set_asm_fingerprint` at 144,
  `extend_asm_fingerprint` at 154, `extend_asm_fingerprint_from` at
  184.
- Step 2 cites `crates/ir/src/validate/mod.rs:83` for
  `validate_with_options` — confirmed.
- Step 2 cites `crates/ir/src/validate/layer_c.rs::asm_fingerprint_exempt`
  — confirmed at `layer_c.rs:184`. The exempt list (Entry,
  InitialMemory, InitialVar, FunctionArg, ControlState, MemPhi,
  VarPhi, ValuePhi, StackStorePhi) is correct.
- Step 5 references "the per-region driver wraps each `process_insn`
  call ... in `set_lift_addr(Some(addr)) ... set_lift_addr(None)`".
  `set_lift_addr` confirmed at `crates/ir/src/builder/mod.rs:393`.
- Step 5 cites `crates/strider/src/strider/insn/` — directory exists
  with `control.rs` and `mod.rs`.

This is the cleanest of the 19 skills.

### strider-fixture-author — NEEDS-UPDATE-MINOR

Drifts:

1. **`fixtures/Makefile:19`** — confirmed: `ARCHES :=` is at line 19.
2. **`fixtures/Makefile:59`** — confirmed: `SHARED_CASES :=` is at
   line 59.
3. **Step 5 says "16 today"** for the per-arch test count. Today's
   `ARCHES` list is `x86 x86_kernel x64 aarch64 aarch64be arm arm_be
   arm_thumb mips32le mips32be mips64le mips64be ppc32be ppc32le
   ppc64be ppc64le` — exactly 16. **Pitfalls section says "expands
   to 15 tests"** — contradiction within the same skill.
4. **Step 5** lists case names in `fixtures/cases/`. Verified:
   `abi.c, arithmetic.c, builtins.c, calling_convention.c, calls.c,
   complex.c, control.c, elf_relocs.c, floats.c, globals.c,
   indirect_branch.c, memory.c, patterns.c, stack.c, switch.c` —
   matches the directory listing exactly.
5. **Step 5 macro location** — `macro_rules! per_arch_test` is at
   `crates/strider/tests/common/mod.rs:410`. Not cited by line, so
   no drift.

Proposed edits:

- Pitfalls section: "15 tests" → "16 tests" for consistency with
  Step 5.

### strider-flagcmp-rule-author — NEEDS-UPDATE-MINOR

Drifts:

1. **Step 2 says "around `mod.rs:314`"** for `build_rules`. Actual:
   `fn build_rules` is at `crates/opt/src/flag_cmp_canonicalize/mod.rs:317`.
   Close — 3 lines off.
2. **Step 7 says "Append to the `vec!` returned by `build_rules`
   (`mod.rs:314`)"** — same drift, should be 317.
3. **Step 2 says "the existing 9 rules"**. Counting via grep:
   `grep -c "rule(\|rule_unary("` returned 14 (these include the
   helper definitions plus call sites). The actual call-site count
   inside `build_rules` is 11 rules per a more careful read. The
   skill's "9" appears stale.
4. **Step 9 negative test** suggestion is sound; pinning the
   rule-author contract in `Rule::rhs_capture: Option<Capture>` is
   confirmed at `mod.rs:112`.
5. `try_apply_rule` at `mod.rs:115`, `build_int_cmp` at `mod.rs:173`,
   `build_bool_neg` at `mod.rs:189`, `rule` ctor at `mod.rs:293`,
   `rule_unary` at `mod.rs:305` — all verified.

Proposed edits:

- "around `mod.rs:314`" → `mod.rs:317`.
- "the existing 9 rules" → "the existing rules in `build_rules`"
  (avoiding a count that drifts with each new rule).

### strider-indirect-shape-author — NEEDS-UPDATE-MINOR

Drifts:

1. **Step 4** says "Wire into `classify_anchor` in
   `crates/opt/src/indirect_branch_resolve/classify.rs`". Confirmed:
   `pub fn classify_anchor` at `classify.rs:66`. Two related variants
   exist (`classify_anchor_with_rom` line 100, `classify_anchor_with_rom_and_sp`
   line 139) — skill doesn't mention these.
2. **Step 5** says apply functions live at
   `crates/opt/src/indirect_branch_resolve/inplace.rs` and are
   re-exported from `crates/strider/src/indirect_resolve/inplace.rs`.
   Confirmed: `apply_link_register` at `inplace.rs:43`, `apply_tail_call`
   at `inplace.rs:103` (in opt). Re-exports in
   `crates/strider/src/indirect_resolve/inplace.rs` confirmed by
   directory listing.
3. **Step 6** says wire `Decision` at `crates/strider/src/orchestrator.rs::LoopState`.
   Confirmed: `enum Decision` at `orchestrator.rs:188` and `LoopState::step`
   at line 416, `run_stable_only` at 464, `rebuild` at 483.
4. **`MAX_TABLE_ENTRIES = 4096`** — confirmed at
   `crates/opt/src/indirect_branch_resolve/mod.rs:53`.

Proposed edits:

- Step 4 should also mention `classify_anchor_with_rom` and
  `classify_anchor_with_rom_and_sp` — the resolver has three entry
  points today and a new shape needs to consider whether it depends
  on rom/SP.

### strider-opt-pass-author — NEEDS-UPDATE-MINOR

Drifts:

1. **Step 6 cites `crates/ir/src/graph/store.rs:184`** for
   `extend_asm_fingerprint_from` — confirmed.
2. **Step 6 mentions `Graph::create_node_attributed(..., &[contributor])`**
   — confirmed at `store.rs:294`.
3. **Step 1** says "Templates: `crates/opt/src/if_cond_inversion/`
   and `crates/opt/src/constant_fold/` are both `OptimizerOnBuilt`."
   The directories exist (verified). I did not deeply verify both are
   `OptimizerOnBuilt` impls vs `Optimizer` — taking the skill's word.
4. **Step 4 cites `crates/opt/src/pipeline.rs`** for the pipelines.
   `OptimizerPipeline` lives there (`trait Optimizer` at line 98,
   `trait OptimizerOnBuilt` at line 141). But `default_pipeline`,
   `stable_default_pipeline`, `destructive_default_pipeline` are
   actually in `crates/opt/src/lib.rs:193, 123, 165` respectively —
   NOT in pipeline.rs. Step 4 cites the wrong file.
5. **Step 8 cites Python `PipelineState::from_default()`** — confirmed
   at `crates/strider-py/src/opt.rs:55`.

Proposed edits:

- Step 4: "Pick a pipeline placement in `crates/opt/src/pipeline.rs`"
  → "Pick a pipeline placement: `default_pipeline()` /
  `stable_default_pipeline()` / `destructive_default_pipeline()` are
  in `crates/opt/src/lib.rs` (lines 193, 123, 165 respectively); the
  `OptimizerPipeline` impl and `Optimizer` / `OptimizerOnBuilt` traits
  are in `pipeline.rs`."

### strider-orchestrator-extend — NEEDS-UPDATE-MAJOR

Drifts:

1. **Step 1, "around line 837", and Exit Criteria "(`orchestrator.rs:908`)"**:
   the `Builder::for_arch` site is actually at
   `crates/strider/src/orchestrator.rs:940`. ~100 lines off.
2. **Pitfalls section** says "Concretely, `crates/cfg/src/cfg/builder/mod.rs:113`
   sets `preset: target::ArchPreset::X86_64` unconditionally in
   `with_endianness`; only `for_arch` (lines 129-146) reads `arch.preset`".
   Actual: `with_endianness` is at line 122 and the `preset:
   target::ArchPreset::X86_64` line is line 133, NOT 113. `for_arch`
   is at lines 149-167, NOT 129-146.
3. **Step 3 cites the `enum Decision` at "line 188"** — confirmed.
4. **Steps 3 and 4 cite the dispatch at "lines 178-184"** — confirmed
   (the `match state.step()? { ... }` block is at lines 178-183).
5. **Step 2 cites `crates/strider/tests/common/mod.rs:220`** for
   `Builder::for_arch` — confirmed at line 220.
6. **Step 2 cites `crates/strider/benches/scaling.rs:93`** — confirmed
   at line 93.
7. **Worked Example Step 1** says "Extend the enum at
   `crates/strider/src/orchestrator.rs:188`" — confirmed.

Proposed edits:

- Step 1: "around line 837" → "around line 940".
- Exit Criteria: `orchestrator.rs:908` → `orchestrator.rs:940`.
- Pitfalls section: `cfg/src/cfg/builder/mod.rs:113` → `:133` (the
  `preset:` line inside `with_endianness`); "lines 129-146" →
  "lines 149-167".

### strider-pattern-author — KEEP-AS-IS

Drifts: none significant.

- Step 1 cites `crates/pattern/src/pat/builders/call.rs`,
  `ret.rs`, `memory.rs`, `branch.rs`, `phi.rs`. All files exist
  in the listed directory.
- The free-constructor list (`int_const_any_of`, `predicate`, etc.)
  matches `crates/pattern/src/pat/ctor/wildcards.rs` and
  `consts.rs`. `pub fn int_const_any_of` confirmed at
  `wildcards.rs:98`; free `call() / ret() / if_node() / stack_store()`
  at `crates/pattern/src/pat/ctor/control.rs:30,122,135,148`.
- `Capture` at `crates/pattern/src/var.rs:36` — confirmed.
- Python intern_str path `pattern.rs::intern_str` confirmed at
  `crates/strider-py/src/pattern.rs:127`.

### strider-public-api-encapsulation — KEEP-AS-IS

Drifts: none significant.

- `BuiltCallingConvention::try_from_parts` confirmed at
  `crates/target/src/calling_convention/mod.rs:213`.
- `cfg::Cfg::region_id_at_start` confirmed at
  `crates/cfg/src/cfg/query.rs:143`.
- `ResolvedTargets::multiple` confirmed at
  `crates/opt/src/indirect_branch_resolve/mod.rs:113`.
- `MAX_TABLE_ENTRIES` constant for context at `:53`.

The skill is mostly a "round-9 retrospective + ongoing methodology"
skill. Its references hold up.

### strider-py-binding — KEEP-AS-IS

Drifts: none significant.

- The crate-side modules listed (`arch.rs, cc.rs, cfg.rs, dot.rs,
  errors.rs, graph.rs, matcher.rs, opt.rs, pattern.rs, reader.rs,
  run.rs, sleigh.rs, strider_cls.rs`) — confirmed by directory
  listing.
- Error helpers (`into_pattern_err`, `into_reader_err`, `into_lift_err`,
  `into_rewrite_err`, `into_strider_err`) confirmed at
  `crates/strider-py/src/errors.rs:31, 42, 64-66`.
- `PipelineState::from_default` confirmed at
  `crates/strider-py/src/opt.rs:55`.
- `test_public_api_snapshot.py` confirmed exists.

### strider-rewrite-rule-multinode-audit — KEEP-AS-IS

Drifts: none significant.

- The engine implementation (`pre_build_node_id = ctx.graph.next_node_id();`)
  confirmed at `crates/pattern/src/rewrite.rs:71`.
- The walk-bound check `cur.index() < pre_build_node_id.index()`
  confirmed at `rewrite.rs:127`.
- `next_node_id` on `Graph` confirmed at
  `crates/ir/src/graph/access.rs:115`.
- The reference test
  `crates/opt/tests/asm_fingerprint_propagation.rs::constant_fold_rule_and_dist_attributes_inner_nodes`
  confirmed (file exists).

### strider-silent-failure-audit — KEEP-AS-IS

Drifts: none significant. This is a pattern-recognition methodology
skill — its content is generic guidance plus a list of round-9
findings (H-6, H-8, H-4, H-5, H-7) that have no live line citations
to drift. The exception taxonomy (`StriderError`, `LiftError`, etc.)
is verified to exist in `crates/strider-py/src/errors.rs`.

### strider-target-arch — NEEDS-UPDATE-MAJOR

Drifts:

1. **Step 3 cites CC preset locations at specific lines** that are
   all wrong:
   - `aarch64_aapcs64` cited as line 249 — actually line 458.
   - `arm_aapcs` cited as line 289 — actually line 498.
   - `mips_o32` cited as line 330 — actually line 539.
   - `mips_n64` cited as line 364 — actually line 573.
   - `x86_cdecl` cited as line 493 — actually line 714.
   - `x86_64_systemv` cited as line 178 — actually line 387.

   Every single CC preset line citation in this skill is wrong by
   60-300 lines. The presets have been added to since the skill was
   written and shifted the line numbers. The skill `strider-cc-preset-extend`
   has the right approach: "Locate by symbol name rather than line
   number".

2. **Step 2** lists existing `SleighArch` constructor names. All are
   present:
   - `x86_64()` at `crates/target/src/arch.rs:158`
   - `x86()` at `:169`
   - `aarch64()` at `:217`
   - `aarch64be()` at `:228`
   - `arm()` at `:206`
   - `arm_be()` at `:344`
   - `arm_thumb()` at `:323`
   - `mipsbe32()` at `:180`
   - `mipsle32()` at `:191`
   - `mipsbe64()` at `:248`
   - `mipsle64()` at `:258`
   - `ppc32be()` at `:264`
   - (skill omits `ppc32le`, `ppc64be`, `ppc64le` from the list — at
     `:277, 293, 307`).

3. **Step 6 mentions widths "1, 2, 4, 8, 10, 16 bytes"** for `vn_mask`.
   Actual: `vn_mask` at `crates/pcode-lift/src/vn_io.rs:38` accepts
   1, 2, 4, 8, 10, 16, 32, 64 (with degraded `u128::MAX` mask for
   widths > 16). Tests at lines 641, 648 confirm 32-byte (YMM) and
   64-byte (ZMM) acceptance.

Proposed edits:

- Step 3: drop all preset-specific line numbers, instead list symbol
  names and direct the reader to grep — adopt the
  `strider-cc-preset-extend` skill's approach.
- Step 2: add `ppc32le`, `ppc64be`, `ppc64le` to the constructor list.
- Step 6: update the documented widths to "1, 2, 4, 8, 10, 16, 32,
  64 bytes (>16 use degraded `u128::MAX`)".

### strider-validation-invariant-extend — NEEDS-UPDATE-MINOR

Drifts:

1. **Step 4 cites `enum ValidationError` "around line 148"** —
   actual line 156 in `crates/ir/src/validate/mod.rs`. 8-line drift.
2. **Step 2 cites `crates/ir/src/validate/layer_c.rs:234-258`** for
   the function-arg uniqueness check, **and `:224-232` for the
   reachability comment**, **and `:228`**. Verified:
   `check_layer_c_function_arg_uniqueness` is at line 234, the
   doc-comment ("Reachability-scoped — every other Layer-C
   per-node check is reachability-gated") is at 224-232, and
   line 228 sits within that doc-comment (correct). All three
   citations align with the current file.
3. **Step 2 cites `mod.rs:90`** for "Layer A is already reachability-scoped
   at the entry". Actual: the reachable-gating loop is at
   `crates/ir/src/validate/mod.rs:97`.
4. **Step 1 list** of existing Layer-C checks
   (`check_layer_c_uniqueness`, `_control_state`, `_phis`,
   `_function_arg_uniqueness`, `_wide_consts`, `_asm_fingerprints`)
   — all confirmed at lines 23, 56, 108, 234, 271, 202 respectively.

Proposed edits:

- Step 2: `mod.rs:90` → `:97`.
- Step 4: "around line 148" → "line 156".

## Proposed new skills

### strider-pcode-lift-vn-aliasing-extend

**Trigger phrase**: "register-aliasing fails for `<arch>`'s wide
register", "add support for a >16-byte container in `vn_mask`",
"strider rejects a width-N varnode read".

**File paths the skill operates on**:
- `crates/pcode-lift/src/vn_io.rs` — `vn_mask`, `find_largest_fitting_register`,
  `read_vn`, `write_vn`.
- `crates/pcode-lift/src/lib.rs` — `ValueLifter` struct.

**Step-by-step procedure**:
1. Identify the offending varnode (size, container, address space).
2. Decide whether the new width needs (a) a precise sub-register mask
   or (b) the degraded `u128::MAX` fallback (for >16-byte containers).
3. Extend `vn_mask` to accept the new width.
4. Add a unit test next to `vn_mask_for_10_bytes_is_low_80_bits`
   (line 592) verifying the new width.
5. If sub-register reads need to compose, audit
   `find_largest_fitting_register` against the largest containing
   register's width; ensure the wide-container guard still rejects
   sub-register aliasing within >16-byte containers.

**Verification step**: `cargo test --package pcode-lift vn_mask` plus
a fixture that exercises the new width.

**Exit criteria**: width is enumerated in `vn_mask`'s match arms and
covered by tests; `find_largest_fitting_register` does not silently
truncate. CLAUDE.md width list is updated to match.

(Justification: this gap was hit silently in `strider-target-arch`'s
"widths 1/2/4/8/10/16" claim — the actual code accepts 32 and 64
already, but no skill captures the procedure to extend further.)

### strider-arch-preset-dispatch-table-extend

**Trigger phrase**: "add a `target::call_other_abi` arch-specific
classifier row for the new arch I just added", "wire `ArchPreset::*`
into existing dispatch tables".

**File paths**:
- `crates/target/src/arch.rs` — `enum ArchPreset` (line 91).
- `crates/target/src/call_other_abi.rs::classify_arch_specific` (line 76).
- `crates/cfg/src/cfg/builder/mod.rs` — `Builder::for_arch` (line 149).
- `crates/strider/src/orchestrator.rs` — production call-site (line 940).

**Procedure**:
1. Identify every `match preset { ... }` site that pattern-matches
   over `ArchPreset` (`grep -rn "match.*preset\|match.*ArchPreset"`).
2. For each site, decide whether the new variant needs an arm or
   falls through to a default. Adding `#[non_exhaustive]` would help
   here but is a workspace-wide policy decision.
3. Add the new row in `classify_arch_specific` for arch-specific
   user-ops the new arch emits.
4. Confirm `Builder::for_arch` does NOT need a special case (it
   propagates `arch.preset` opaquely).
5. Run `cargo build --workspace` — `match` exhaustiveness will catch
   missed sites.

**Verification step**: lift one fixture per emitted user-op; assert
no `UnknownCallOtherError`.

**Exit criteria**: every `match` over `ArchPreset` covers the new
variant; no `UnknownCallOtherError` on a fixture for the new arch.

(Justification: `strider-target-arch` and `strider-callother-abi` are
both adjacent but neither walks the dispatch-table audit explicitly.
The two-step sequence is required to avoid the round-8 cross-arch
default-preset bug repeating on every new arch.)

### strider-py-snapshot-refresh

**Trigger phrase**: "I added a Rust API and the Python public-API
snapshot test fails", "refresh `test_public_api_snapshot.py`".

**File paths**:
- `crates/strider-py/tests/python/test_public_api_snapshot.py`.
- `crates/strider-py/src/lib.rs` — `#[pymodule]` block.

**Procedure**:
1. Build wheel: `uv run maturin develop`.
2. Run snapshot in update mode: `uv run pytest --snapshot-update
   crates/strider-py/tests/python/test_public_api_snapshot.py`.
3. Diff the snapshot file. Confirm only the intended additions appear.
   If unexpected symbols leaked, audit `#[pyclass(name = "...")]` and
   `#[pymodule]` registrations.
4. Commit the snapshot update alongside the source change.

**Verification step**: `uv run pytest crates/strider-py/tests/python/test_public_api_snapshot.py`
exits clean.

**Exit criteria**: snapshot regenerated; diff only reflects the
intended API change.

(Justification: this is a recurring task that the existing
`strider-py-binding` skill mentions but doesn't fully procedurise.
Adding a sub-skill clarifies the snapshot-update workflow.)

## Summary

| Verdict | Count |
|---------|-------|
| KEEP-AS-IS | 8 |
| NEEDS-UPDATE-MINOR | 8 |
| NEEDS-UPDATE-MAJOR | 3 |
| DELETE | 0 |

KEEP-AS-IS: strider-builder-for-arch-migration, strider-doc-line-number-refresh,
strider-fingerprint-audit, strider-pattern-author,
strider-public-api-encapsulation, strider-py-binding,
strider-rewrite-rule-multinode-audit, strider-silent-failure-audit.

NEEDS-UPDATE-MINOR: strider-callother-abi, strider-cli-runner,
strider-debug-pattern, strider-fixture-author,
strider-flagcmp-rule-author, strider-indirect-shape-author,
strider-opt-pass-author, strider-validation-invariant-extend.

NEEDS-UPDATE-MAJOR: strider-cc-preset-extend (cites a
nonexistent `pcode_lift::ValueLifter::clobbered_outputs` function as
the load-bearing implementation point of the LR-as-callee-saved
tradeoff), strider-orchestrator-extend (line numbers off by 30-100,
e.g. orchestrator.rs:837 cited but actual is :940), strider-target-arch
(every CC preset line citation in the file is wrong by 60-300 lines).
