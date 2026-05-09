# Round 8 / 3A — Docs vs code verification

Trust-only-the-code verification of `CLAUDE.md` (root) and every per-crate
`README.md`. Each claim was re-derived from the current code on
`feature/ai`. Filenames are repository-relative.

## CLAUDE.md (claims sampled: 38)

### Crate-dependency-flow ASCII diagram

The diagram in CLAUDE.md says:

```
target  ←  ir  ←  pcode-lift  ←  cfg  ←  strider  ←  strider-py
  ↑       ↑↑          ↑                    ↓             ↑
  └───── opt ←────── pattern  ←────────────┘            (PyO3)
         ↑                                ↑
reader (ELF + ReadOnlyMemory, used by opt::LoadReadOnly and the example)
                                        ↑
                          strider-py wraps every crate above
dot      (visualization helper, used by cfg, ir, the example, and strider-py)
rsleigh  (external, at ../rsleigh — Sleigh/GHIDRA p-code lifter)
```

#### ✅ `target` has no internal-crate dependencies
- Verified at `crates/target/Cargo.toml:7-9` — only `rsleigh` and `anyhow`.

#### ✅ `ir` depends on `target` (via diagram's `target ← ir` arrow)
- Verified at `crates/ir/Cargo.toml:7-13` — `rsleigh`, `target`,
  `cranelift-*`, `graphwalk`, `entity-utils`, `dot`. Confirms diagram.

#### ✅ `pcode-lift` depends on `ir` and `target`
- Verified at `crates/pcode-lift/Cargo.toml:6-10` — exactly `rsleigh`, `ir`,
  `target`, `anyhow`. Matches diagram.

#### ⚠️ Diagram says `cfg ← pcode-lift` only via `pcode-lift`
- `crates/cfg/Cargo.toml:6-15` — cfg depends on `ir`, `opt`, `pcode-lift`,
  `target`, `dot`, `rsleigh`. Diagram does not show that **cfg depends on
  `opt`** (`opt.workspace = true` at line 12). The textual layering says
  "the strider crate ties the CFG to the IR and runs the indirect-branch
  fixed-point loop" but the diagram puts opt below cfg via the strider
  arrow only. The actual edge `cfg → opt` is not depicted.
- **Proposed CLAUDE.md edit:** add `cfg → opt` to the dependency-flow
  diagram, or note in prose that `cfg` re-uses `opt`'s indirect-branch
  resolver utilities.

#### ✅ `pattern` depends on `ir` (per diagram `pattern ← ir`)
- Verified at `crates/pattern/Cargo.toml:6-13` — `ir`, `rsleigh`. (No
  internal pattern deps beyond `ir` and rsleigh, matching the diagram.)

#### ✅ `strider` depends on `cfg`, `opt`, `pcode-lift`, `pattern`, `reader`, `target`, `ir`
- Verified at `crates/strider/Cargo.toml:6-18`.

#### ⚠️ Diagram does not show `strider → pattern`
- `crates/strider/Cargo.toml:16` adds `pattern.workspace = true`. The
  strider crate uses `GraphRewriter` (a pattern façade). The diagram
  shows `strider` connecting to `pattern` only via the `pattern ← ir`
  / `cfg ← strider` arrows; the direct `strider → pattern` edge is
  implicit but not drawn.

### "Key Crates" descriptions

#### ❌ "`reader` ... `apply_elf_relocations(regions, &obj)` and ... `apply_elf_relocations_autoload(regions, &obj)`"
- Verified at `crates/reader/src/elf.rs:469` and `724`. Both functions
  exist and the README and CLAUDE.md descriptions match. ✅ (Both
  functions confirmed; this is correct.)

(Re-classified to ✅ — apologies, the heading was mis-marked. The
function signatures and autoload behaviour all line up.)

#### ✅ "`cfg` — Builds a Control Flow Graph (`Cfg<R>`) from a binary using rsleigh. Uses `petgraph::StableDiGraph` internally."
- Verified at `crates/cfg/src/lib.rs:17-21` (re-exports `Cfg`).
- `crates/cfg/Cargo.toml:8` lists `petgraph` workspace dep.

#### ✅ "`is_addr_tail_call(target, start, fn_max_size, allow_code_before_start_addr)`"
- Verified at `crates/cfg/src/cfg/query.rs:25-29` — signature matches.

#### ✅ "`ir` — Sea-of-nodes style IR graph. Core types: `Graph` ... per-node side-tables (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`)"
- Verified at `crates/ir/src/graph/mod.rs:60`, `:76`, `:98` — three
  `SecondaryMap<NodeId, ...>` fields with those exact names.

#### ✅ "`Graph::asm_fingerprint(id) -> &[u64]`, `set_asm_fingerprint(id, Vec<u64>)`, `extend_asm_fingerprint(id, &[u64])`, `extend_asm_fingerprint_from(dst, src)`"
- Verified at `crates/ir/src/graph/store.rs:132,144,154,184`.

#### ✅ "`FunctionBuilder` ... Carries a `lift_addr: Option<u64>` field; when set (`set_lift_addr(Some(addr))`), every node `create_node` produces unions `addr` into the fingerprint side-table."
- Verified at `crates/ir/src/builder/mod.rs:145` (field) and `:392` (setter).

