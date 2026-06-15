# strider-cfg deep audit — 2026-06-14

Read-only audit of `crates/strider-cfg/src` (Builder, region_builder,
split, types, query, indirect_resolver, options, dot). Verified against
actual code + the strider-lift consumer (`lift/dispatch.rs`,
`lift/mod.rs`, `lift/control.rs`) and the orchestrator resolution loop,
not against comments/CLAUDE.md.

Overall the crate is in good shape: edge classification, the
split-retains-id invariant, the `region_if` containment polarity, the
empty-region rules, and the cfg/IR division of labour (handle_branch
no-op + `link_region_edges` wiring Unconditional) are all internally
consistent and the test suite is dense. Findings below are mostly
LOW-severity hardening + a couple of MED edge-case gaps.

---

## CFG-01 — `build()` infinite loop if `lift_one` returns `machine_insn_len == 0`

- **Dimension:** Soundness (code vs itself) / robustness vs external contract
- **Severity:** LOW
- **Confidence:** HIGH
- **Location:** `builder/region_builder.rs:591-616` (`build`),
  `:29-48` (`next_pcode_addr`)

**What & why.** The main decode loop advances with
`cur_addr = next_pcode_addr(cur_addr, &lift_res)`. When the current
machine instruction lifts to a non-empty pcode list with **no**
terminating opcode, `next_pcode_addr` falls through to the
"next machine instruction" branch and adds `lift_res.machine_insn_len`
to the machine address (line 42). `rsleigh::LiftRes` (verified at
`../rsleigh/src/core_types.rs:192-198`) imposes **no invariant that
`machine_insn_len > 0`**. If Sleigh ever returns `machine_insn_len == 0`
for a non-terminating instruction, `cur_addr` does not change, the loop
re-lifts the same address forever, and `detect_fallthrough_oob_tail_call`
never fires (it gates on `cur_addr.machine_addr != start` advancing).
This is a hang, not a panic, so it would manifest as an unkillable CFG
build rather than a diagnosable error.

**Proposed fix.** In `next_pcode_addr`, when taking the
next-machine-instruction branch, treat `machine_insn_len == 0` as a hard
error (`bail!("sleigh returned zero-length machine instruction at …")`)
the same way the overflow case already bails. One-line guard; cheap
insurance against an opaque hang.

---

## CFG-02 — `fn_max_size` upper-bound overflow can mis-classify a wrapped tail call as in-range

- **Dimension:** Soundness vs assembly semantics (bounded-lift edge case)
- **Severity:** LOW
- **Confidence:** MED
- **Location:** `query.rs:40-45` (`is_addr_tail_call`)

**What & why.** The upper bound is `start_addr.saturating_add(sz)`. When
`start + sz` overflows u64 the bound saturates to `u64::MAX`, so the
`target >= upper` test only fires for `target == u64::MAX`. A function
placed at the very top of the address space (`start` near `u64::MAX`)
with a `fn_max_size` that overflows therefore classifies *every* target
≥ start (and, with the lower bound, everything except a tiny window) as
in-range, including addresses that are genuinely a different function
just past the (wrapped) end. The existing test
`fn_max_size_plus_start_addr_overflow_treats_inside_range_as_non_tail_call`
pins this as intended behaviour, but it is a real soundness corner: the
half-open `[start, start+sz)` window is not representable when it wraps.

**Proposed fix.** Either (a) document that `start + fn_max_size` must not
overflow as a caller precondition, or (b) detect the overflow
(`checked_add`) and, on overflow, fall back to "no upper bound" *only*
when `start` itself is in the top page — otherwise this is essentially a
won't-fix given how exotic a top-of-address-space function is. Lowest
cost: leave as-is but add a one-line doc note that the window is
non-wrapping. Flagging for completeness, not urgency.

---

## CFG-03 — `dot.rs` mislabels the false arm of a degenerate same-target CondBranch

- **Dimension:** Cosmetic / debug rendering (not a build-soundness bug)
- **Severity:** LOW
- **Confidence:** HIGH
- **Location:** `dot.rs:93-114`

**What & why.** Unlike `Cfg::region_if` (which guards on
`if_true_region.is_none()` so the *second* parallel edge falls to the
false side), the dot renderer labels each incoming CondBranch edge purely
by `node.contains_addr(true_target)`. For the degenerate
`if (c) goto L else goto L` case (two parallel edges to one region, e.g.
`je +0`), **both** edges render `"if-true"` — the false arm is never
shown. `region_if` reports the same region for both arms correctly, so
only the visual dump is affected. Stale-comment check: the doc at line
78-80 claims the taken side is "the edge whose target starts at
true_target", but the code actually uses `contains_addr` (interior
match) — the comment is slightly behind the code (harmless here).

**Proposed fix.** Track a "first match already labelled if-true" flag per
node the way `region_if` does, or simply note in the doc that the
degenerate both-arms case renders both edges as `if-true`. Debug-only,
very low priority.

---

## CFG-04 — `Switch` reachability silently depends on `region_id_at_start` finding every target; no diagnostic if a target was never decoded

- **Dimension:** Soundness (code vs itself) — cfg/IR contract
- **Severity:** MED
- **Confidence:** MED
- **Location:** `builder/region_builder.rs:445-477` (Switch construction),
  consumed at `strider-lift/src/lift/control.rs` `handle_switch`
  (via `Cfg::region_id_at_start`, `query.rs:160-179`)

