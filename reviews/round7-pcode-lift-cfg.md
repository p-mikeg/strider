# Round 7 — pcode-lift + cfg Audit

Independent review (code-only, no prior reviews trusted) of `crates/pcode-lift` and `crates/cfg`, cross-checked against `../rsleigh/src/**`.

---

## Section A — pcode-lift / vn_io Register Aliasing

### A1. `vn_mask` supported widths — MED
- **Where:** `crates/pcode-lift/src/vn_io.rs:31-41`
- Accepted sizes: 1, 2, 4, 8, 10, 16 bytes. Anything else → `Err("unsupported register size N bytes")`.
- AVX-2 YMM (32 bytes) and AVX-512 ZMM (64 bytes) are rejected. If a Sleigh x86 spec emits a 32-byte varnode (e.g. `YMM0`), the entire lift fails with an error. Error path is correct (no panic) but the limitation is undocumented at the public API surface.
- **Fix:** Either widen `vn_mask` to support 32/64-byte widths, or document the AVX-512 limitation in `pcode-lift::lib.rs` and surface it as a typed error in the public API.

### A2. AH/AL split — write semantics — VERIFIED CORRECT
- **Where:** `crates/pcode-lift/src/vn_io.rs:252-324`
- Writing `AH` (offset=1, size=1) places the value at bits 8..15 via `shift = 8 * (1-0) = 8`. Mask `0xFF00`. Preserve mask `~0xFF00`. AL and AH do not overlap. Verified by formula and by tests.

### A3. Sub-register read of partial parent — VERIFIED CORRECT
- A read of `EAX` after only `AL` was written produces a `Truncate(InitialVar(RAX), 32)`-shaped subgraph. The IR builder tracks the full-width parent variable, and the read correctly carries lineage. No bug.

### A4. Concatenated sub-registers (e.g., `EAX:EDX`) — VERIFIED CORRECT
- Sleigh emits multi-register concatenations as explicit `Piece` opcodes, not as single varnodes. `read_vn` handles single varnodes only — sound.

### A5. Endianness shift direction — VERIFIED CORRECT
- **Where:** `crates/pcode-lift/src/vn_io.rs:183-196`
- LE: `shift = 8 * (reg.addr_off - container.addr_off)`. BE: `shift = 8 * (container.size - reg.size - (reg.addr_off - container.addr_off))`. Test suite covers both.

### A6. Production panics in pcode-lift — VERIFIED ABSENT
- No `unwrap()` / `expect()` / `panic!` in production code paths. All occurrences are inside `#[cfg(test)]`. Fallible paths return `anyhow::Result`.

---

## Section B — ValueLifter Routing

### B1. Opcode dispatch exhaustiveness — VERIFIED CORRECT
- **Where:** `crates/pcode-lift/src/value/mod.rs:32-131`
- 74 `rsleigh::Opcode` variants. All value-producing opcodes covered. Control/Store/Call opcodes correctly fall through `_ => Ok(false)` so the caller routes them. `Indirect` (61) and `MultiEqual` falls to caller's `bail!` since rsleigh's `lift_one` does not emit them.

### B2. Lift-time canonicalization correctness — VERIFIED CORRECT (HIGH-IMPORTANCE)
All eight lowerings verified to match what `pattern::*` aliases expect:
- `IntSub(a,b) → Add(a, Neg(b))` — `arithmetic.rs:151-163` ✓
- `IntLessEqual(a,b) → BoolNeg(IntLess(b,a))` — `arithmetic.rs:103-117` ✓
- `IntSlessEqual(a,b) → BoolNeg(IntSless(b,a))` — `arithmetic.rs:119-138` ✓
- `IntNotEqual(a,b) → BoolNeg(IntEqual(a,b))` — `arithmetic.rs:81-95` ✓
- `FloatSub(a,b) → FloatAdd(a, FloatNeg(b))` — `float.rs:99-107` ✓
- `FloatNotEqual(a,b) → BoolNeg(FloatEqual(a,b))` — `float.rs:115-122` ✓
- `FloatLessEqual(a,b) → Or(FloatLess(a,b), FloatEqual(a,b))` — `float.rs:132-140` ✓ (NaN-safe)
- `FloatNan(x) → BoolNeg(FloatEqual(x,x))` — `float.rs:78-90` ✓

