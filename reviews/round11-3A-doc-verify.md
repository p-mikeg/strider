# Round 11 — 3A: doc-vs-code verification

## Summary

| Source | Samples | Confirmed | Partially | Refuted | Stale |
|--------|---------|-----------|-----------|---------|-------|
| CLAUDE.md | 11 | 11 | 0 | 0 | 0 |
| Root README | 4 | 4 | 0 | 0 | 0 |
| Per-crate READMEs | 9 | 7 | 1 | 1 | 0 |
| SKILL.md | 5 | 4 | 0 | 0 | 1 |
| **Total** | **29** | **26** | **1** | **1** | **1** |

## Findings

### 1. Side-tables on `Graph`
- **Source:** CLAUDE.md:63
- **Claim:** "Per-node side-tables (`stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>`, `call_other_names: SecondaryMap<NodeId, Option<String>>`, `asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>`) hold ancillary data."
- **Code under test:** `crates/ir/src/graph/mod.rs:60`, `:76`, `:101`
- **Verdict:** confirmed — three exact field declarations match: `pub(crate) stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>`, `pub(crate) call_other_names: SecondaryMap<NodeId, Option<String>>`, `pub(crate) asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>`.

### 2. Asm-fingerprint public API
- **Source:** CLAUDE.md:64
- **Claim:** "Public API on `Graph`: `asm_fingerprint(id) -> &[u64]`, `set_asm_fingerprint(id, Vec<u64>)`, `extend_asm_fingerprint(id, &[u64])`, `extend_asm_fingerprint_from(dst, src)`."
- **Code under test:** `crates/ir/src/graph/store.rs:132,144,154,184`
- **Verdict:** confirmed — all four `pub fn`s exist with the documented signatures.

### 3. Validate three-layer scheme + opt-in fingerprint check
- **Source:** CLAUDE.md:70-74
- **Claim:** "`validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` adds an opt-in Layer-C check… Default `validate` is unchanged…"
- **Code under test:** `crates/ir/src/validate/mod.rs:71-115`, `crates/ir/src/validate/layer_c.rs:199-222`
- **Verdict:** confirmed — `validate(graph, entry)` calls `validate_with_options(graph, entry, ValidateOptions::default())`, and `validate_with_options` runs `check_layer_c_asm_fingerprints` only when `options.check_asm_fingerprints` is true.

### 4. Asm-fingerprint exempt kinds
- **Source:** CLAUDE.md:64
- **Claim:** "Region / phi / initial-state kinds (`Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`, `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi`) are exempt from non-empty checks…"
- **Code under test:** `crates/ir/src/validate/layer_c.rs:184-197` (`asm_fingerprint_exempt`)
- **Verdict:** confirmed — exact 9-kind set matches the doc list.

### 5. NodeOutputType variants
- **Source:** CLAUDE.md:67
- **Claim:** "`NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U80` (x87 80-bit extended), `U128`, `U256`, `U512`; floats `F32`, `F64`, `F80`."
- **Code under test:** `crates/ir/src/node/output_type.rs:9-39`
- **Verdict:** confirmed — enum order: `Bool`, `U8`, `U16`, `U32`, `U64`, `U80`, `U128`, `U256`, `U512`, `F32`, `F64`, `F80`.

### 6. `IntConst(u128)` rejects U256/U512
- **Source:** CLAUDE.md:67
- **Claim:** "Wide types (`U256` / `U512`) are stored via `IntConstWide(WideConstId)` interned in `Graph::wide_consts`; `IntConst(u128)` rejects them."
- **Code under test:** `crates/ir/src/builder/nodes.rs:86-99`
- **Verdict:** confirmed — `build_int_const` returns an error string `"build_int_const({output_type:?}) not supported - IntConst storage is u128; use build_int_const_wide for U256/U512"` when called with U256/U512.

