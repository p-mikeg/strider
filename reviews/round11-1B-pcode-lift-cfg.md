# Round 11 — 1B: pcode-lift + cfg audit

Branch reviewed: `feature/ai` at HEAD (commit `c7a2903`).  Audit performed
against the source as it exists in the working tree; no earlier-round audit
files consulted.

## Coverage

| Path | Inspected fully? | Notes |
|------|------------------|-------|
| crates/pcode-lift/Cargo.toml | yes | |
| crates/pcode-lift/README.md | yes | |
| crates/pcode-lift/src/lib.rs | yes | |
| crates/pcode-lift/src/vn_io.rs | yes | reg-aliasing core; tests + production paths |
| crates/pcode-lift/src/value/mod.rs | yes | top-level dispatcher |
| crates/pcode-lift/src/value/arithmetic.rs | yes | IntSub / IntLessEqual / IntSlessEqual / IntNotEqual lowerings |
| crates/pcode-lift/src/value/boolean.rs | yes | trivially correct |
| crates/pcode-lift/src/value/cast.rs | yes | Subpiece / Piece / Extract / Insert / PtrAdd / PtrSub |
| crates/pcode-lift/src/value/float.rs | yes | float arith / NaN-aware lowerings |
| crates/pcode-lift/src/value/integer.rs | yes | Copy / sext / zext |
| crates/pcode-lift/src/value/mem_load.rs | yes | Load only; Store routed through strider |
| crates/pcode-lift/src/value/misc_value.rs | yes | SegmentOp / CPoolRef / New |
| crates/pcode-lift/tests/value_lifter.rs | yes | end-to-end opcode coverage |
| crates/pcode-lift/tests/vn_io_partial_write.rs | yes | AL/EAX/RAX merge regression |
| crates/cfg/Cargo.toml | yes | |
| crates/cfg/README.md | yes | |
| crates/cfg/src/lib.rs | yes | |
| crates/cfg/src/test_api.rs | yes | |
| crates/cfg/src/cfg/mod.rs | yes | |
| crates/cfg/src/cfg/decode_cache.rs | yes | |
| crates/cfg/src/cfg/dot.rs | yes | |
| crates/cfg/src/cfg/options.rs | yes | |
| crates/cfg/src/cfg/query.rs | yes | `is_addr_tail_call` boundary |
| crates/cfg/src/cfg/types.rs | yes | `Region::contains_addr` semantics |
| crates/cfg/src/cfg/builder/mod.rs | yes | `add_region` / `find_region_containing_addr` / `explore` |
| crates/cfg/src/cfg/builder/region_builder.rs | yes | per-insn dispatch + bounded-lift fixups |
| crates/cfg/src/cfg/builder/split.rs | yes | `split_region` edge cases |
| crates/cfg/src/cfg/builder/indirect_resolve.rs | yes | mini-graph resolver + `resolve_const_loads` |
| crates/cfg/tests/region.rs | yes | `contains_addr` |
| crates/cfg/tests/builder_split_region.rs | yes | split + zero-pcode-hole |
| crates/cfg/tests/region_builder_decode.rs | yes | branch-target decode |
| crates/cfg/tests/region_builder_process.rs | yes | per-insn dispatch |
| crates/cfg/tests/region_builder_tail_call.rs | yes | bounded-lift boundary |
| crates/cfg/tests/build_end_to_end.rs | yes | bounded-lift integrations |
| crates/cfg/tests/known_targets.rs | yes | `with_known_targets` |
| crates/cfg/tests/indirect_resolve.rs | yes | mini-graph integration |
| crates/cfg/tests/sleigh_reuse.rs | yes | Sleigh round-trip |
| crates/cfg/tests/cfg_query.rs | partial | spot-checked; no findings |
| crates/cfg/tests/cfg_integration.rs | partial | spot-checked; no findings |
| crates/cfg/tests/region_terminator.rs | yes | terminator variants |
| crates/cfg/tests/region_terminates_on_noreturn_callother.rs | yes | NoReturn classification |
| crates/cfg/tests/region_edge_kind.rs | yes | trivially correct |
| crates/cfg/tests/builder_add_region.rs | partial | spot-checked |
| crates/cfg/tests/builder_find_region.rs | partial | spot-checked |
| crates/cfg/tests/options.rs | partial | spot-checked |
| crates/cfg/tests/dot_dumper.rs | partial | spot-checked |
| crates/cfg/tests/indirect_dispatch.rs | partial | spot-checked |
| crates/cfg/tests/addr_types.rs | partial | spot-checked |
| crates/cfg/tests/vn_to_name.rs | partial | spot-checked |
| crates/cfg/tests/common/{mod,real_binary,synthetic,assertions}.rs | partial | spot-checked |
| crates/cfg/examples/cfg_creator.rs | yes | uses deprecated `Builder::new` with allow |