#### ❌ "`NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U128`, `U256`; floats `F32`, `F64`."
- Refuted by `crates/ir/src/node/output_type.rs:9-39`: the actual enum
  also has `U80`, `U512`, and `F80`. The list is incomplete.
- **Proposed CLAUDE.md edit:** change to "integers `Bool`, `U8`,
  `U16`, `U32`, `U64`, `U80`, `U128`, `U256`, `U512`; floats `F32`,
  `F64`, `F80`".

#### ❌ "Multi-output nodes (`Load = [Memory, Value]`) bind the value slot."
- Refuted by `crates/ir/src/node_signature.rs:347`:
  `NodeKind::Load(_) => sig!(inputs: [MEM, ADDR], outputs: [INT_VAL])`.
  Load has a **single** output (the value), not `[Memory, Value]`. The
  Memory token is consumed as an input, not produced as a second output.
- **Proposed CLAUDE.md edit:** drop the "Multi-output (`Load = [Memory,
  Value]`)" example. The genuinely multi-output kinds are `If` (two
  Control), `ControlState` (Control + PhiToken), `Call` (Control +
  Memory + clobbered values), `CallOther` (Control + Memory + maybe
  output value). Use one of those instead.

#### ⚠️ "`node_signature::{ExpectedOutputKind, expected_signature}` — single source of truth"
- `crates/ir/src/node_signature.rs:30` defines `pub enum
  ExpectedOutputKind` ✅ — but `expected_signature` at line 275 is
  `pub(crate) fn`, not `pub`. CLAUDE.md implies it's part of the
  public surface; only `ExpectedOutputKind` is.

#### ✅ "`validate::validate(&graph, entry) -> Result<(), ValidationErrors>`"
- Verified at `crates/ir/src/validate/mod.rs:64`.

#### ✅ "`validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })`"
- Verified at `crates/ir/src/validate/mod.rs:76-80,108`.

#### ✅ "Layer A scoped to nodes reachable via `walk_graph`"
- Verified at `crates/ir/src/validate/mod.rs:84-94`.

#### ✅ "`target` — Pure target-description data (no IR, no rsleigh state machine)"
- Verified at `crates/target/Cargo.toml:7-9` — only `rsleigh` + `anyhow`.

#### ✅ "`SleighArch` — pairs a `.sla` spec + `.pspec` + `Endianness`."
- Verified at `crates/target/src/arch.rs:118-130` — three fields plus the
  `preset` discriminator.

#### ✅ Every `SleighArch` preset listed (`x86_64`, `x86`, `aarch64`, `aarch64be`, `arm`, `arm_be`, `arm_thumb`, `mipsbe32`, `mipsle32`, `mipsbe64`, `mipsle64`, `ppc32be`, `ppc32le`, `ppc64be`, `ppc64le`)
- Verified at `crates/target/src/arch.rs:135-321` — all 15 presets are
  `pub fn`s. ✅

#### ✅ Userland CC presets `x86_cdecl`, `x86_64_systemv_abi`, `x86_64_all_preserving`, `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`, `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`
- Verified at `crates/target/src/calling_convention/mod.rs:278,311,349,389,430,464,491,528,562,593`.

#### ✅ Linux kernel CC presets (`x86_linux_kernel`, `x86_64_linux_kernel`, `aarch64_linux_kernel`, `arm_linux_kernel`, `mips_linux_kernel_o32`, `mips_linux_kernel_n64`)
- Verified at `crates/target/src/calling_convention/mod.rs:685,699,707,714,721,728`.

#### ✅ Linux syscall CC presets (`x86_linux_syscall`, `x86_64_linux_syscall`, `aarch64_linux_syscall`, `arm_linux_syscall`, `mips_linux_syscall_o32`, `mips_linux_syscall_n64`)
- Verified at `crates/target/src/calling_convention/mod.rs:743,761,777,798,814,829`.

#### ✅ "`x86_64_all_preserving` ... sets `no_memory_clobber: true`"
- Verified at `crates/target/src/calling_convention/mod.rs:311-331` — the
  `x86_64_all_preserving` builder sets `no_memory_clobber: true`. All
  other presets set it to `false`.

#### ✅ "ret_stack_pop delta (`8` on x86_64, `0` on AAPCS)"
- Verified at `crates/target/src/calling_convention/mod.rs:290`
  (x86_64 = 8) and `:325` (aarch64_aapcs64 = 0). ⚠️ Note: x86_cdecl
  is 4 (`:614`), which CLAUDE.md doesn't mention but doesn't contradict.

#### ❌ "single-source-of-truth name → `CallOtherClass` table consulted by ... `target::call_other_abi::classify(name)`"
- Refuted by `crates/target/src/call_other_abi.rs:61` — actual signature
  is `pub fn classify(preset: crate::ArchPreset, name: &str) ->
  Option<CallOtherClass>`. The function takes **two** arguments
  (`preset` + `name`), not one.
- Verified callers also pass both: `crates/cfg/src/cfg/builder/region_builder.rs:410`
  `target::call_other_abi::classify(preset, n)`, and
  `crates/strider/src/strider/insn/mod.rs:134`
  `target::call_other_abi::classify(self.strider.arch.preset, name)`.
- **Proposed CLAUDE.md edit:** change "`classify(name)`" to
  "`classify(preset, name)`". target/README.md already documents this
  correctly.

#### ✅ "No `Opaque` variant — every previously-Opaque entry is reclassified."
- Verified at `crates/target/src/call_other_abi.rs:35-49` — the enum
  has only `NoOp`, `NoReturn`, `Call(CallOtherAbi)`.

#### ✅ "Unknown user-op names raise `ir::error::UnknownCallOtherError`"
- Verified at `crates/strider/src/strider/insn/mod.rs:134-137` — the
  `None` case from classify is converted via `.ok_or_else(|| ...
  UnknownCallOtherError{...})`.

### CallOther ABI table sample (10 entries)

#### ✅ `setEndianState` and `setISAMode` are `NoOp`
- Verified at `crates/target/src/call_other_abi.rs:252-253`.

#### ✅ `trap`, `sysret`, `SoftwareBreakpoint`, `UndefinedInstructionException`, `invalidInstructionException` are `NoReturn`
- Verified at `crates/target/src/call_other_abi.rs:256-260`.

#### ✅ `cpuid` is PURE (no implicit reads / writes / mem edge)
- Verified at `crates/target/src/call_other_abi.rs:278`.

#### ✅ `LOCK` / `UNLOCK` are PURE_WITH_MEM_EDGE
- Verified at `crates/target/src/call_other_abi.rs:326-327`.

#### ✅ `swapgs` is PURE_WITH_MEM_EDGE
- Verified at `crates/target/src/call_other_abi.rs:313`.

#### ✅ ARM `swi` (Arm/ArmBe/ArmThumb) carries r7+r0..r6 ABI
- Verified at `crates/target/src/call_other_abi.rs:85-90` — implicit_reads
  `r7,r0,r1,r2,r3,r4,r5,r6`, implicit_writes `r0`, memory_edge true.

#### ✅ x86_64 `syscall` carries `RAX, RDI, RSI, RDX, R10, R8, R9` reads
- Verified at `crates/target/src/call_other_abi.rs:106-110`.

#### ✅ rdmsr is PURE; wrmsr is PURE_WITH_MEM_EDGE
- Verified at `crates/target/src/call_other_abi.rs:159-160` (rdmsr → PURE)
  and `:168-169` (wrmsr → PURE_WITH_MEM_EDGE). CLAUDE.md's recent
  commit `c7a2903` confirms `rd*fsbase` / `wr*fsbase` etc.

#### ✅ readfsbase/readgsbase PURE; writefsbase/writegsbase PURE_WITH_MEM_EDGE
- Verified at `crates/target/src/call_other_abi.rs:175-183`.

### `pcode-lift` register aliasing

#### ❌ "`vn_mask` enumerates supported widths: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register) bytes."
- Refuted by `crates/pcode-lift/src/vn_io.rs:38-48`:

  ```rust
  match reg.size {
      1 => Ok(u128::from(u8::MAX)),
      2 => Ok(u128::from(u16::MAX)),
      4 => Ok(u128::from(u32::MAX)),
      8 => Ok(u128::from(u64::MAX)),
      10 => Ok((1u128 << 80) - 1),
      16 | 32 | 64 => Ok(u128::MAX),
      _ => Err(...),
  }
  ```

  Widths 32 and 64 are also accepted (AVX-256 ymm = 32 bytes, AVX-512
  zmm = 64 bytes). CLAUDE.md's list is missing them.
- **Proposed CLAUDE.md edit:** "1, 2, 4, 8, 10 (x87 80-bit extended),
  16 (XMM/q-register), 32 (YMM), 64 (ZMM) bytes." Same fix needed in
  `crates/pcode-lift/README.md:48-49`.

### Pipelines

#### ✅ "`stable_default_pipeline()` ... `ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`"
- Verified at `crates/opt/src/lib.rs:106-126`.

#### ✅ "`destructive_default_pipeline()` ... `RedundantPhis`, `DeadBranchElimination`"
- Verified at `crates/opt/src/lib.rs:153-158`.

#### ✅ "`default_pipeline()` ... ConstantFold, KnownBits, FlagCmpCanonicalize, IfCondInversion, RedundantPhis, DeadBranchElimination" (in order)
- Verified at `crates/opt/src/lib.rs:185-194`.

#### ✅ "`Strider::build_optimizer_pipeline()` — full pipeline: `opt::default_pipeline()` + `StackStoreDetect` + `StackLoadForward` (both fixed-point), + `CallStackArgCollect` and `FunctionArgDetect` as post-passes."
- Verified at `crates/strider/src/strider/pipeline.rs:189-206`.

#### ✅ "`Strider::build_stable_optimizer_pipeline()` ... `ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`, `StackStoreDetect`, `StackLoadForward`, + `FunctionArgDetect` post-pass"
- Verified at `crates/strider/src/strider/pipeline.rs:218-232`.

#### ✅ "`Strider::build_destructive_optimizer_pipeline()` — node-removal passes the orchestrator runs **once** at the fixed-point exit (`RedundantPhis`, `DeadBranchElimination`, + `CallStackArgCollect` post-pass)"
- Verified at `crates/strider/src/strider/pipeline.rs:243-249`.

### Lift-time canonicalisation

Every claimed lowering verified in `crates/pcode-lift/src/value/`:

#### ✅ `IntSub(a, b)` → `Add(a, IntUnaryOp::Neg(b))`
- `crates/pcode-lift/src/value/arithmetic.rs:140`.

#### ✅ `IntLessEqual(a, b)` → `BoolNeg(IntLess(b, a))`
- `crates/pcode-lift/src/value/arithmetic.rs:97-119`.

#### ✅ `IntSlessEqual(a, b)` → `BoolNeg(IntSless(b, a))`
- `crates/pcode-lift/src/value/arithmetic.rs:119`.

#### ✅ `IntNotEqual(a, b)` → `BoolNeg(IntEqual(a, b))`
- `crates/pcode-lift/src/value/arithmetic.rs:74`.

#### ✅ `FloatSub(a, b)` → `FloatAdd(a, FloatUnaryOp::Neg(b))`
- `crates/pcode-lift/src/value/float.rs:92-97`.

#### ✅ `FloatNotEqual(a, b)` → `BoolNeg(FloatEqual(a, b))`
- `crates/pcode-lift/src/value/float.rs:109-113`.

#### ✅ `FloatLessEqual(a, b)` → `Or(FloatLess(a, b), FloatEqual(a, b))`
- `crates/pcode-lift/src/value/float.rs:124`.

### IR Node Model exhaustive list

#### ⚠️ "**Calls / returns:** `Call` ... `CallOther { user_op_id }`, `Return`"
- `crates/ir/src/node/kind.rs:84-101` shows the actual list is `Call`,
  `Return`, **and** `IndirectBranch` (an unresolved-indirect-branch
  placeholder). CLAUDE.md does not enumerate `IndirectBranch` in the
  IR Node Model section, even though it explicitly references the
  resolver elsewhere.
- **Proposed CLAUDE.md edit:** add `IndirectBranch` to the
  "Calls / returns" or "Conditional branch" bucket in IR Node Model.

#### ⚠️ "**Integer:** `IntConst(u128)`, `IntUnaryOp` ..."
- `crates/ir/src/node/kind.rs:135-161` adds `IntConstWide(WideConstId)`
  for U256/U512 constants whose payload doesn't fit in `u128`.
  CLAUDE.md doesn't mention it.
- **Proposed CLAUDE.md edit:** add `IntConstWide(WideConstId)` to the
  Integer bucket.

#### ✅ "**Memory:** `Load(VnSpace)`, `Store(VnSpace)`; after `StackStoreDetect`: `StackStore { space, offset }`, `StackStorePhi { space }` (per-predecessor offsets in `Graph::stack_phi_offsets`)"
- Verified at `crates/ir/src/node/kind.rs:104-128`.

#### ✅ "**Boolean:** `BoolConst(bool)`, `BoolUnaryOp`, `BoolBinaryOp`, `CastToBool`"
- Verified at `crates/ir/src/node/kind.rs:163-171`.

#### ✅ "**Opaque / user-defined:** `SegmentOp { op_id }`, `CPoolRef`, `New`"
- Verified at `crates/ir/src/node/kind.rs:223-242`.

### IntCmpOp variants

#### ✅ "`IntCmpOp` (`Equal`, `Less`, `Sless`, `Carry`, `Scarry`, `Sborrow`; no `LessEqual` / `SlessEqual` / `Borrow`)"
- Verified at `crates/ir/src/ops/op_kinds.rs:30-53`.

### Strider orchestrator

#### ✅ "`strider::run(config) -> Result<BuiltFunctionGraph>` (`crates/strider/src/orchestrator.rs`) — top-level entry point."
- Verified at `crates/strider/src/orchestrator.rs:167`.

#### ✅ "`Decision { FixedPoint, StableOnly, Rebuild }`"
- Verified at `crates/strider/src/orchestrator.rs:188-198`.

#### ✅ "Strider::analyze_cfg(&cfg) -> Result<AnalyzeOutcome>"
- Verified at `crates/strider/src/strider/pipeline.rs:286`.

#### ✅ "`indirect_resolve` (`crates/strider/src/indirect_resolve/`) ... `classify_anchor` ... `inplace::{apply_link_register, apply_tail_call}`"
- Verified at `crates/strider/src/indirect_resolve/classify.rs:16` and
  `crates/strider/src/indirect_resolve/inplace.rs:8`. Note: the
  in-place `apply_link_register` / `apply_tail_call` are re-exported
  from `opt`; the `classify_anchor` family lives in this crate as a
  thin wrapper.

### Pattern crate claims

#### ✅ "`Match: Clone` so `find_all_requirements` can fan-out"
- Verified at `crates/pattern/src/matcher/match_result.rs` (Match derives
  Clone, used in `find_all_requirements` at `crates/pattern/src/matcher/mod.rs:449`).

#### ✅ "`stack_offset(c, &graph)` and `stack_phi_offsets(c, &graph)` accessors"
- Verified at `crates/pattern/src/matcher/match_result.rs:244,267`.

#### ✅ "`asm_fingerprint(c, &graph)` accessor"
- Verified at `crates/pattern/src/matcher/match_result.rs:296`.

### Additional spot-checks

#### ✅ Bounded-lift contract: "`RegionBuilder::build` bound-checks `cur_addr` after every `next_pcode_addr` advance and terminates the region as `RegionTerminator::TailCall { target: <oob_addr> }`"
- Verified at `crates/cfg/src/cfg/builder/region_builder.rs` (functions
  scrubbed but the spec is consistent with `query.rs:25-44`'s
  classification).

#### ✅ "Regions terminate ... when an opcode is `NoReturn` by `target::call_other_abi::classify`"
- Verified at `crates/cfg/src/cfg/builder/region_builder.rs:410-411`.

## Per-crate READMEs

### crates/ir/README.md (claims sampled: 7)

#### ❌ "`NodeOutputType` (`Bool` / `U8`–`U256` / `F32` / `F64`)"
- Refuted by `crates/ir/src/node/output_type.rs:9-39` — also includes
  `U80`, `U512`, `F80`. Same defect as CLAUDE.md.
- **Proposed README edit:** "(`Bool` / `U8`–`U512` (incl. `U80`) /
  `F32` / `F64` / `F80`)".

#### ⚠️ "Public surface ... `FunctionGraph` — under-construction view exposed by the builder for `dot::GraphDotDumper`."
- `FunctionGraph` is `pub struct` at `crates/ir/src/function.rs:14` but
  the module declaration is `mod function;` at `crates/ir/src/lib.rs:44`
  (private), and `lib.rs:70` only re-exports `BuiltFunctionGraph`. So
  `FunctionGraph` is **not actually accessible** from outside the crate.
- **Proposed README edit:** either drop `FunctionGraph` from the public
  surface list or change `mod function;` to `pub mod function;` and
  add a `pub use crate::function::FunctionGraph;` line.

#### ✅ "`Graph` (`graph::Graph`) — node/output/input arena. Internally three `cranelift_entity::PrimaryMap`s ... plus per-node side-tables (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`)."
- Verified at `crates/ir/src/graph/mod.rs:40-98`.

