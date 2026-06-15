# strider-ir deep audit — 2026-06-14

Read-only audit of `crates/strider-ir/src` (graph alias, dedup cache policy,
`function/data.rs`, `EditFunction`, `validate/`, `viewer`, `walk`, register
aliasing in `builder/vn_io.rs`, const canonicalization, wide-const interning).
Cache *mechanism* lives in `strider-graph`; it was read to verify the policy
contracts.

Verification stance: every claim below was traced against the real code, not
the doc-comments. The masking/aliasing soundness comments were checked and
found accurate (no findings there) — they are noted as such.

## Counts

- HIGH: 1
- MED: 4
- LOW: 4

---

## IR-1 — `dedup_overlapping_largest` is O(n²) over the tracked-varnode set
- Dimension: RUNTIME
- Severity: MED
- Confidence: High
- Location: `crates/strider-ir/src/builder/mod.rs:63-81`
- What & why: For each varnode in `all_used_variables` the filter does
  `all_used_variables.iter().any(...)` — a nested linear scan, i.e. O(n²) in
  the tracked-varnode count. This runs once per function in
  `FunctionBuilder::new`. The very next step (`build_largest_container_map`,
  line 110) deliberately uses an O(V log V) bucket+stack-sweep for the *same*
  containment relation, with a doc-comment explaining the algorithm — yet the
  dedup pass that feeds it stays quadratic. The set is "every REGISTER/UNIQUE
  varnode the CFG touches"; on large functions / wide-ISA lifts (x86 with many
  uniques, AArch64 SIMD) this is realistically hundreds of entries, and the
  workspace's stated invariant is "never O(n²)".
- Proposed fix: Replace the nested `any` with a sweep mirroring
  `build_largest_container_map`: bucket by `addr_space`, sort by
  `(addr_off asc, size desc)`, and drop any varnode strictly enclosed by the
  current open maximal enclosure. Or, simpler: build the container map first
  and keep only the varnodes that map to themselves. Either is O(n log n).

## IR-2 — `will_attach_value` resurrection can mark a node live without re-canonicalizing it
- Dimension: SOUNDNESS (code vs itself)
- Severity: MED
- Confidence: Med
- Location: `crates/strider-ir/src/function/edit.rs:268-305` (`will_attach_value`),
  interaction with `clean` at `:436-452`
- What & why: When a previously-dead producer cone is resurrected (an edit
  attaches a use onto a node whose producer was outside `live_nodes`), the
  cone is walked and each node `insert`ed into `live_nodes`/`roots`, but **no
  node in the resurrected cone is flagged `NEEDS_RECANON` or enqueued**. The
  dedup cache (`NodeCache`) may already hold a *different* canonical
  representative for a structurally-identical node that was minted while the
  cone was dead (e.g. a fold created `Add(x,y)` after the dead `Add(x,y)` was
  detached; the dead one was evicted by `detach_node_inputs`, but a resurrected
  cone re-inserts an equal twin only at the next `compact`/`rebuild_cache`, not
  here). Result: two live structurally-equal cacheable nodes can coexist after
  resurrection with no scheduled merge. The cache layer tolerates twins
  (`node_cache.rs:208-221` documents this), and a later `compact` re-keys, so
  this is precision/idempotence rather than corruption — hence Med, not High —
  but it breaks the "cached live set is fully canonical between cleans" mental
  model that `NEEDS_RECANON` exists to maintain.
- Proposed fix: In `will_attach_value`, when a node is freshly inserted into
  `live_nodes`, also `enqueue_for_recanon(node)` (or at minimum the cone's
  cacheable nodes) so the next `clean` merges any twin. Alternatively document
  that resurrection defers canonicalization to the finalize `compact` and add
  a test pinning that twins-after-resurrect are merged by `compact`.

## IR-3 — `canonicalize_node` merge ignores `replace_value` failure / does not re-enqueue the dead duplicate deterministically
- Dimension: SOUNDNESS (code vs itself)
- Severity: MED
- Confidence: Med
- Location: `crates/strider-ir/src/function/edit.rs:345-360`
- What & why: `canonicalize_node` merges `node` into `twin` via
  `let _ = self.replace_value(node_out, twin_out);`. `replace_value`
  (`:662-678`) ends by `enqueue_killed_def_node(from)` where `from` is
  `node`'s producer — but `from == node` here, and `enqueue_killed_def_node`
  gates on `has_side_effects`. A cacheable node is never side-effecting, so it
  *is* enqueued, good. However the `Ok([node_out])` guard at `:351` silently
  `return`s (dropping the merge) if `node` is somehow not single-output — and
  for a cacheable node that path is "impossible" per the comment, so the early
  return leaves `node` flagged-clear but un-merged and still live (a leaked
  twin) with no error surfaced. The `let _ =` also discards a genuine
  `replace_value` error (the `Result` arm is "never" today but the type is
  fallible). Combined with IR-2, the canonicalization cascade has two silent
  escape hatches.