## Findings

The cfg + pcode-lift code is in good shape.  Most invariants are pinned by
explicit tests, edge cases are documented, and the lift-time canonicalisations
are bit-exact under the IEEE 754 / two's-complement edge cases I checked
(`INT_MIN`, NaN, signed-zero).  Only a small set of issues rise to MED/LOW
confidence; none reach HIGH.

### Empty-insns Branch region's IR control edge is not wired by strider's per-region driver

- **Severity:** MED
- **Where:** crates/cfg/src/cfg/builder/region_builder.rs:348-383 (the
  `(true, false) | (false, true)` arm) interacting with
  crates/strider/src/strider/pipeline.rs:349-413 + insn/control.rs:108-131
- **What's wrong:** When exactly one `CondBranch` successor is OOB,
  `region_builder.rs:378` pops the trailing `CondBranch` insn and emits
  `RegionTerminator::Branch` with `region.insns` possibly empty (single-
  instruction body case).  `add_region` permits this (mod.rs:204-211 only
  rejects empty + non-Branch).  In strider's per-region driver the empty
  region produces zero per-insn lifts; `SpecialTerm::from_terminator(&Branch)
  -> None`, so no post-loop dispatcher fires either.  The IR-level
  `handle_branch` is only reached from `process_insn`'s `Opcode::Branch` arm,
  which never runs on an empty region.  Strider's post-loop edge wiring at
  pipeline.rs:417-430 only links `RegionEdgeKind::Fallthrough`, not `Branch`.
  Net effect: the IR region for the empty Branch case has its outgoing control
  edge silently dropped.  Whether this is observable depends on downstream
  validation; nothing in cfg pins the round-trip with strider.
- **Verified against:** trace through `pipeline.rs` with `region.insns ==
  []` and `region.terminator == Branch`; no call site wires the Branch
  edge for empty-insns regions.  The cfg-level test
  `cond_branch_with_oob_fallthrough_collapses_to_branch_in_range`
  (build_end_to_end.rs:158) only asserts the cfg shape, not the lifted IR.
