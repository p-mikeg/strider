# Round 13 — 1E: `strider` + `target` + `reader` audit

Branch: `review/ai7` · Scope: `crates/{strider,target,reader}/{src,tests}/**`, plus benches + examples.

## Verdict

**1 LOW + 1 LOW.** Both are observed-correct but documentation/test-coverage gaps; no HIGH-severity correctness regressions.

## Findings

### TR13-1 — `arch_independent_call_entries_have_empty_register_channels` test lists `sysret`/`swapgs` despite their arch-specific placement
- **Severity:** LOW (confidence 85)
- **Where:** `crates/target/src/call_other_abi.rs:796-822`
- **What:** `arch_independent_names` array (line 796) includes `"sysret"` (line 803) and `"swapgs"` (line 821).  Both moved to `classify_arch_specific` in R12 W1.  Test passes for the wrong reasons:
  - `sysret` on X86_64 returns `Some(NoReturn)` → hits `_ => continue` at line 832 (silently skipped).
  - `swapgs` on X86_64 returns `Some(Call(empty_abi))` → coincidentally has empty register channels, so the assertion at line 834 passes by accident.
  Comment at line 823 "by definition these resolve identically on every arch" is factually wrong for both — the actual arch-specific invariant is covered by `sysret_and_swapgs_are_x86_only` (line 494).
- **Fix:** Remove `"sysret"` and `"swapgs"` from the `arch_independent_names` list.  (Round 1B flagged the same site; consolidate fix.)

### TR13-2 — `locate_spliced_call` returns wrong Call when a ControlState has multiple Call predecessors
- **Severity:** LOW (confidence 80) — correctness-neutral per the function's own doc
- **Where:** `crates/strider/src/orchestrator.rs:754-762`
- **What:** `locate_spliced_call` walks backwards from a freshly-spliced Return through an optional ControlState bridge, returning the first `Call` it finds among the ControlState's control inputs (loop at line 756).  If two in-place edits in the same `apply_in_place_edits` iteration each produce a tail-Call Return feeding the same ControlState (both with per-address CC overrides), the second edit's `locate_spliced_call` traversal returns the Call inserted by the first edit.  `set_call_clobbered_override` then attaches the second CC's clobber list to the wrong NodeId.
- **Blast radius:** doc at lines 738-742 explicitly calls this "best-effort / correctness-neutral".  IR stays structurally valid; only a pattern query reading `.call_clobbered_override` on the second edit's Call would see wrong data.
- **Fix:** Have `opt::apply_tail_call` return `(Return NodeId, Call NodeId)` instead of only the Return, and thread the Call NodeId through `apply_in_place_edit` directly.  Eliminates the ambiguous graph walk.

## Focus areas verified clean

| # | Area | Verdict |
|---|---|---|
| 1 | Orchestrator fixed-point convergence + monotonicity | `known_targets` only grows; `stall_budget` resets on Rebuild. ✓ |
| 2 | Indirect-resolve `Decision { FixedPoint, StableOnly, Rebuild }` | `recompute_unresolved` uses `mem::take` and correctly restores list. ✓ |
| 3 | `LoopState::step` stall-guard ordering | `prev_unresolved_len` captured before `apply_in_place_edits`; guard `>` not `>=`. ✓ |
| 4 | `locate_spliced_call` ControlState chain walking | TR13-2 (multi-pred ambiguity). |
| 5 | `GraphRewriter::re_optimize` | Delegates to `OptimizerPipeline::run`; validation runs at pipeline exit. ✓ |
| 6 | `CallingConvention` varnode resolution per arch | All 15 presets built against live Sleigh register tables in `calling_convention/tests.rs`. ✓ |
| 7 | `apply_elf_relocations_autoload` correctness + rollback | Staged regions truncated on Err; pre-existing byte mutations not reverted (documented partial-rollback). ✓ |
| 8 | MIPS / PPC RELATIVE / GLOB_DAT / JUMP_SLOT arms | `R_MIPS_REL32` correctly 4-byte on both MIPS32 and MIPS64.  PPC64 RELATIVE/IRELATIVE 8B. ✓ |
| 9 | `RelocationStats` programmatic surface | All counters present with doc comments. ✓ |
| 10 | Per-arch `test_utils` wrapper coverage | x86_64, x86, aarch64, arm, mips_o32 (LE+BE), ppc32be, ppc64le, ppc64be present.  MIPS N64 absent in test_utils but covered in `calling_convention/tests.rs` data-driven cases. |
| 11 | `Builder::for_arch` migration completeness | `Builder::new` and `Builder::with_endianness` fully deleted (R12 W5c).  Zero remaining call sites. ✓ |
| 12 | `RunConfig::start_addr: cfg::MachineInsnAddr` newtype | Correctly typed; `From<u64>` + `MachineInsnAddr::new(u64)` ctors present. ✓ |
| 13 | CallOther entries (monitor/mwait family; sysret/swapgs arch-specific) | All four MONITOR/MWAIT entries correct per Intel SDM.  `sysret`/`swapgs` correctly arch-specific.  Test stale-list issue at TR13-1. |
| 14 | `OptionsBuilder::set_function_boundary` typed setter | Present in `cfg::OptionsBuilder`. ✓ |
| 15 | `Cfg::{graph,entry,sleigh,into_sleigh}()` accessors | All four present (`cfg/mod.rs:123-144`). ✓ |
| 16 | Production panics in non-test code | `#![allow(clippy::expect_used)]` only in `cfg(test)` scopes + `test_utils`.  No `unwrap`/`expect`/`panic!` in production. ✓ |

## Coverage table

| File group | Status |
|---|---|
| `strider/src/{lib,errors,orchestrator,rewrite,test_utils}.rs` | Fully read |
| `strider/src/strider/{mod,pipeline,vn_io}.rs` | Fully read |
| `strider/src/strider/insn/{mod,control}.rs` | Fully read |
| `strider/src/indirect_resolve/{mod,classify,inplace}.rs` | Fully read |
| `target/src/{lib,arch,call_other_abi}.rs` | Fully read |
| `target/src/calling_convention/{mod,tests}.rs` | Fully read |
| `reader/src/{lib,elf}.rs` | Fully read |
| `strider/tests/`, `target/tests/`, `reader/tests/` | Spot-checked behaviour |