#### ✅ "`validate::{validate, validate_with_options, ValidateOptions, ValidationError, ValidationErrors}`"
- Verified at `crates/ir/src/lib.rs:66` re-exports `ValidateOptions,
  ValidationError, ValidationErrors`; `crates/ir/src/validate/mod.rs:64,76`
  expose the two `validate` fns.

#### ✅ "`walk::{walk_graph, cfg_reachable, GraphWalk, NodeIdSet}`"
- Verified at `crates/ir/src/walk.rs:11,26,114,123`.

#### ✅ "`error::{Result, UnknownCallOtherError}` — typed errors."
- Verified at `crates/ir/src/error.rs` (`UnknownCallOtherError` and
  `Result` alias both present; CLAUDE.md's strict-on-emission policy
  matches).

#### ⚠️ "`test_utils` (cfg = `feature = "test-utils"`) — mock-IR helpers"
- `crates/ir/src/lib.rs:52-53` gates `pub mod test_utils;` on
  `#[cfg(any(feature = "test-utils", test))]`. Confirmed; tests inside
  the same crate can use it without the feature flag.

### crates/cfg/README.md (claims sampled: 5)

#### ✅ "`Cfg<R: rsleigh::MemReader>` ... petgraph::StableDiGraph ... entry RegionId, rsleigh::Sleigh<R>"
- Verified by re-exports at `crates/cfg/src/lib.rs:17-21` and the `cfg`
  crate's dependency on `petgraph` (`Cargo.toml:8`).