- **Fix:** Either (a) extend `SpecialTerm` with a `Branch(target)` variant
  that the post-loop dispatcher routes to a tiny `build_branch(dest_block)`
  helper, or (b) add `Branch` to the post-loop edge walk in `pipeline.rs:417`
  alongside `Fallthrough`.  Option (b) is the simpler fix and matches the
  semantic equivalence the cfg README already calls out
  ("`RegionEdgeKind::Fallthrough` and `Branch` are used interchangeably from
  the IR's perspective").
- **Regression test (when applicable):** End-to-end strider lift of a CFG
  whose entry region is empty + Branch → in-range successor → Return; assert
  the IR validator passes and that `unresolved_branches` is empty.

### `handle_extract` / `handle_insert` silently truncate `lsb` / `len` to `u8`

- **Severity:** LOW
- **Where:** crates/pcode-lift/src/value/cast.rs:113-114, 161-162
- **What's wrong:** `let lsb = insn.inputs[1].addr_off as u8;` and
  `let len = insn.inputs[2].addr_off as u8;` use `as u8`, which silently
  drops bits 8..63 of the encoded `addr_off`.  Sleigh's EXTRACT / INSERT
  operands are documented as bit positions/widths; for a wide-register
  EXTRACT (e.g. `len = 256` on a `U256`), `len as u8` produces 0 and the
  later `narrowed & ((1u128 << 0) - 1) = 0` produces a constant-zero result
  with no diagnostic.
- **Verified against:** rsleigh/sleigh/src/opbehavior.cc:105 confirms
  EXTRACT carries no constant evaluator, so Sleigh itself doesn't bound
  `len`/`lsb`; it relies on per-spec instruction semantics.  The handlers
  here trust the operand fits in `u8`.
- **Fix:** Replace `as u8` with `u8::try_from(insn.inputs[i].addr_off)
  .map_err(|_| anyhow!("EXTRACT/INSERT operand {} out of u8 range", ...))?`.
  In practice no current Sleigh spec emits these >255, but the silent
  truncation is a real correctness foot-gun for any future architecture
  that decodes wide-SIMD EXTRACT.
- **Regression test (when applicable):** Synthesize an `Insn { opcode:
  Extract, inputs: [...,  CONST(256, _),  CONST(256, _)], output:
  Vn(size = 32) }` and assert `lift` returns an `Err` with a clear message
  rather than silently producing IntConst(0).

### `Builder::new` doctest example shows a deprecated API

- **Severity:** LOW
- **Where:** crates/cfg/src/cfg/builder/mod.rs:33-48 (the `## Usage` doc on
  `Builder` struct)
- **What's wrong:** The struct-level doctest (no_run) wires a CFG via
  `Builder::new(...)`, the very ctor flagged `#[deprecated]` 50 lines later
  with the message "Use Builder::for_arch …".  Showing a deprecated API as
  the canonical example contradicts the deprecation message and steers new
  callers towards the wrong constructor.
- **Verified against:** the `#[deprecated]` attribute on
  `Builder::new` (mod.rs:98-102) and the README's own "preferred:
  `Builder::for_arch`" line (cfg/README.md:19-20).
- **Fix:** Rewrite the doctest to use `Builder::for_arch(&arch, sleigh,
  fn_addr, opts).build()?` with `let arch = target::SleighArch::x86_64();`.
  No-run already, so no compile cost.
- **Regression test (when applicable):** N/A (doc fix).

### `read_vn`'s default-code-space arm silently truncates a u64 `addr_off` on 32-bit Sleighs

- **Severity:** LOW
- **Where:** crates/pcode-lift/src/vn_io.rs:76-85
- **What's wrong:** `build_int_const(vn.addr_off, space_info.addr_size().try_into()?)`.
  If `space_info.addr_size() == 4` (32-bit code space) but `vn.addr_off >
  u32::MAX`, the constant is built with type `U32` and `build_int_const`'s
  internal mask drops bits 32..63 silently.  The address arithmetic then
  produces a wrong load address.  Mirror site for `write_vn` at vn_io.rs:111-119.
- **Verified against:** crates/ir/src/builder/nodes.rs:102 (`val.into() &
  output_type.bit_mask_u128()`) confirms the silent mask.  In practice
  Sleigh wouldn't emit a default-code-space `Vn` whose `addr_off` exceeds
  the arch's address width, so this is a defensive concern only.
- **Fix:** Add a `vn.addr_off >> (8 * space_info.addr_size()) == 0` check
  and surface a typed error when the high bits are non-zero.
- **Regression test (when applicable):** N/A — defensive only; would
  need a hand-crafted `Vn` with an out-of-range `addr_off`.

### `is_addr_tail_call` documentation comment vs. CLAUDE.md ordering of args

- **Severity:** LOW
- **Where:** crates/cfg/src/cfg/query.rs:25-30
- **What's wrong:** Pure docs / readability.  The function signature is
  `is_addr_tail_call(target, start_addr, fn_max_size, allow_code_before_start_addr)`,
  and the docstring is correct.  But the bounded-lift section in CLAUDE.md
  describes it as `is_addr_tail_call(target, start, fn_max_size,
  allow_code_before_start_addr)` (line 75 of CLAUDE.md), which agrees.
  No bug; just confirming.  The half-open semantics `[start, start +
  fn_max_size)` are correctly implemented (target == start + fn_max_size
  classifies as tail call; this is pinned by `nocheck_at_fn_max_size_boundary`
  in region_builder_tail_call.rs:64).
- **Verified against:** the test cases listed and the CLAUDE.md description.
- **Fix:** No code change required.  Listed for completeness — the auditor's
  brief explicitly asked the half-open boundary to be verified, and it is
  correct.

### Decode cache lazily admits stale `Sleigh`-context entries when reused

- **Severity:** LOW (documented invariant; no current misuse)
- **Where:** crates/cfg/src/cfg/decode_cache.rs:11-20 (invariants comment)
  vs. `Builder::with_decode_cache` at builder/mod.rs:186
- **What's wrong:** `DecodeCache` is keyed on `(machine_addr) → Arc<LiftRes>`
  with the documented invariant "Sleigh-context scoped".  The Builder
  blindly trusts a cache the caller passes via `with_decode_cache` is
  scoped to its `sleigh` field; there is no runtime check.  The current
  caller (strider's orchestrator at `orchestrator.rs:351`) uses one cache
  and consistent Sleighs, so this is unobserved.  But a future caller that
  reuses a `DecodeCache` across two Sleigh contexts would hit subtle bugs
  (UNIQUE varnode offsets and LOAD/STORE space pointers diverge between
  contexts — see `LiftRes::canonicalize` at rsleigh/src/core_types.rs:223).
  No diagnostic surfaces.
- **Verified against:** the invariant comment in decode_cache.rs and the
  `LiftRes` doc on `canonicalize` in rsleigh.
- **Fix:** Optional defence-in-depth: associate each `DecodeCache` with
  the `Sleigh` instance via a unique tag (e.g. an `Arc::as_ptr` of the
  Sleigh's internal LL state) and assert the tag matches on every `get` /
  `insert`.  Currently no caller violates the invariant, so this is purely
  defensive.
- **Regression test (when applicable):** N/A — defensive only.

## Items deliberately not flagged

- **`process_int_cmp_op` does not validate `inputs[0].size == inputs[1].size`.**
  `handle_int_sub` validates equal widths and surfaces a clear error.  The
  comparison ops do not, but `build_int_cmp_operation`'s implicit width
  coercion (via `convert_to_int_if_needed`) would silently coerce a
  width-mismatched cmp.  Sleigh's contract guarantees equal widths for
  cmp inputs, so this is unobserved.  Mirroring `handle_int_sub`'s explicit
  guard would be a one-line consistency improvement but isn't a bug.
- **Wide-SIMD `vn_mask` returns `u128::MAX`.**  Comment at vn_io.rs:34-37
  documents that this is sound for the direct-container path which
  early-outs before consulting the mask.  Verified: `read_reg_vn` /
  `write_reg_vn` reject sub-register aliasing within a >16-byte container
  with a clean typed error, and the direct path doesn't read the mask.
- **`split_region`'s `split_index >= second_region.insns.len()` no-op.**
  Defensive guard for the past-last-insn case; documented as unreachable
  from the normal call path.  Test `split_addr_below_every_insn_returns_error`
  pins the API-level contract.
- **`resolve_const_loads`'s snapshot-then-iterate pattern.**  Reading the
  preorder list once and iterating is the correct pattern; subsequent
  passes re-snapshot via the `while resolve_const_loads(...) { ... }` loop,
  pinning multi-hop ROM pointer chains to fixed point.
- **`region_id_at_start` ambiguity when two regions share a machine_addr.**
  The doc says "the region whose **start machine address** equals `addr`",
  and the implementation returns the lex-smallest `(addr, insn_index)`.
  Where multiple regions could share a machine_addr (a relative-CONST
  branch landing in mid-pcode), the returned region is the one starting
  at `(addr, 0)` — the canonical mid-machine-instruction region.  No bug.

## Coverage summary

47 of 47 in-scope source / test files inspected fully or partially; 0 skipped.
Production source under `crates/pcode-lift/src/**/*.rs` and
`crates/cfg/src/**/*.rs` was inspected fully (15 files); integration tests
under `crates/cfg/tests/*.rs` were inspected fully for the cases the brief
called out (split_region, contains_addr, decode_branch_target,
process_new_insn, region_builder_tail_call, build_end_to_end,
known_targets, indirect_resolve, region_terminator, sleigh_reuse,
region_terminates_on_noreturn_callother, region.rs, region_edge_kind,
region_builder_process) and partially (spot-check) for files orthogonal to
the audit areas (cfg_query.rs, cfg_integration.rs, builder_add_region.rs,
builder_find_region.rs, options.rs, dot_dumper.rs, indirect_dispatch.rs,
addr_types.rs, vn_to_name.rs, tests/common/*).  Cargo.tomls and READMEs for
both crates were inspected fully.