- Proposed fix: Make the single-output extraction an `expect` (it is a
  validator-guaranteed invariant per the "Panic on invariants" project rule)
  rather than a silent `return`, and propagate or `expect` the
  `replace_value` result so a broken merge is loud, not leaked.

## IR-4 — `get_signed_int` rejects width > 128 but `bit_mask_u128` silently approximates I256/I512
- Dimension: SOUNDNESS (vs semantics)
- Severity: LOW
- Confidence: High
- Location: `crates/strider-ir/src/node/value_type.rs:259-309`
- What & why: `bit_mask_u128(I256)` and `bit_mask_u128(I512)` return
  `u128::MAX` (a deliberate conservative approximation, tested at `:501-505`).
  `get_unsigned_int(I256, v)` therefore returns `Some(v & u128::MAX) = Some(v)`
  unmasked — i.e. it claims success for a 256-bit type while only ever seeing
  the low 128 bits. `get_signed_int` is *inconsistent*: it returns `None` for
  `bits > 128`. So for an I256 value, `get_as_unsigned_int` can return `Some`
  while `get_as_signed_int` returns `None`, and `get_as_int` (which `zip`s
  them) returns `None`. This asymmetry is currently masked because `IntConst`
  never carries I256/I512 inline (they route through the interner and
  `int_const_u128` returns `None` for them at `:159-163`), so the unsigned path
  is unreachable for true wide consts. It is a latent trap: any future caller
  that reaches `get_unsigned_int(I256, _)` directly gets a silently-truncated
  "success".
- Proposed fix: Make `get_unsigned_int`/`bit_mask_u128` return `None`/error
  for `is_wide_int()` types that exceed the `u128` carrier (I256/I512),
  matching `get_signed_int`'s `bits > 128` rejection, so the two stay
  symmetric and a >128-bit query fails loudly.

## IR-5 — `initial_var_index` / `value_vn` populations can desync from the live graph after rewrites (no validator check)
- Dimension: SOUNDNESS (code vs itself)
- Severity: MED
- Confidence: Med
- Location: `crates/strider-ir/src/function/data.rs:229-235` (index),
  `:691-709` (accessors); compaction at `:955-963`
- What & why: `initial_var_index` is described as "advisory and never
  re-checked" (`register_initial_var`, `:702-709`) and `initial_var_for`
  explicitly warns callers to validate the returned id against the use-list
  because it "can hold a node culled-but-not-yet-compacted mid-pipeline."
  `compact` only drops entries whose *NodeId* didn't survive — it does **not**
  drop an entry whose node survived but whose kind was rewritten away from
  `InitialVar(vn)` (a payload-rewrite leaves the NodeId valid). Nothing in
  `validate/` checks that `initial_var_index[vn]` actually points at an
  `InitialVar(vn)` node, nor that `value_vn` keys point at live Phi/clobber
  outputs. A stale entry surfaces as a wrong `initial_sp_value` /
  arg-detection result rather than a validation error. `initial_sp_value`
  (`:722-733`) defensively re-walks instead of trusting the index, which
  confirms the index is known-untrustworthy — but other callers
  (`indirect_resolve`, lifter `read_or_init_var`) trust it.
- Proposed fix: Add a cheap whole-graph validator check (in
  `graph_invariants.rs`) that every `initial_var_index` entry resolves to a
  live `InitialVar(vn)` node with matching `vn`, and that every `value_vn` key
  is a live value output. This converts the "advisory" hazard into an enforced
  invariant for the always-on validator.

## IR-6 — Sub-register write of a 1-bit (`I1`) value relies on undocumented coercion ordering
- Dimension: SOUNDNESS (vs semantics)
- Severity: LOW
- Confidence: Med
- Location: `crates/strider-ir/src/builder/vn_io.rs:182-232`,
  `build_masked_insert:322-349`
- What & why: The direct-container `write_reg_vn` arm coerces `I1` → register
  width via `convert_to_int_if_needed` (`:196-198`, well-documented). The
  *sub-register* arm (`:202-231`) does NOT pre-coerce: it passes `val`
  straight into `build_masked_insert`, which calls `extend_if_needed(val, ty,
  ZeroExtend)`. For an `I1` `val` this zero-extends 0/1 to the container width,
  which is correct — but only because `extend_if_needed` keys on bit width and
  treats `I1` as a 1-bit integer. If a caller ever passes a float `val` to a
  sub-register write (e.g. a scalar-FP write into a sub-slice of a SIMD
  container), `extend_if_needed` errors ("cannot integer-extend non-integer"),
  whereas the direct arm would have bitcast it. The two arms therefore have
  divergent type-coercion behaviour for the same logical operation; the
  sub-register arm is stricter by accident, not by design.