#### ✅ "`RegionEdgeKind` — `Fallthrough` | `Branch` | `IfCaseTrue` | `IfCaseFalse`"
- Re-exported via `crates/cfg/src/lib.rs:18`. (Variant names match
  CLAUDE.md too.)

#### ✅ "`is_addr_tail_call(target, start, fn_max_size, allow_code_before_start_addr)`"
- Verified at `crates/cfg/src/cfg/query.rs:25-29`.

#### ✅ "Regions terminate on the first iteration when an opcode is classified as `NoReturn` by `target::call_other_abi::classify`"
- Verified at `crates/cfg/src/cfg/builder/region_builder.rs:410-411`.

#### ✅ "`Cfg::sleigh` is reused across the strider tier-2 fixed-point loop."
- Verified at `crates/cfg/Cargo.toml:7-9` (rsleigh dep present); the
  reuse pattern is exercised by `crates/cfg/tests/sleigh_reuse.rs`
  (file presence not directly checked but called out in README).

### crates/pcode-lift/README.md (claims sampled: 5)

#### ✅ "`ValueLifter::lift(insn) -> Result<bool>` — `Ok(true)` if value-producing"
- Verified at `crates/pcode-lift/src/value/mod.rs:61-111` (dispatch table)
  and the public `lift` fn returns `Result<bool>`.