No mismatches.

### B3. Memory-edge correctness — VERIFIED CORRECT
- `Load` threads memory via `builder.build_load`. `Store` is in strider's handler. `CallOther` memory-edge handled per-op via `CallOtherAbi::memory_edge`.

### B4. Asm-fingerprint propagation — VERIFIED CORRECT
- **Where:** `crates/strider/src/strider/insn/mod.rs:35-39`
- Wrapper brackets `set_lift_addr(Some(addr))` … `set_lift_addr(None)` around `process_insn_inner`. All builder calls inside inherit attribution.
- The cfg-time mini-graph (`cfg::builder::indirect_resolve.rs`) intentionally does NOT call `set_lift_addr`. Mini-graph nodes carry empty fingerprints; `builder.build()` uses the non-`check_asm_fingerprints` validate variant. Acceptable because the mini-graph is discarded.

---

## Section C — cfg Correctness

### C1. `is_addr_tail_call` bounds — VERIFIED CORRECT
- **Where:** `crates/cfg/src/cfg/query.rs:25-45`
- Bounded mode: `target < start_addr` → tail. `start_addr + fn_max_size <= target` → tail. Half-open `[start, start + fn_max_size)`. Correct off-by-one.
- Unbounded mode: `lower_bound_strict = !allow_code_before_start_addr`. With `allow_code_before_start_addr = true`, backward branches are allowed.
- CLAUDE.md claim "when `fn_max_size.is_some()`, `allow_code_before_start_addr` is ignored" verified at line 31: `lower_bound_strict = fn_max_size.is_some() || !allow_code_before_start_addr`.

### C2. RegionBuilder bound check after `next_pcode_addr` — VERIFIED CORRECT
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:627-652`
- Loop terminates as `RegionTerminator::TailCall { target: cur_addr.machine_addr.addr }` when OOB. `insns.is_empty()` guard prevents empty-region panic.

### C3. CondBranch sole-instruction OOB silent edge loss — **HIGH / CRITICAL**
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:356-385`
- When `(true, false) | (false, true)` (exactly one successor OOB) AND `self.insns.len() == 1`:
  ```rust
  self.finish_current_region(RegionTerminator::TailCall {
      target: in_range.machine_addr.addr,
  })?;
  ```
- `in_range` is the **in-range** successor address — the code emits a `TailCall` to an address inside the function, AND **does not push the in-range edge to `work_queue`**. The in-range successor region is never explored. Result: silently incomplete CFG.
- Existing comment: "This degenerate case loses the in-range edge entirely, but it is essentially unobserved in real binaries." This is a self-fulfilling claim — the case occurs in compiler-generated trampolines / hot-patching thunks / tail-call stubs.
- **Fix:** When `self.insns.len() == 1`, emit the sole instruction as a one-instruction region with `RegionTerminator::Branch` to `in_range`, and push `in_range` to `work_queue`. Alternatively, raise an explicit error so the caller can decide.

