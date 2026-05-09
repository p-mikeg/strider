# Round 8 — CLAUDE.md correctness diff (proposed)

**Source:** `round8-3A-doc-verify.md` sampled 38 claims from CLAUDE.md and 5-7 from each per-crate README; **75 confirmed, 6 partial, 7 refuted.**  Below are the concrete edits to bring CLAUDE.md back into sync with the code.  No source files are modified by this round; this is a proposal.

## Refuted claims — required edits

### 1. `NodeOutputType` variant list incomplete

**Where:** CLAUDE.md "IR Node Model" section, `NodeOutputType` description, plus mirror in `crates/ir/README.md:43-46`.

**Current text:**
> `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U128`, `U256`; floats `F32`, `F64`.

**Replacement:**
> `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U80` (x87 80-bit extended), `U128`, `U256`, `U512`; floats `F32`, `F64`, `F80`.

**Source of truth:** `crates/ir/src/node/output_type.rs:9-39`.

### 2. `vn_mask` width list incomplete

**Where:** CLAUDE.md "Register Aliasing" section, plus mirror in `crates/pcode-lift/README.md`.

**Current text:**
> `vn_mask` enumerates supported widths: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register) bytes.

**Replacement:**
> `vn_mask` enumerates supported widths: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register), 32 (YMM), 64 (ZMM) bytes.  Widths > 16 return a degraded `u128::MAX` mask; the wide-container guard rejects sub-register aliasing for these.

**Source of truth:** `crates/pcode-lift/src/vn_io.rs:45`.

### 3. `call_other_abi::classify` signature

**Where:** CLAUDE.md "CallOther classification" section, multiple references.

**Current text:**
> `target::call_other_abi::classify(name)` — single-source-of-truth name → `CallOtherClass {NoOp, NoReturn, Call(CallOtherAbi)}` table…

**Replacement:**
> `target::call_other_abi::classify(preset, name)` — single-source-of-truth `(ArchPreset, name) → CallOtherClass {NoOp, NoReturn, Call(CallOtherAbi)}` table…  The two-arg form supports arch-specific entries (e.g. ARM `swi` vs x86 `swi`); arch-independent entries (e.g. `mfence`, `cpuid`) are checked against any preset.

**Source of truth:** `crates/target/src/call_other_abi.rs:61` (correct in `target/README.md`; only CLAUDE.md is stale).

### 4. `Load` output shape

**Where:** CLAUDE.md "pattern" section, `Match.output(c)` description.

**Current text:**
> Multi-output nodes (`Load = [Memory, Value]`) bind the value slot.

**Replacement:**
> `Load` produces a single value output (the loaded scalar); the memory chain is threaded through the consumer's memory input separately, not a `Load` output.  Builders that return `[Memory, Value]` for multi-output nodes are: `Call`, `CallOther` (in modeled form).

**Source of truth:** `crates/ir/src/node_signature.rs:347` — `Load`'s `outputs` slice has length 1.

### 5. IR Node Model omits `IndirectBranch` and `IntConstWide`

**Where:** CLAUDE.md "IR Node Model" section, "Initial state" / "Memory" / "Integer" subsections.