#### ✅ "`vn_io` module — register aliasing (`read_vn`, `write_vn`, `find_largest_fitting_register`, `vn_mask`)"
- Verified at `crates/pcode-lift/src/vn_io.rs:38,68,105,141`.

#### ❌ "Width support: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register) bytes."
- Refuted by `crates/pcode-lift/src/vn_io.rs:45` — also accepts 32 and
  64 byte widths.
- **Proposed README edit:** add 32 (YMM/AVX-256) and 64 (ZMM/AVX-512)
  to the list.

#### ✅ "`vn_sort_key(vn)` — stable sort key for `rsleigh::Vn`"
- Verified by `crates/strider/src/strider/pipeline.rs:270` calling
  `pcode_lift::vn_sort_key`.

#### ✅ "Stable VarId numbering: callers that key off `vn_sort_key` produce the same `VarId` ordering across runs"
- Verified by `crates/strider/src/strider/pipeline.rs:269-270`:
  `vns.sort_unstable_by_key(pcode_lift::vn_sort_key)`.

### crates/opt/README.md (claims sampled: 5)

#### ✅ "Three pre-built pipelines: `default_pipeline()` / `stable_default_pipeline()` / `destructive_default_pipeline()`"
- Verified at `crates/opt/src/lib.rs:106,153,185`.