- Proposed fix: Route the sub-register arm's `val` through the same
  `convert_to_int_if_needed` / bitcast prelude as the direct arm before
  `build_masked_insert`, so both write paths accept the identical operand-type
  set, and add a test for an `I1` sub-register write and a float sub-register
  write.

## IR-7 — `remove_node_input` shifts trailing indices in O(tail) per removal → O(n²) in `remove_region_predecessors` worst case
- Dimension: RUNTIME
- Severity: LOW
- Confidence: High
- Location: `crates/strider-graph/src/graph.rs:411-430`
  (`remove_node_input`), driven by
  `crates/strider-ir/src/function/edit.rs:728-773`
  (`remove_region_predecessors`)
- What & why: `remove_node_input` rewrites `input_index` for every trailing
  slot (`:421-427`). `remove_region_predecessors` removes a batch of K dead
  predecessors from a Region with P preds and from each of its M phis, calling
  `remove_node_input` K times per node — each an O(P) tail rewrite, so
  O(K·P·M). Worst case (a Region that fanned in from many dead branches and is
  collapsed) is quadratic in predecessor count. Region fan-in is normally
  small, so this is Low, but a degenerate switch-lowering CFG with a wide
  Region could hit it.
- Proposed fix: When removing many slots from one node, do a single
  filter-rebuild of the input list (O(P)) rather than K independent
  O(P) removals. `remove_region_predecessors` already sorts the batch; it
  could pass the full index set to a new `Graph::remove_node_inputs_batch`.

## IR-8 — `extend_asm_fingerprint` near-sorted fast path degrades on interleaved inputs
- Dimension: RUNTIME
- Severity: LOW
- Confidence: High
- Location: `crates/strider-ir/src/function/data.rs:746-767`
- What & why: The append loop appends in order while `addr > last`, else sets
  `needs_resort` and re-`sort_unstable` + `dedup` the whole vector. If a rewrite
  unions a fingerprint whose addresses interleave with the existing (common
  when absorbing a sibling cone's mixed addresses), every such call re-sorts
  the full list — O(m log m) per absorption, and absorptions cascade through
  `canonicalize_node`/`replace_value`. On a hot rewrite this is repeated full
  re-sorts of a growing list. Bounded (fingerprint lists are small per the
  `SmallVec<[u64;2]>` choice) so Low, but the "fast path" is illusory for the
  realistic interleaved case.
- Proposed fix: Since both `existing` and `contributors` are individually
  sorted-deduped, use a single linear merge into a fresh `SmallVec` instead of
  append-then-maybe-resort. O(m+k), no re-sort.

## IR-9 — `gc_wide_consts` / `compact` cache rebuild is correct but unconditionally O(arena) on every compact even with zero wide consts
- Dimension: RUNTIME
- Severity: LOW (verification note — no bug)
- Confidence: High
- Location: `crates/strider-ir/src/function/data.rs:984-1051`
- What & why: Verified sound: `gc_wide_consts` scans `all_node_ids()` (O(N))
  to find wide nodes; when none exist it resets the interner and returns
  `false`, so `rebuild_cache` is skipped (`:984-986`). The O(N) scan happens on
  every `compact` regardless, but `compact` already walks the arena, so this is
  not additive complexity. No fix needed — recorded so a future audit doesn't
  re-flag the scan. (The masking choke-point `create_node_attributed`
  `:796-850` and the LE/BE shift formulas in `vn_io.rs` were likewise verified
  sound; the SOUNDNESS comments there match the code.)

---

## Edge cases lacking test coverage (proposed test names; not written)

- `will_attach_value_resurrection_re_canonicalizes_twin` — resurrect a dead
  cone that is a structural twin of a live cacheable node and assert the next
  `clean()` merges them (covers IR-2).
- `canonicalize_node_merge_is_loud_on_broken_invariant` — a merge where
  `replace_value` would fail must not silently leak the duplicate (covers
  IR-3).
- `get_unsigned_int_i256_does_not_falsely_succeed` — pin the desired symmetry
  with `get_signed_int` for >128-bit types (covers IR-4).
- `validate_flags_stale_initial_var_index_entry` — rewrite an `InitialVar`
  node's payload, keep its NodeId, and assert the validator (post-fix) flags
  the now-stale index entry (covers IR-5).
- `write_reg_vn_subregister_accepts_i1_and_float_like_direct_arm` — symmetric
  coercion across the two write arms (covers IR-6).
- `remove_region_predecessors_wide_fanin_is_linear` — a Region with a large
  dead fan-in collapsed in one batch; assert single-rebuild behaviour
  (covers IR-7).
- `dedup_overlapping_largest_handles_many_aliasing_uniques` — a tracked set
  with hundreds of overlapping UNIQUE slices; a perf/behaviour pin for IR-1.