**Add after "Conditional branch" line:**
> - **Indirect branch:** `IndirectBranch` (placeholder consumed by the orchestrator's indirect-resolution loop; rewritten in place by `apply_link_register` / `apply_tail_call` / jump-table classifier).

**Add after `IntConst(u128)` line:**
> - `IntConstWide(WideConstId)` — for U256 / U512 constants whose value cannot fit in `u128`.  Backed by `Graph::wide_consts` interning table; same-value calls dedup to the same `WideConstId`.

**Source of truth:** `crates/ir/src/node/kind.rs:101, 144`.

### 6. `expected_signature` visibility

**Where:** CLAUDE.md "Key Crates → ir" section, `node_signature::{ExpectedOutputKind, expected_signature}` reference.

**Current text:**
> `node_signature::{ExpectedOutputKind, expected_signature}` — single source of truth for expected input/output slot kinds per `NodeKind`.

**Replacement:**
> `node_signature::{ExpectedOutputKind, expected_signature}` — single source of truth for expected input/output slot kinds per `NodeKind`.  Note: `expected_signature` is `pub(crate)`, used internally by `validate`; not part of the public API.

**Source of truth:** `crates/ir/src/node_signature.rs` — function is `pub(crate)`, not `pub`.

### 7. Crate-dependency-flow diagram missing edges

**Where:** CLAUDE.md "Crate Dependency Flow" diagram.

**Current text** shows: `target ← ir ← pcode-lift ← cfg ← strider ← strider-py` and `opt ← pattern ← strider-py`.

**Missing edges to add:**
- `cfg → opt` (cfg depends on opt; verified `crates/cfg/Cargo.toml:12`).
- `strider → pattern` (strider depends on pattern; verified `crates/strider/Cargo.toml:16`).

The diagram should additionally annotate that `strider-py` re-exports surfaces from every crate it depends on (already noted in prose).

## Partial / additive issues

### 8. `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` partial-state form

**Where:** CLAUDE.md "Key Crates → ir" — `BuiltFunctionGraph` description.

**Add a paragraph:**
> `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` constructs a partial-state form with empty `variables`, `call_clobbered`, and `call_other_clobbered` fields.  Used by `pattern::rewrite_rule` and `GraphRewriter` when no per-call clobber semantics are needed.  Direct callers must NOT invoke methods that depend on these fields (see `round8-2D-types.md` for the type-design tension).  Round-7 work introduced `RewriteCtx` to scope this surface; full migration is incomplete.

### 9. PowerPC CC presets — LR is intentionally listed as callee-saved

**Where:** CLAUDE.md "Key Crates → target" — CC preset list.

**Add a footnote after the PowerPC presets:**
> Note: `powerpc_sysv32`, `powerpc64_elf_v1`, and `powerpc64_elf_v2` list `LR` in `callee_saved_regs`.  This deviates from the official PowerPC ABI specs (which mark LR as volatile) and is a deliberate tradeoff that enables `InitialVar(LR)` to flow through call sites for the indirect-branch resolver's link-register classification.  See `round8-17-graph-soundness.md` B-2 for the soundness implication for tail-call shims.

The same note applies to `aarch64_aapcs64` (x30 / LR) and `arm_aapcs` (lr) — see `round8-17-graph-soundness.md` B-1, B-3.

### 10. `IfPat` direct-layout-only via `IfCondInversion`

**Where:** CLAUDE.md "Key Crates → pattern" — `IfPat` description.

**Current text:** "`IfPat` matches **direct layout only** — the [`opt::IfCondInversion`] pass guarantees every `If` in the optimised IR is in canonical direct layout (cond is not a `BoolNeg`) before patterns run."

**Add subscript:**
> ⚠ Round 8 finding `round8-correctness-invariants.md` H-2 reports that `IfCondInversion` does not absorb the `BoolNeg`'s asm-fingerprint into the surviving inner-cond node; running `validate_with_options(check_asm_fingerprints: true)` on a graph that has been through `IfCondInversion` may flag empty fingerprints.  Fix is pending.

### 11. CallOther classification arch-specificity

**Where:** CLAUDE.md "CallOther classification" section.

**Add:**
> The `(preset, name)` lookup at `target::call_other_abi::classify` is **delivered to the CFG region builder via `Builder::for_arch`**, not via `Builder::with_endianness`.  The latter hardcodes `ArchPreset::X86_64`.  See `round8-correctness-cross-arch.md` §1 for a CRITICAL finding that the orchestrator's `strider::run` currently uses `Builder::with_endianness`, breaking arch-specific CallOther dispatch on AArch64/ARM/MIPS/PPC.  A one-line fix at `crates/strider/src/orchestrator.rs:826` resolves it.

### 12. PyO3 — known constraints on `.when()` predicates

**Where:** CLAUDE.md "Key Crates → strider-py" — `pattern.when()` / `Graph.find_all` description.

**Add:**
> Python predicates passed to `.when()` must NOT call mutating graph methods (`reoptimize`, `rewrite`, `compact`, `optimize`) on the same `Graph` they were invoked from.  Doing so deadlocks via re-entrant `RwLock` acquisition (see `round8-correctness-borrowing.md`).  A future change will replace the blocking `RwLock::write()` with `try_write()` returning a typed `StriderError` to fail fast instead of deadlock.

## Per-crate README issues (mirrored to CLAUDE.md where applicable)

### 13. `target/README.md` ArchPreset variant list

**Issue:** Uses `X8664` instead of `X86_64`, `Mipsbe32` instead of `MipsBe32`, omits all four `Ppc*` variants, all Linux-kernel CC presets, all Linux-syscall CC presets, and `x86_64_all_preserving`.

**Fix:** Regenerate the variant list from `crates/target/src/lib.rs::ArchPreset` and the CC preset list from `crates/target/src/calling_convention/mod.rs`'s `pub fn` block.  Cross-link to CLAUDE.md.

### 14. `ir/README.md` claims `FunctionGraph` is public

**Issue:** `mod function;` is private; `FunctionGraph` is not in `crates/ir/src/lib.rs` re-exports.

**Fix:** Either (a) make the module + type `pub` and update CLAUDE.md, or (b) remove `FunctionGraph` from the README's public surface.

### 15. `ir/README.md` describes control flow using `IfCase`

**Issue:** `IfCase` is not a `NodeKind`; it is only a CFG edge label.  The IR uses `If` (variadic outputs: true control, false control) followed by a `ControlState` join, not separate `IfCase` nodes.

**Fix:** Replace "control flow uses `ControlState`/`If`/`IfCase` nodes" with "control flow uses `ControlState`/`If` nodes; the `If`'s true and false outputs are consumed by downstream `ControlState` predecessors."

### 16. `pcode-lift/README.md` says cfg integration is "(planned)"

**Issue:** It shipped as part of round-7 / round-8.

**Fix:** Replace "(planned)" with "delivered: `pcode_lift::ValueLifter` is the shared lifter used by both `crates/strider/src/strider/insn` and `crates/cfg/src/cfg/builder/indirect_resolve` for mini-graph construction."

### 17. `strider-py` Python `PhiPat` docstring

**Issue:** `crates/strider-py/src/pattern.rs:580-583` claims `PhiPat` covers `VarPhi` / `MemPhi` / `ValuePhi`; only matches `VarPhi`.

**Fix:** Replace with "`PhiPat` matches `VarPhi` only.  Use `mem_phi()` / `value_phi()` for the other two phi kinds."

### 18. `strider-py` `match.vn(c)` docstring

**Issue:** `crates/strider-py/src/matcher.rs:183-187` claims VarPhi/FunctionArg support; underlying Rust `Match::get_vn` handles only `InitialVar` and `Call`/`CallOther` clobber slots.

**Fix:** Replace with "`match.vn(c)` returns the varnode bound by an `InitialVar(vn)` capture or a `Call`/`CallOther` clobber-slot capture.  Returns `None` for other capture kinds (including `VarPhi`, `FunctionArg`, `IntConst`)."

## Verified-correct claims (sampled, no edits needed)

- All 15 `SleighArch` presets exist as listed.
- All userland / kernel / syscall CC presets exist as listed.
- All 8 lift-time canonicalisations match the source (`crates/pcode-lift/src/value/{arithmetic,float}.rs`).
- Pipeline composition (`default_pipeline`, `stable_default_pipeline`, `destructive_default_pipeline`) matches.
- All Python parity claims (`and_`/`or_`/`not_`/`if_`, `find_all_requirements`, `stack_offset`, `asm_fingerprint`) match.
- All sampled CallOther ABI table entries match Intel SDM / ARM ARM / SMCCC 1.2 / GHIDRA Sleigh source.