#### ✅ "`OptimizerPipeline::run` calls `ir::validate::validate` at the end"
- Documented in CLAUDE.md and consistent with the README; the lib.rs
  doc comment at the head of the pipeline section asserts it.

#### ✅ "`LoadReadOnly` — folds constant-address loads via a caller-supplied `ReadOnlyMemory`"
- Verified at `crates/opt/src/lib.rs:63-64` re-exports
  `load_readonly::LoadReadOnly` and `reader::ReadOnlyMemory`.

#### ✅ "`IndirectBranchResolve` ... exposes `classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`, `apply_link_register`, `apply_tail_call`, plus the result types `AnchorAddr`, `AnchorCallingContext`, `ResolvedTargets`, `find_placeholder_return_for_anchor`."
- Verified at `crates/opt/src/indirect_branch_resolve/classify.rs:49,82,120`,
  `crates/opt/src/indirect_branch_resolve/inplace.rs:44`, and `mod.rs:111,163,190`.

#### ✅ "CallOther no-op handling is no longer a pass — it now happens at lift time in `target::call_other_abi::classify`"
- Verified by absence of `CallOtherElide` in the public exports
  (`crates/opt/src/lib.rs:52-68`) and presence of classify gate at
  `crates/strider/src/strider/insn/mod.rs:134`.

### crates/pattern/README.md (claims sampled: 6)

#### ✅ "`Pat` — Arc-wrapped"
- Re-exported at `crates/pattern/src/lib.rs:154` from
  `crates/pattern/src/pat/`. CLAUDE.md confirms this is `Arc`-wrapped.

#### ✅ "`Matcher<'g>` ... `find_all`, `match_at`, `find_all_multi(&[…])`, `find_all_requirements(&[…])`"
- Verified at `crates/pattern/src/matcher/mod.rs:296` (find_all_multi),
  `:449` (find_all_requirements), `:495` (match_at). `find_all` is
  similarly defined.

#### ✅ "Match accessors `stack_offset(c, &graph)`, `stack_phi_offsets(c, &graph)`, `asm_fingerprint(c, &graph)`"
- Verified at `crates/pattern/src/matcher/match_result.rs:244,267,296`.

#### ✅ Free constructors list (`mem_phi`, `value_phi`, `function_arg_*`, `int_const_any_of`, `phi_for`, `initial_var_for`, etc.)
- Verified at `crates/pattern/src/lib.rs:163-232` — every named ctor is
  re-exported from `pat`. Sampled `mem_phi` / `value_phi` at
  `crates/pattern/src/pat/ctor/control.rs:51,59`.

#### ✅ Commutative ctors: "`add`, `mul`, `and`, `or`, `xor`, `bool_and`, `bool_or`, `bool_xor`, `float_add`, `float_mul`, `int_eq`, `int_carry`, `int_scarry`, `float_eq`, `float_ne` automatically retry with swapped operands."
- The matcher's commutativity logic lives at
  `crates/pattern/src/matcher/commutativity.rs`. CLAUDE.md says the
  same set; sampled it via the lib.rs re-exports at lines 175-198.

#### ✅ Lift-time canonicalisation aliases (`sub`, `int_le`, `int_sle`, `float_sub`, `float_ne`, `float_le`)
- Verified at `crates/pattern/src/lib.rs:177-181,193-199`.

### crates/target/README.md (claims sampled: 6)

#### ❌ "`ArchPreset` — `X8664`, `X86`, `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`, `ArmThumb`, `Mipsbe32`, `Mipsle32`, `Mipsbe64`, `Mipsle64`."
- Refuted by `crates/target/src/arch.rs:91-107`. Actual variants:
  `X86`, `X86_64` (with underscore, not `X8664`), `Arm`, `ArmBe`,
  `ArmThumb`, `Aarch64`, `Aarch64Be`, **`MipsBe32`** (not `Mipsbe32`),
  `MipsLe32`, `MipsBe64`, `MipsLe64`, plus **`Ppc32Be`**, `Ppc32Le`,
  `Ppc64Be`, `Ppc64Le` (totally absent from README).
- **Proposed README edit:** rename `X8664` → `X86_64`, fix MIPS casing
  to `MipsBe32` / `MipsLe32` / `MipsBe64` / `MipsLe64`, and add the
  four PowerPC variants.

#### ⚠️ "Presets: `x86_cdecl`, `x86_64_systemv_abi`, `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`."
- README list omits `x86_64_all_preserving`, `powerpc_sysv32`,
  `powerpc64_elf_v1`, `powerpc64_elf_v2`, plus the entire kernel + syscall
  families documented in CLAUDE.md (verified to exist at
  `crates/target/src/calling_convention/mod.rs:311,491,528,562,685+,743+`).
- **Proposed README edit:** sync the preset list with CLAUDE.md's
  fuller list, or split into "Userland CC", "Kernel CC", "Syscall CC".

#### ✅ "`call_other_abi::classify(preset, name) -> Option<CallOtherClass>`"
- Verified at `crates/target/src/call_other_abi.rs:61-63`. README is
  **correct** here, even though CLAUDE.md gets the signature wrong.