### 7. `vn_mask` width support
- **Source:** CLAUDE.md:147
- **Claim:** "`vn_mask` enumerates supported widths: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register), 32 (YMM), 64 (ZMM) bytes. Widths > 16 use a degraded `u128::MAX` mask…"
- **Code under test:** `crates/pcode-lift/src/vn_io.rs:38-46`
- **Verdict:** confirmed — match arms exactly cover 1, 2, 4, 8, 10, and the `16 | 32 | 64 => Ok(u128::MAX)` arm.

### 8. Optimizer pipeline composition (default / stable / destructive)
- **Source:** CLAUDE.md:89
- **Claim:** "`default_pipeline()` (all 6 base passes), `stable_default_pipeline()` (rewrites that survive phi-input growth — `ConstantFold` + `KnownBits` + `FlagCmpCanonicalize` + `IfCondInversion`), `destructive_default_pipeline()` (node-removal passes safe only at fixed point — `RedundantPhis` + `DeadBranchElimination`)."
- **Code under test:** `crates/opt/src/lib.rs:123-202`
- **Verdict:** confirmed — `default_pipeline` adds 6 passes (ConstantFold, KnownBits, FlagCmpCanonicalize, IfCondInversion, RedundantPhis, DeadBranchElimination), `stable_default_pipeline` adds the first 4, `destructive_default_pipeline` adds the last 2.

### 9. `Decision { FixedPoint, StableOnly, Rebuild }` enum
- **Source:** CLAUDE.md:82
- **Claim:** "The fixed-point loop is implemented as a small `LoopState` returning a `Decision { FixedPoint, StableOnly, Rebuild }` per step."
- **Code under test:** `crates/strider/src/orchestrator.rs:187-197`
- **Verdict:** confirmed — `enum Decision { FixedPoint, StableOnly, Rebuild }` with exactly these three variants.

### 10. `IntCmpOp` variant set (no LessEqual / SlessEqual / Borrow)
- **Source:** CLAUDE.md:139
- **Claim:** "`IntCmpOp` (`Equal`, `Less`, `Sless`, `Carry`, `Scarry`, `Sborrow`; no `LessEqual` / `SlessEqual` / `Borrow`)"
- **Code under test:** `crates/ir/src/ops/op_kinds.rs` `pub enum IntCmpOp`
- **Verdict:** confirmed — variants are `Equal`, `Sless`, `Less`, `Carry`, `Scarry`, `Sborrow`, with explicit doc comments stating LessEqual/SlessEqual are lift-time-lowered and that `Less` doubles as the unsigned-borrow predicate.

### 11. CallOther classification API
- **Source:** CLAUDE.md:95
- **Claim:** "`target::call_other_abi::classify(preset, name)` (in the `target` crate) — single-source-of-truth `(ArchPreset, name) → CallOtherClass {NoOp, NoReturn, Call(CallOtherAbi)}` table"
- **Code under test:** `crates/target/src/call_other_abi.rs:35-61` (`pub enum CallOtherClass { NoOp, NoReturn, Call(CallOtherAbi) }` and `pub fn classify(preset, name) -> Option<CallOtherClass>`)
- **Verdict:** confirmed — exact enum + function signature.

### 12. Quickstart `apply_elf_relocations` autoload
- **Source:** README.md:71 (and 57)
- **Claim:** "`mem.apply_elf_relocations(path)` applies dynamic relocations and *autoloads* any missing site sections (e.g. `.got.plt`) before applying."
- **Code under test:** `crates/strider-py/src/reader.rs:320-326`, `crates/reader/src/elf.rs:751-772` (`apply_elf_relocations_autoload`)
- **Verdict:** confirmed — Python `apply_elf_relocations` calls Rust `apply_elf_relocations_autoload` which lazy-loads sections via `find_loadable_section_containing` for any uncovered site address.