### C4. CallOther unknown-name handoff — VERIFIED CORRECT
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:395-423`
- `classify` returns `None` for unknown user-ops; cfg returns `Ok(ProcessInsnRes::DidntFinishProcessing)` and defers strict error to strider's IR layer (`UnknownCallOtherError`). Documented design. No bug.

### C5. Indirect dispatch handling — VERIFIED CORRECT
- `BranchIndirect` → `process_branch_indirect` → tier-1 mini-graph resolver or cached `known_targets`. Unresolvable → `UnresolvedIndirectBranch` terminator with `target_vn` and pcode `addr` for tier-2.

### C6. DecodeCache correctness — VERIFIED CORRECT
- **Where:** `crates/cfg/src/cfg/decode_cache.rs`
- `Arc<Mutex<FxHashMap<u64, Arc<LiftRes>>>>`. `get`/`insert` recover from poisoning via `unwrap_or_else(|e| e.into_inner())`. `LiftRes` is not mutated post-insert. No race, no stale-entry hazard. TODO(Task17) is removal-pending; no current correctness issue.

### C7. Production panics in cfg — VERIFIED ABSENT (in cfg itself)
- No `unwrap()` / `panic!()` in cfg production paths. (rsleigh has its own panics on OOM and unknown-exception kinds — not cfg's responsibility.)

### C8. rsleigh integration (Opcode coverage) — VERIFIED CORRECT
- 74 `rsleigh::Opcode` variants. Strider's match dispatcher (`strider/insn/mod.rs`) handles Nop, Branch, CondBranch, Store, Return, BranchIndirect, Call, CallIndirect, CallOther, MultiEqual + value path; `Indirect` falls through to `bail!` (rsleigh::lift_one doesn't emit it).

---

## Section D — Cross-Crate Naming & Dead Code

### D1. "tier 1 / tier 2" language in cfg — LOW (comment only)
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:435`
- Internal jargon in comments. No public types/functions named `tier_*` in cfg.

### D2. `Builder::new` defaults are footguns — MED
- **Where:** `crates/cfg/src/cfg/builder/mod.rs:94-118`
- `Builder::new` hard-codes `Endianness::Little` + `ArchPreset::X86_64`. A direct `cfg::Builder::*` user (not via `strider::run`) who forgets `with_preset` + `with_endianness` will silently misclassify CallOthers and decode bytes with the wrong endianness on non-x86_64 / big-endian targets.
- **Fix:** Replace `Builder::new()` with `Builder::for_arch(SleighArch)` that sets endianness + preset atomically. Mark `Builder::new` as `#[deprecated]` or remove.

### D3. `region_id_at_start` linear scan — LOW (perf)
- **Where:** `crates/cfg/src/cfg/query.rs:140-150`
- O(n) scan over all `NodeIndex` per call. `Builder` has a `start_addr_to_region_id` BTreeMap that's not promoted to `Cfg`.
- **Fix:** Promote the BTreeMap to `Cfg` for O(log n) lookups.

### D4. `test_api` modules `pub` on cfg — INFO
- `#[doc(hidden)] pub mod test_api` is intentional for integration tests. Acceptable.

---

## Section E — Python Parity (strider-py)

- `cfg::DecodeCache` — not exposed; created internally by `strider.run()`. Reasonable.
- `cfg::OptionsBuilder::set_read_only_memory` — verify Python `OptionsBuilder` exposes this; if not, Python callers cannot enable tier-1 jump-table resolution from `.rodata`. Confidence on parity gap: medium.
- `cfg::Builder::with_preset` — Python callers route through `strider.run(arch, cc, …)` which threads preset automatically.
- Direct `cfg::Builder` access from Python — not currently a use case.

---

## Critical Issues Summary (confidence ≥ 80)

**CRITICAL (1):**
1. **CondBranch sole-instruction OOB silent edge loss** — `region_builder.rs:374-385`. In-range edge dropped from CFG; emits `TailCall` to in-range address. Conf: 95.

**MED (3):**
2. **`vn_mask` rejects 32-byte (YMM) and 64-byte (ZMM) widths** — `vn_io.rs:31-41`. Hard-blocks AVX-2/512 lifting.  Conf: 85.
3. **`Builder::new` defaults to `X86_64` + `Little`** — `builder/mod.rs:94-118`. Silent misuse on direct cfg consumers. Conf: 88.
4. **`region_id_at_start` O(n) scan** — `query.rs:140-150`. Perf only. Conf: 95.

**Verified-correct items worth recording:**
- All eight lift-time canonicalization lowerings match pattern aliases.
- AH/AL split, endianness shift, sub-register read semantics.
- `is_addr_tail_call` bounded/unbounded logic.
- DecodeCache thread-safety.
- Asm-fingerprint funnel via `set_lift_addr` wrapper.
- No production panics in pcode-lift or cfg.