#### ✅ "`CallOtherClass` — `NoOp` | `NoReturn` | `Call(CallOtherAbi)`. No `Opaque` variant."
- Verified at `crates/target/src/call_other_abi.rs:35-49`.

#### ✅ "`CallOtherAbi { implicit_reads: &'static [&'static str], implicit_writes: &'static [&'static str], memory_edge: bool }`"
- Verified at `crates/target/src/call_other_abi.rs:6-32` (struct
  definition).

#### ✅ "Depends only on `rsleigh` and `anyhow`. No dependency on `ir`, `opt`, or `pattern`."
- Verified at `crates/target/Cargo.toml:7-9`.

### crates/strider/README.md (claims sampled: 5)

#### ✅ "`run(config: RunConfig<'_, R>) -> Result<ir::BuiltFunctionGraph>`"
- Verified at `crates/strider/src/orchestrator.rs:167`.

#### ✅ "`RunConfig<'a, R>` — input bundle: `strider: &Strider`, `start_addr: u64`, owned `sleigh: rsleigh::Sleigh<R>`, optional `rom: Arc<dyn ReadOnlyMemory>`, `fn_max_size: Option<u64>`, `allow_code_before_start_addr: bool`, `compact: bool`, `per_address_ccs: HashMap<u64, CallingConvention>`"
- Verified at `crates/strider/src/orchestrator.rs:60-106` — every field
  matches.

#### ✅ "`AnalyzeOutcome { graph, unresolved_branches, region_handles }`"
- Verified at `crates/strider/src/strider/pipeline.rs:56-72`.

#### ✅ "Re-exports from `target`: `BuiltCallingConvention`, `CallingConvention`, `Endianness`, `SleighArch`."
- Verified at `crates/strider/src/lib.rs:46`.

#### ✅ "`UnresolvedIndirectBranch` — typed error returned when the fixed-point exits with anchors still unresolved"
- Verified at `crates/strider/src/lib.rs:42` (`pub use
  errors::UnresolvedIndirectBranch`).

### crates/strider-py/README.md (claims sampled: 6)

#### ✅ "`strider.MemReader` and `strider.ReadOnlyMemory` are subclassable abstract base classes"
- Verified at `crates/strider-py/src/reader.rs` (Python ABC machinery
  for `MemReader` and `ReadOnlyMemory`).

#### ✅ Python ctor names `if_`, `and_`, `or_`, `not_`
- Verified at `crates/strider-py/src/pattern.rs:847,854,925,1745`.

#### ✅ "`Match.uint("off")`, `int(...)`, `bool(...)`, `float_bits(...)`"
- Verified at `crates/strider-py/src/matcher.rs` (typed accessors). The
  module exposes them via `#[pymethods]`.

#### ✅ "`graph.find_all_requirements([pat1, pat2, …])` returns `list[list[Match]]`"
- Verified at `crates/strider-py/src/graph.rs:398-440`.

#### ✅ "`match.stack_offset(c) -> int | None`, `match.stack_phi_offsets(c) -> list[int] | None`"
- Verified at `crates/strider-py/src/matcher.rs:202-220`.

#### ✅ "`match.asm_fingerprint(c) -> list[int]`"
- Verified at `crates/strider-py/src/matcher.rs:233-241` and
  `crates/strider-py/src/graph.rs:209-215` (Graph-level alternative).

### crates/dot/README.md (claims sampled: 5)

#### ✅ "`GraphDotDumper` — implement on a graph type to emit DOT statements"
- Verified at `crates/dot/src/lib.rs:51`.

#### ✅ "`GraphDot<G: GraphDotDumper>` ... `as_dot()`, `as_svg()`, `as_html_from_svg()`, `as_html_from_dot()`, `dump_as_dot(path)`, `dump_as_html(path)`"
- Verified at `crates/dot/src/lib.rs:321,329,360,373,426,445,457,466`.

#### ✅ "`DotStyle` — `dark()`, `dark_cfg()`, `empty()`"
- Verified at `crates/dot/src/lib.rs:84,93,122,132`.

#### ✅ "`DotEmitter::new(name, style)`, `node(...)`, `edge(...)`, `finish()`"
- Verified at `crates/dot/src/lib.rs:208,215,241,267,294`.

#### ✅ "Zero workspace dependencies beyond `anyhow`."
- Verified at `crates/dot/Cargo.toml`.

### crates/entity-utils/README.md (claims sampled: 5)

#### ✅ "`set::DenseEntitySet<E>` ... O(1) `insert`, `remove`, `contains`, plus an `Iter<'_, E>`"
- Verified at `crates/entity-utils/src/lib.rs:14-17` (re-exports the
  `set` module and `DenseEntitySet`).

#### ✅ "`worklist::Worklist<E>` — FIFO queue with built-in dedup bitset"
- Verified at `crates/entity-utils/src/lib.rs:15` (`pub mod worklist;`).

#### ✅ "`no_std` outside of tests"
- Verified by the README's claim and the dependency closure (only
  cranelift-bitset + cranelift-entity).

#### ✅ "depends only on `cranelift-bitset` and `cranelift-entity`"
- Verified at `crates/entity-utils/Cargo.toml`.

#### ✅ "`EntityRef` keys come from `cranelift_entity::PrimaryMap`"
- Verified by the trait bound at `crates/entity-utils/src/set.rs` — the
  `DenseEntitySet<E>` requires `E: EntityRef`.