**What & why.** The Switch arm pushes every target via
`work_queue.push((Some(region), at_machine_start(target)))` and then
relies on the IR lifter's `handle_switch` to relocate each target with
`region_id_at_start(machine_addr)`. `region_id_at_start` matches **only**
a region whose `start_addr.machine_addr == addr` exactly. This holds in
the normal path (a target landing mid-region triggers `split_region`,
whose second half starts at the target). However, the two layers encode
the same target twice with **different address granularity**: the cfg
builder validates each Switch target only with `is_branch_tail_call_nocheck`
(an *address-bounds* check), never that the target machine address is a
valid *instruction boundary*. If a `known_targets` `Multiple` entry
(produced by the IR jump-table classifier) contains an address that is
in-range but is **not** an instruction start — e.g. it lands inside a
multi-byte instruction with no zero-pcode hole to round into — then
`explore`→`split_region` either splits at the wrong boundary or the
target is never registered at that exact machine address, and
`handle_switch`'s `region_id_at_start` lookup silently fails downstream.
The cfg layer offers no boundary validation and no error here; the
failure surfaces (if at all) only inside the lifter as a region-lookup
miss.

This is bounded in practice because the jump-table classifier feeds back
addresses it derived from rodata that *are* instruction starts, but the
cfg builder is the layer that owns "is this a decodable boundary" and it
currently trusts the feedback unconditionally.

**Proposed fix.** After the Switch successors are enqueued and the whole
CFG is built, the cfg layer cannot easily re-validate (boundaries are
only known post-decode). The cheap, honest fix is at the consumer seam:
have `handle_switch` return a clear `bail!` naming the unresolved target
machine address when `region_id_at_start` misses (verify whether it
already does — if it silently drops the ladder arm, that is the actual
bug). Document on `RegionTerminator::Switch` that every `targets[i]` is a
caller-guaranteed instruction-start address.

---

## CFG-05 — `find_region_containing_addr` / `region_id_at_start` duplicate two near-identical BTreeMap range queries

- **Dimension:** Simplification (no behaviour change)
- **Severity:** LOW
- **Confidence:** HIGH
- **Location:** `builder/mod.rs:184-197`, `query.rs:160-179`

**What & why.** Both walk `start_addr_to_region_id` with a range query
keyed on `PcodeInsnAddr`. `find_region_containing_addr` does
`range(..=addr).next_back()` + `contains_addr`; `region_id_at_start` does
an `(Included(lower), Included(upper))` range to pin a single machine
address. They are not identical (one is containment, one is
start-equality) so they cannot fully merge, but the
`PcodeInsnAddr { machine_addr, insn_index: u64::MAX }` upper-bound idiom
in `region_id_at_start` is hand-rolled and could be a small named helper
shared with any future "all regions at machine address M" query. Pure
tidiness; flagging only because both are O(log R) hot-path lookups and a
shared helper would prevent the two from drifting.

**Proposed fix.** Optional: extract a private
`fn region_range_at_machine(addr) -> Range` helper on the map. Skip if
not worth the churn.

---

## CFG-06 — `add_region` allows empty `Unconditional`/`TailCall` but `split_region` can also produce an in-place empty non-`Unconditional` second half without going through the guard

- **Dimension:** Soundness (code vs itself) — invariant enforcement gap
- **Severity:** LOW
- **Confidence:** MED
- **Location:** `builder/split.rs:78-91`, `builder/mod.rs:126-145`

**What & why.** `add_region` is the SSoT that rejects empty regions with
a disallowed terminator. `split_region`, however, mutates the second-half
region **in place** (`second_region.insns = upper`, line 95-98) and only
the *first* half goes through `add_region`. The two early-return guards
(`split_index == 0` and `split_index >= len`) are documented as defensive
against the "second region would be empty + retain a non-Unconditional
terminator" case, and they do cover the reachable paths. But the
invariant ("an empty region may only carry Unconditional/TailCall") is
enforced for `add_region`-created regions and merely *assumed* for the
in-place split path — there is no assertion. A future change to the split
index arithmetic that produced `0 < split_index < len` but left the
second half empty would silently bypass the guard. The current code is
correct; the invariant is just not defended at the in-place mutation
site.

**Proposed fix.** Add a debug-assert after the in-place mutation that
`second_region` is non-empty (it always is when `0 < split_index < len`),
or a comment cross-linking the two guards to the `add_region` invariant.
Defensive only.

---

## Edge-case tests worth adding (named, not written)

1. **`build_bails_on_zero_length_machine_insn`** — a `MemReader` /
   stub where `lift_one` returns `machine_insn_len == 0` with a
   non-terminating pcode body; assert `build()` returns an error rather
   than hanging (covers CFG-01). Would require a fake Sleigh/lift seam.

2. **`switch_target_not_at_instruction_boundary_is_diagnosed`** — a
   `known_targets` `Multiple` whose target machine address is in-range
   but mid-instruction; assert the build (or downstream lift) surfaces a
   clear error instead of a dropped ladder arm (covers CFG-04).

3. **`cond_branch_true_oob_false_oob_distinct_addresses`** — a CondBranch
   where the taken arm and the fall-through are *both* OOB but at
   *different* addresses; assert two distinct stubs are created and
   `region_if` resolves polarity correctly. The existing test only covers
   the both-arms-*same*-OOB-address degenerate case.

4. **`switch_target_lands_in_zero_pcode_hole_rounds_and_resolves`** — a
   Switch target that falls into an AArch64 PAC-style zero-pcode hole of
   an already-decoded region; assert `split_region` rounds down,
   `start_addr_to_region_id` is keyed at the exact target, and
   `region_id_at_start(target)` resolves the second half. Exercises the
   cfg↔lift Switch correlation through the hole path.

5. **`detect_fallthrough_oob_across_zero_pcode_nop_prefix`** — a run of
   true-NOP (zero-pcode) machine instructions that walks `cur_addr` past
   `start + fn_max_size` while `self.insns` stays empty; assert the
   function-boundary error fires (the code claims to handle this via the
   `advanced_past_start` gate but there is no integration test for the
   zero-pcode-prefix overflow specifically).