### 13. Root README install path
- **Source:** README.md:50-55
- **Claim:** "uv sync --group dev / uv run maturin develop / uv run pytest" workflow
- **Code under test:** `crates/strider-py/pyproject.toml`, `crates/strider-py/README.md:14-21`
- **Verdict:** confirmed — strider-py README mirrors the same three-command sequence and references `[dependency-groups].dev` (PEP 735).

### 14. Root README quickstart `strider.run` parameters
- **Source:** README.md:76-83
- **Claim:** Calling `strider.run(arch=…, cc=…, mem=…, rom=…, entry=…, function_max_size=…, allow_code_before_start_addr=…)`
- **Code under test:** `crates/strider-py/src/run.rs:40-69` (`pyfunction` decorator + `pub fn run`)
- **Verdict:** confirmed — function takes arch, cc, mem, entry, rom, pipeline, allow_code_before_start_addr, function_max_size, compact, per_address_ccs; default-arg signature matches the README usage.

### 15. Root README pattern table — IndirectBranchResolve note
- **Source:** README.md:221
- **Claim:** "`IndirectBranchResolve`… implements the `Optimizer` trait but is instantiated *directly* by the strider orchestrator, not registered in any of the three named pipelines above."
- **Code under test:** `crates/opt/src/lib.rs:193-202` (no `IndirectBranchResolve` registered), `crates/strider/src/orchestrator.rs:179-181` (orchestrator `LoopState::run_stable_only`/`rebuild`)
- **Verdict:** confirmed — none of `default_pipeline`, `stable_default_pipeline`, `destructive_default_pipeline` add `IndirectBranchResolve`; orchestrator drives it directly via `classify_anchor` + inplace helpers.

### 16. cfg README — `Builder::for_arch` preferred
- **Source:** crates/cfg/README.md:19
- **Claim:** "`Builder::for_arch(arch, sleigh, start_addr, options)` — **preferred**: derives both endianness and `ArchPreset` from a `target::SleighArch` atomically."
- **Code under test:** `crates/cfg/src/cfg/builder/mod.rs:103,122,149` — `new` and `with_endianness` are `#[deprecated]` while `for_arch` carries no deprecation
- **Verdict:** confirmed — `Builder::new` and `with_endianness` both have `#[deprecated]` attributes pointing callers at `for_arch`; the latter sets `endianness: arch.endianness, preset: arch.preset`.

### 17. ir README — `walk` exports
- **Source:** crates/ir/README.md:34
- **Claim:** "`walk::{walk_graph, cfg_reachable, GraphWalk, NodeIdSet}`"
- **Code under test:** `crates/ir/src/walk.rs:13` (`pub type NodeIdSet = …`), `:26` (`pub fn cfg_reachable`), `:114` (`pub type GraphWalk<'a> = PreOrder<…>`), `:123` (`pub fn walk_graph`)
- **Verdict:** confirmed — all four symbols are public, with `GraphWalk` being a type alias and `NodeIdSet` a type alias as well.

### 18. ir README — `IntConst(u128)` line 51 / Sub not primitive
- **Source:** CLAUDE.md:139, crates/ir/README.md:50-52
- **Claim:** "`IntBinaryOp` (no `Sub`; lifter lowers to `Add(_, Neg(_))`)"
- **Code under test:** `crates/ir/src/ops/op_kinds.rs` `pub enum IntBinaryOp` (no `Sub` variant); `crates/pattern/src/pat/ctor/int.rs:56-60` (`pub fn sub(l, r) -> Pat { Add(l, Neg(r)) }` lowering)
- **Verdict:** confirmed.