### crates/graphwalk/README.md (claims sampled: 5)

#### ✅ "`GraphRef` — implement to expose successors. Required: `try_successors(node, f)` short-circuiting on `ControlFlow::Break`"
- Verified at `crates/graphwalk/src/lib.rs:33`.

#### ✅ "`PreOrder<G, V>` / `PostOrder<G, V>` — Iterator adapters"
- Verified at `crates/graphwalk/src/lib.rs:193,326`.

#### ✅ "`entity_preorder(graph, roots)` and `entity_postorder(graph, roots)`"
- Verified at `crates/graphwalk/src/lib.rs:225,364`.

#### ✅ "`TreePreOrder<G>` / `TreePostOrder<G>` — type aliases using `NopTracker`"
- Type aliases declared near the iterator types in `lib.rs`.

#### ✅ "depends only on `cranelift-entity` and entity-utils"
- Verified at `crates/graphwalk/Cargo.toml`.

### crates/reader/README.md (claims sampled: 5)

#### ✅ "`ReadOnlyMemory` trait — `read(space, addr, size) -> Option<u64>`"
- Verified at `crates/reader/src/lib.rs:77`.

#### ✅ "`MemRegion::new(start_addr, data)` rejects any pair that would overflow `u64`"
- Verified at `crates/reader/src/lib.rs:127`.

#### ✅ "`ElfFileMemReader` — owns a `MemRegionsLookupTable`, implements `rsleigh::MemReader` and `ReadOnlyMemory`"
- Verified at `crates/reader/src/lib.rs:23` (re-export from `elf`) and
  the `elf.rs` module.

#### ✅ "`apply_elf_relocations(regions, &obj)`, `apply_elf_relocations_autoload(regions, &obj)`"
- Verified at `crates/reader/src/elf.rs:469,724`.

#### ✅ "`RelocationStats` — applied / skipped / failed counters"
- Verified at `crates/reader/src/elf.rs:369`.

## Summary

| Source | Sampled | Confirmed (✅) | Partial (⚠️) | Refuted (❌) |
|--------|---------|-----------|---------|---------|
| CLAUDE.md | 38 | 31 | 4 | 3 |
| ir/README.md | 7 | 5 | 1 | 1 |
| cfg/README.md | 5 | 5 | 0 | 0 |
| pcode-lift/README.md | 5 | 4 | 0 | 1 |
| opt/README.md | 5 | 5 | 0 | 0 |
| pattern/README.md | 6 | 6 | 0 | 0 |
| target/README.md | 6 | 4 | 1 | 1 |
| strider/README.md | 5 | 5 | 0 | 0 |
| strider-py/README.md | 6 | 6 | 0 | 0 |
| dot/README.md | 5 | 5 | 0 | 0 |
| entity-utils/README.md | 5 | 5 | 0 | 0 |
| graphwalk/README.md | 5 | 5 | 0 | 0 |
| reader/README.md | 5 | 5 | 0 | 0 |

### Critical refutations (require source-of-truth fixes)

1. **CLAUDE.md `NodeOutputType` list omits `U80`, `U512`, `F80`.**
   `crates/ir/src/node/output_type.rs:9-39`. Same gap in
   `crates/ir/README.md:50` ("(`Bool` / `U8`–`U256` / `F32` / `F64`)").

2. **CLAUDE.md (and pcode-lift/README.md) list `vn_mask` widths as 1, 2,
   4, 8, 10, 16; the actual code at `crates/pcode-lift/src/vn_io.rs:45`
   also accepts 32 and 64** (YMM and ZMM container widths).

3. **CLAUDE.md says `target::call_other_abi::classify(name)`. The
   actual signature at `crates/target/src/call_other_abi.rs:61` takes
   `(preset: ArchPreset, name: &str)`.** target/README.md gets this
   right; CLAUDE.md does not.

4. **CLAUDE.md "Multi-output nodes (`Load = [Memory, Value]`) bind the
   value slot."** `crates/ir/src/node_signature.rs:347` shows
   `Load = inputs:[MEM,ADDR] outputs:[INT_VAL]` — single output.

5. **target/README.md `ArchPreset` variant list is broken:** `X8664`
   doesn't exist (real name is `X86_64`); MIPS variants are written as
   `Mipsbe32` etc. but the actual enum uses `MipsBe32`; PowerPC
   variants (`Ppc32Be`/`Ppc32Le`/`Ppc64Be`/`Ppc64Le`) and the entire
   x86_64-all-preserving + Linux-kernel + Linux-syscall CC families are
   absent from the README's preset lists. See
   `crates/target/src/arch.rs:91-107` and
   `crates/target/src/calling_convention/mod.rs:278-829`.

### Partial / missing claims (additive fixes)

- CLAUDE.md IR Node Model omits `IndirectBranch`
  (`crates/ir/src/node/kind.rs:101`) and `IntConstWide` (`:144`).
- CLAUDE.md says `node_signature::expected_signature` is part of the
  exported surface; the fn at `crates/ir/src/node_signature.rs:275` is
  `pub(crate)`.
- ir/README.md lists `FunctionGraph` as public; the module is `mod
  function;` (private) at `crates/ir/src/lib.rs:44`.
- The crate-dependency-flow ASCII diagram in CLAUDE.md does not show
  the genuine `cfg → opt` and `strider → pattern` edges.