### 19. opt README — `OptimizationResult` enum
- **Source:** crates/opt/README.md:16
- **Claim:** "`OptimizationResult` — `Changed | NoChange` (both unit variants)."
- **Code under test:** `crates/opt/src/pipeline.rs:6-10` `pub enum OptimizationResult { NoChange, Changed }`
- **Verdict:** confirmed — both variants are unit; declaration order is `NoChange, Changed` (the doc reverses them but that's narrative).

### 20. pattern README — `find_all_requirements` join semantics
- **Source:** crates/pattern/README.md:69-74
- **Claim:** "`find_all_requirements(&[pat1, pat2, …])` … returns the cross-product of their matches, filtered to tuples whose **shared captures** … bind to the same `Binding`"
- **Code under test:** `crates/pattern/src/matcher/mod.rs:449-486`
- **Verdict:** confirmed — the impl seeds with `per_pat[0]` matches, iteratively cross-products with each subsequent pattern's matches, and filters via `prefix_agrees(prefix, m)` which checks shared-capture binding agreement.

### 21. pattern README — `Match` accessors
- **Source:** crates/pattern/README.md:18-21
- **Claim:** "typed extractors `get_int(c, &graph)`, `get_uint(c, &graph)`, `get_bool(c, &graph)`, `get_float_bits(c, &graph)`, `get_vn(c, &graph)`, `stack_offset(c, &graph)`, `stack_phi_offsets(c, &graph)`, `asm_fingerprint(c, &graph)`"
- **Code under test:** `crates/pattern/src/matcher/match_result.rs:64-310` — every listed accessor exists as a `pub fn`.
- **Verdict:** confirmed.

### 22. target README — `ArchPreset` variant names
- **Source:** crates/target/README.md:10-12
- **Claim:** "`ArchPreset` — `X8664`, `X86`, `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`, `ArmThumb`, `Mipsbe32`, `Mipsle32`, `Mipsbe64`, `Mipsle64`, `Ppc32Be`, `Ppc32Le`, `Ppc64Be`, `Ppc64Le`."
- **Code under test:** `crates/target/src/arch.rs:91-107`
- **Verdict:** **refuted** — the actual enum spells the variants as `X86_64` (not `X8664`), `MipsBe32` (not `Mipsbe32`), `MipsLe32` (not `Mipsle32`), `MipsBe64`, `MipsLe64`. The MIPS naming-case discrepancy in particular would mislead a contributor copying the README into Rust source.
- **Proposed doc edit:** rewrite the bullet to "`X86_64`, `X86`, `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`, `ArmThumb`, `MipsBe32`, `MipsLe32`, `MipsBe64`, `MipsLe64`, `Ppc32Be`, `Ppc32Le`, `Ppc64Be`, `Ppc64Le`."

### 23. target README — link-register handling note (callee-saved x30/lr/LR)
- **Source:** CLAUDE.md:79
- **Claim:** "`aarch64_aapcs64` lists `x30` in `callee_saved_regs`, `arm_aapcs` lists `lr`, and the PowerPC presets list `LR`."
- **Code under test:** `crates/target/src/calling_convention/mod.rs` `aarch64_aapcs64` (`callee_saved_regs: …, "x30"`) and `powerpc_sysv32` (`callee_saved_regs: …, "LR"`)
- **Verdict:** confirmed.

### 24. pcode-lift README — public surface lists `require_output_vn`
- **Source:** crates/pcode-lift/README.md:31-33
- **Claim:** "`first_input_or_err(insn)` / `decode_space_id(insn)` / `require_output_vn(insn)` — small shared helpers used by the per-opcode handlers."
- **Code under test:** `crates/pcode-lift/src/lib.rs:93` (`pub(crate) fn require_output_vn`), `:117` (`pub fn first_input_or_err`), `:138` (`pub fn decode_space_id`)
- **Verdict:** **partially-confirmed** — `first_input_or_err` and `decode_space_id` are `pub`, but `require_output_vn` is only `pub(crate)`. Since the README lists it under the "Public surface" header alongside truly public items, this is a small surface-mismatch.
- **Proposed fix (either):** (a) move `require_output_vn` from the public-surface bullet to a "Crate-private helpers" sentence, or (b) promote the function to `pub` if external users need it (no current external user found).

### 25. strider README — `RunConfig` fields
- **Source:** crates/strider/README.md:14-17
- **Claim:** "`RunConfig<'a, R>` — input bundle: `strider: &Strider`, `start_addr: u64`, owned `sleigh: rsleigh::Sleigh<R>`, optional `rom: Arc<dyn ReadOnlyMemory>`, `fn_max_size: Option<u64>`, `allow_code_before_start_addr: bool`, `compact: bool`, `per_address_ccs: HashMap<u64, CallingConvention>`."
- **Code under test:** `crates/strider/src/orchestrator.rs:60-105`
- **Verdict:** confirmed — every listed field maps 1:1 to a `pub` field on `RunConfig`.

### 26. SKILL strider-pattern-author — file paths
- **Source:** crates/strider/.claude/skills/strider-pattern-author/SKILL.md:27,29,30
- **Claim:** Cites `crates/pattern/src/pat/builders/call.rs`, `…/ret.rs`, `…/memory.rs`, `…/branch.rs`, `…/phi.rs`, plus `crates/pattern/src/var.rs` for `Capture`, and `crates/pattern/src/matcher/mod.rs` for `Matcher::find_all_*`.
- **Code under test:** all six referenced files exist; `find_all`, `find_all_multi`, `find_all_requirements`, `match_at` all live in `crates/pattern/src/matcher/mod.rs`.
- **Verdict:** confirmed.

### 27. SKILL strider-callother-abi — `build_call_other_*` IR builders
- **Source:** crates/strider/.claude/skills/strider-callother-abi/SKILL.md:27
- **Claim:** "`NoReturn` … emitted via `ir::FunctionBuilder::build_call_other_terminal`… `Call(CallOtherAbi { ... })` … emits via `ir::FunctionBuilder::build_call_other_modeled`."
- **Code under test:** `crates/ir/src/builder/call.rs:192` (`pub fn build_call_other_terminal`), `:279` (`pub fn build_call_other_modeled`)
- **Verdict:** confirmed.

### 28. SKILL strider-opt-pass-author — `OptimizerOnBuilt` blanket impl + extend_asm_fingerprint_from cite
- **Source:** crates/strider/.claude/skills/strider-opt-pass-author/SKILL.md:25,30
- **Claim:** "the blanket `impl<T: OptimizerOnBuilt> Optimizer for T` adapts via `with_rewrite_ctx`… `Graph::extend_asm_fingerprint_from(new, contributor)` (`crates/ir/src/graph/store.rs:184`)."
- **Code under test:** `crates/opt/src/pipeline.rs:141-165` (`pub trait OptimizerOnBuilt` + `impl<T: OptimizerOnBuilt> Optimizer for T`); `crates/ir/src/graph/store.rs:184` `pub fn extend_asm_fingerprint_from`.
- **Verdict:** confirmed — the cited line numbers are accurate and the blanket impl exists exactly as described.

### 29. SKILL strider-target-arch — line numbers for CC presets
- **Source:** crates/strider/.claude/skills/strider-target-arch/SKILL.md:28
- **Claim:** "Mirror the structure of `aarch64_aapcs64` (line 249), `arm_aapcs` (line 289), `mips_o32` (line 330), `mips_n64` (line 364), `x86_cdecl` (line 493), or `x86_64_systemv` (line 178)."
- **Code under test:** `crates/target/src/calling_convention/mod.rs` actual lines: `x86_64_systemv: 387`, `aarch64_aapcs64: 458`, `arm_aapcs: 498`, `mips_o32: 539`, `mips_n64: 573`, `x86_cdecl: 714`.
- **Verdict:** **stale** — the file has grown since the line numbers were captured (every cited line is off by 200+ lines). A user following the skill verbatim would be misled to a totally different function or to mid-body code.
- **Proposed doc edit:** drop hard-coded line numbers and reference functions by name only, e.g. "Mirror the structure of `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`, `x86_cdecl`, or `x86_64_systemv` (all in `crates/target/src/calling_convention/mod.rs`)." Alternatively, run a `strider-doc-line-number-refresh`-style sweep.
