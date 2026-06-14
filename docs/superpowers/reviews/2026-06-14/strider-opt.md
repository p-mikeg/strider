# strider-opt deep audit — 2026-06-14

READ-ONLY audit of `crates/strider-opt` (optimization passes, OptimizerPipeline,
rewrite_rule/apply_rules driver, memory-SSA engine, sp_expr, value_range,
indirect-branch classifiers, post-passes). Every claim below was verified against
the actual code (comments/docs/CLAUDE.md/memory treated as suspect, not ground
truth). Findings ordered by severity within dimension.

## Severity counts

- HIGH: 0
- MED: 7
- LOW: 17
- INFO: several (verified-sound notes, not tallied)

The core soundness story is solid. The flag-tree identities are all
semantically equivalent, the memory-SSA walk correctly treats `MemPhi` as an
opaque join (never picks a single predecessor) and `Call` as a barrier,
`value_range` has no arithmetic-widening transfer functions and fails *open*
(toward top) everywhere, and predecessor↔phi-slot index alignment is correct in
every control-edge-removing pass. No confirmed miscompile was found. The MED
findings are latent width-assumptions the validator does not backstop, two
runtime blow-ups inside loops, and two confidently-wrong-target risks in the
jump-table classifier (the design's safety margin is over-approximation, and
these are the spots where a *wrong* single target can escape it).

---

## MED findings

### M1 — Jump-table fold rebuilds `default_pipeline()` per (candidate × index value); full function clone per fold
- Dimension: RUNTIME
- Confidence: HIGH
- Location: `post_opt/indirect_branch_resolve/table.rs:248,255` (`fold_dispatch_to_const`)
- Evidence: `let mut clone = base.clone();` (full function clone) and
  `let pipeline = crate::default_pipeline();` are both **inside**
  `fold_dispatch_to_const`, which `enumerate_targets` calls once for every value
  in `lo..=hi` for every candidate. With `MAX_TABLE_ENTRIES = 4096` and several
  candidates, this is thousands of full-function clone + full-pipeline runs per
  branch, and the post-pass re-runs on every orchestrator rebuild iteration. The
  pipeline construction in particular is loop-invariant and rebuilt every call.
- Fix: Hoist `default_pipeline()` out of the per-value loop (build once per
  branch). Memoize `(idx_value, subst) → target` across orchestrator iterations
  when `base` is unchanged. Add a distinct early-exit cap on fold attempts for a
  wrong candidate (separate from the 4096 entry cap).
- Missing test: a perf/regression test bounding the number of `fold_dispatch_to_const`
  invocations for a multi-candidate cone.

### M2 — Recovered jump-table `Multiple` targets get no executable/mapped-memory validation
- Dimension: SOUNDNESS
- Confidence: HIGH
- Location: `post_opt/indirect_branch_resolve/table.rs:109-114` (fold returns any
  in-range constant); consumer `strider-cfg/src/builder/region_builder.rs` only
  range-checks `[start, start+fn_max_size)` via `is_branch_tail_call_nocheck`.
- Evidence: A folded constant inside the function's address window is accepted as
  a CFG edge with no check that it points at executable/mapped memory or an
  instruction boundary; failure is deferred to decode time. The rodata arm reads
  through `rom` but the *folded* constant is never re-validated against rom
  executability.
- Fix: Gate each recovered target through a "plausible code address" predicate
  (mapped + executable, ideally decodable); degrade a decode failure on one
  `Multiple` target to a defer rather than committing the whole edge set.
- Missing test: index range folds to in-range-but-non-executable / mid-instruction
  constants → assert defer, not acceptance.

### M3 — `enumerate_targets` accepts a candidate that folds to an index-independent constant
- Dimension: SOUNDNESS
- Confidence: MED
- Location: `post_opt/indirect_branch_resolve/table.rs:122-134` (`enumerate_targets`),
  `:144-196` (`find_index_candidates`)
- Evidence: The accept rule is "every value in `lo..=hi` folds to *a* constant";
  there is no requirement that distinct indices yield distinct (or
  index-dependent) targets. If the real index is itself foldable, or a decoy
  candidate dominates a guard that makes the dispatch constant, a wrong candidate
  can yield a confidently-wrong single-element `Multiple`. Candidates are tried
  tightest-range-first, so the first spurious match wins over a later correct one.
- Fix: Require the folded target to actually depend on the substituted candidate
  (≥2 distinct indices must yield ≥2 distinct targets), or confirm the candidate
  is structurally on the dispatch's addressing path. Cross-check the single-target
  case against the rodata arm.
- Missing test: cone containing a second finite-range value off the addressing path
  → assert the real index is selected (or the decoy rejected).

### M4 — ConstantFold const-eval rule 1 trusts operand width == output width; validator does not enforce it
- Dimension: SOUNDNESS (latent)
- Confidence: MED
- Location: `opt/constant_fold/eval_int.rs:40-44` consumed by `opt/constant_fold/rules.rs:549-557`
- Evidence: `eval_int_binary` masks both operands to the single output type `ty`
  and computes the shift-distance guard `r >= bits` against the output width. The
  node-signature table types `IntBinaryOp` inputs as `AnyInt`, and local-typing
  checks only slot category, never that LHS-width == RHS-width == output-width.
  For a shift with a wider output than operand the distance semantics shift. No
  live miscompile today (lifter keeps ops width-consistent); a latent assumption.
- Fix: `debug_assert` operand-width == output-width at rule entry, or drive operand
  / shift-amount masking from each operand's own type.
- Missing test: `eval_int_binary_mixed_width_operands` — fold
  `Add(IntConst:I64(0xFFFF_FFFF_FFFF_FFFF), IntConst:I32(1)):I32` → assert I32-correct 0.

### M5 — ConstantFold `int_cmp` rule masks both operands to the LHS width only
- Dimension: SOUNDNESS (latent)
- Confidence: MED
- Location: `opt/constant_fold/eval_int.rs:134-143`; rule `opt/constant_fold/rules.rs:580-588`
- Evidence: `eval_int_cmp` masks both `l` and `r` to one `ty` = `first_value_input_type`
  (the LHS type). If RHS is wider, masking it to LHS width silently changes the
  compared value and can flip a `Less`/`Sless`/`Carry` result. Same root cause as
  M4; latent because real cmps are width-consistent.
- Fix: Assert LHS-width == RHS-width before folding, or reject the fold on mismatch.
- Missing test: `eval_int_cmp_rejects_or_handles_width_mismatch` —
  `Sless(IntConst:I8(0x80), IntConst:I32(200))` with the intended verdict pinned.

### M6 — memory-SSA walk is O(loads × chain) per fixed-point iteration; no cross-load memoization
- Dimension: RUNTIME
- Confidence: HIGH
- Location: `sp_expr/mem_ssa/mod.rs:261-272` (`walk_from` allocates a fresh memo per
  call); driven per-load from `opt/load_forward/mod.rs:71-73`
- Evidence: Each `Load` triggers a fresh memory-SSA walk with a per-call `Resolve`
  memo. The per-walk memo makes a single walk linear, but two loads over the same
  long store chain each re-walk from scratch, and `LoadForward` runs inside the
  fixed-point loop, repeating the O(loads × chain) cost every iteration. The
  narrowing side-effect (repointing a load's mem edge onto its nearest clobber)
  partially mitigates across iterations but the first iteration pays full cost,
  and it only collapses proven-disjoint runs, not phi/call-heavy chains. The
  `SpExprMemo` (address decomposition) is shared but the chain resolution is not.
- Fix: Acknowledge the worst case in docs (current docs imply linearity); if
  profiling warrants, cache nearest-clobber per `mem_value`.
- Missing test: N loads over an N-deep disjoint store chain → assert narrowing
  collapses the chain so iteration 2 is cheap.

### M7 — `enumerate_targets` cone walk can enumerate a loaded table *entry* as the index when an unrelated guard dominates it
- Dimension: SOUNDNESS
- Confidence: MED
- Location: `post_opt/indirect_branch_resolve/table.rs:144-191`
- Evidence: The `entry_load` exclusion depends on
  `ranges.dominating_guard(v, branch).is_none() && !is_and_masked(...)`. A loaded
  table entry that also happens to be reached under an unrelated dominating `Less`
  guard loses its exclusion and is enumerated as an index over its guard range —
  folding the load-of-the-load into bogus sequential targets, exactly the case the
  in-code comment warns against. `dominating_guard` filters only by region
  dominance, not by whether the guard constrains this value's index role.
- Fix: Exclude a load-derived value as index unless the guard is provably on the
  same value feeding the address arithmetic, or require the candidate participate
  in the dispatch's address computation (Mul/shift by stride).
- Missing test: rodata byte-entry table whose loaded entry is also dominated by an
  unrelated range guard → assert it is not enumerated as the index.

---

## LOW findings

### L1 — LoadForward BE narrow path mints `IntConst(Small)` at a wide (I80/I128) output type
- Dimension: SOUNDNESS (IR representation invariant)
- Confidence: HIGH
- Location: `opt/load_forward/mod.rs:208-215`
- Evidence (verified): when a wide store is forwarded into a narrower BE load,
  `data_ty` is I80/I128 and the shift constant is hand-built as
  `NodeKind::IntConst(IntPayload::Small(...))` typed `data_ty`. The canonical
  `build_int_const` routes I80/I128 through the `wide_const_interner` and only
  emits `IntPayload::Wide`. The validator only checks `IntConst(Wide)` nodes, so
  this `Small`-at-wide-type node passes validation but violates the convention and
  won't dedup against the wide form. Value is correct (shift amount ≤56 fits u64);
  any consumer reading wide consts by assuming `Wide` payload would misread it.
- Fix: Build the shift constant at ≤I64 (the amount is always small) or route it
  through `build_int_const`; or bail for `data_ty` that doesn't fit u64.
- Missing test: BE function with an I128/I80 store forwarded into a narrower load →
  assert the shift constant is a valid `Wide` const (or the pass bails).

### L2 — LoadReadOnly straddling-read safety is fully delegated to the external `ReadOnlyMemory` fill-all-or-error contract
- Dimension: SOUNDNESS
- Confidence: MED
- Location: `opt/load_readonly/mod.rs:159-168`
- Evidence: The pass never computes/checks `addr + size` against region bounds; it
  relies entirely on `rom.read` erroring on a partial overlap. A non-conforming
  impl that partial-fills + returns `Ok` would yield a partly-bogus folded const.
- Fix: Document the hard dependency at the call site or add a region-end check if
  extents are exposed.
- Missing test: a `MockRom` whose region ends mid-load (addr valid, addr+size past
  end) returns `Err` → load not folded.

### L3 — LoadReadOnly cannot distinguish MMIO/volatile from rodata
- Dimension: SOUNDNESS (architectural)
- Confidence: HIGH
- Location: `opt/load_readonly/mod.rs:14-31` + whole `apply`
- Evidence: No per-load volatile flag and the chain is intentionally not consulted;
  the only guard against folding MMIO is operator discipline in rom curation. A
  too-broad rom (mapping a writable/MMIO segment) silently miscompiles.
- Fix: None without a volatility model. Verify the orchestrator's rom strictly
  excludes writable/MMIO (e.g. `.got.plt` added by relocation autoload reaches only
  the code reader, never the rom reader).

### L4 — sp_expr `SpExpr::shifted` uses `wrapping_add`; deep Add chains silently wrap the offset
- Dimension: SOUNDNESS
- Confidence: MED
- Location: `sp_expr/decompose.rs:38-43`, classified at `:152-157`
- Evidence: `offset.wrapping_add(delta)` converts an i64 offset overflow into a
  wrong concrete offset that looks like a valid nearby slot, instead of bailing.
  The downstream alias oracle (`ranges_disjoint`) then reasons on the wrapped
  offset. Real frames have small offsets; the decomposer is fed arbitrary lifted
  arithmetic.
- Fix: `checked_add`, decompose to `None` (opaque base) on overflow — fail-closed.
- Missing test: Add chain summing past `i64::MAX` → assert `None`, not a wrapped offset.

### L5 — sp_expr `And(sp, mask)` arm accepts any constant mask as an opaque stack base
- Dimension: SOUNDNESS
- Confidence: MED
- Location: `sp_expr/decompose.rs:174-193`
- Evidence: The And arm only checks one operand is `int_const` and the other is
  SP-rooted; it does not require the constant be an *alignment* (high-run-of-1s)
  mask. `And(sp, 0x0F)` (low-nibble extraction, value in [0,15]) is treated the
  same as `And(sp, 0xFFFFFFF0)` (alignment), yielding a stack base. Blast radius is
  limited (different base ⇒ may-alias unless opted in) but the classification is
  wrong and could feed `distinct_sp_bases_disjoint` Disjoint verdicts.
- Fix: Gate on the constant being a contiguous high-bit mask; reject low-bit masks.
- Missing test: `And(sp, 0xF)` → decompose to `None`, not an opaque base.

### L6 — sp_expr `decompose` does not memoize `None` for multi-output-producer values
- Dimension: RUNTIME (caching gap)
- Confidence: HIGH
- Location: `sp_expr/decompose.rs:114-134`
- Evidence: On a miss the RPO sweep inserts memo entries only for single-output
  producers (`node_outputs_exact::<1>`); a value whose producer has ≠1 output is
  never inserted, so `decompose` re-walks the whole cone on every query. SP
  producers are single-output today so it rarely bites; `classify_addr` runs on
  arbitrary addresses.
- Fix: After the sweep, `memo.insert(value, None)` if `value` is still absent.
- Missing test: query `decompose` twice on a multi-output-producer value → assert
  the second is a memo hit.

### L7 — `value_range::single_control_consumer` takes the first use, asserting uniqueness only by comment
- Dimension: SOUNDNESS
- Confidence: MED
- Location: `value_range/mod.rs:700-706`
- Evidence: `value_uses(ctrl_val).map(...).next()` picks one consumer. If a control
  value ever had >1 consumer, a guard could be attached to a consumer whose
  dominance doesn't imply the edge was taken — an over-tight interval that could
  *drop* a jump-table target (the one place an under-restriction is possible).
  Single-sink control is a structural invariant, but unchecked here.
- Fix: `debug_assert_eq!(value_uses(ctrl_val).count(), 1)` or `.exactly_one()`.
- Missing test: control value with two consumers → assert fail-closed (skip guard),
  not fail-tight.

### L8 — `value_range` `Sless` guard with a negative constant uses the unsigned-masked bound
- Dimension: SOUNDNESS (sound but fragile)
- Confidence: HIGH
- Location: `value_range/mod.rs:639-692`
- Evidence: `n` comes from `int_const_u128` (unsigned mask). For `Sless(v, neg)`
  the true set is empty but the interval becomes near-top — sound over-approx,
  resting entirely on the `is_sign_bit_known_zero` gate.
- Fix: None required; add a regression test pinning the over-approx.
- Missing test: `If(Sless(v, IntConst(0xFFFF_FFFF:i32)))` with v sign-bit-known-zero
  → assert top-ish interval; companion without the gate → assert no guard recorded.

### L9 — `value_range` `Add(X, const)` back-prop mask uses the operand width, not the guarded value width
- Dimension: SOUNDNESS
- Confidence: MED
- Location: `value_range/mod.rs:416-441`, driver `:577-583`
- Evidence: The `lo <= hi` gate correctly drops wrapping subtractions (sound). But
  the mask is the operand's width while the interval came from a compare on the add
  result; a typed width mismatch between add output and operand could misalign.
  Theoretical (lift keeps them same-width).
- Fix: None required; add a wrap-drop test.
- Missing test: `If(Less(Add(X, K), N))` with K large / N small (wrap) → assert no
  guard recorded on X; plus a same-width happy-path shift correctness test.

### L10 — FlagCmpCanonicalize `absorb_cr_pack_fingerprints` is O(pack²) via `Vec::contains`
- Dimension: RUNTIME
- Confidence: HIGH
- Location: `opt/flag_cmp_canonicalize/mod.rs:486-511`
- Evidence: The DFS visited-set is a `Vec` tested with `.contains` inside the
  worklist loop — O(k²) in pack interior nodes. Bounded in practice (depth cap 32)
  but violates the workspace "O(n)/O(n log n), prefer entity-utils" convention.
- Fix: Use `DenseEntitySet<NodeId>` for the visited set.

### L11 — `CallStackArgCollect` re-walks the same memory chain prefix per slot (no per-call clobber memo)
- Dimension: RUNTIME
- Confidence: MED
- Location: `post_opt/call_stack_args/mod.rs:73,82`
- Evidence: One `SpAliasCfg::call_blocking` per call, then `reaching_store` per slot,
  each running `find_nearest_clobber` from the same `mem_value` — O(slots × chain)
  per call. SP decomposition is memoized; the clobber walk is not.
- Fix: Cache the per-`mem_value` nearest-clobber chain if profiling warrants.

### L12 — ConstantFold F32 folds truncate stored bits with `as u32` (unmasked-input assumption)
- Dimension: SOUNDNESS (latent)
- Confidence: MED
- Location: `opt/constant_fold/eval_float.rs:75,94,103`
- Evidence: `eval_binary!(f32, op, bits_l as u32, ...)`; `build_float_const` stores
  bits unmasked. If an F32 `FloatConst` ever carries nonzero bits 32–63 the
  truncation silently drops them rather than skipping. Lifter stores clean low-32
  bits today.
- Fix: Mask F32 inputs (`(bits_l & 0xFFFF_FFFF) as u32`) or `debug_assert` high bits clear.
- Missing test: `eval_f32_ignores_high_garbage_bits`.

### L13 — ConstantFold float folds use host f32/f64 default-rounding arithmetic
- Dimension: SOUNDNESS (documented scope limit)
- Confidence: HIGH
- Location: `opt/constant_fold/eval_float.rs:13-17,52`
- Evidence: `Add/Mul/Div` computed with host ops; NaN withheld but inexact results
  in non-default target rounding modes are not. Matches IEEE round-to-nearest, so
  agrees with the vast majority of targets.
- Fix: None required; if strictness wanted, restrict folding to exactly-representable results.
- Missing test: `eval_float_div_inexact_documents_round_to_nearest` (pin the contract).

### L14 — ConstantFold `sub`-based reassoc rules are dead after `Neg(IntConst)` folds
- Dimension: SOUNDNESS (confluence/hygiene)
- Confidence: HIGH
- Location: `opt/constant_fold/rules.rs:107-140`; Neg-fold at `:563-573`
- Evidence: `sub(...)` compiles to `Add(x, Neg(IntConst c))`; the Neg-fold turns the
  operand into a plain `IntConst`, after which the `sub` LHS can never match and the
  `add` reassoc family takes over. The three sub-rules are effectively dead weight.
- Fix: Drop them in favour of the `add` family, or document the single-visit window.
- Missing test: `(x-3)-4` → assert final IR is `Add(x, IntConst(-7))` regardless of path.

### L15 — `node_known_bits` arity accessors panic (not `Err`) on malformed reachable IR
- Dimension: SOUNDNESS (panic-safety, doc vs code)
- Confidence: HIGH
- Location: `opt/known_bits/mod.rs:159-162,283-286,297-300,324-327,361-364`
- Evidence: `node_inputs_exact::<N>().expect(...)` panics on wrong arity, while the
  `analyze` doc-comment claims errors return as `Err`. Acceptable under the
  panic-on-validator-invariant policy (validator runs before KnownBits), but the
  doc-comment is inaccurate.
- Fix: Align the doc-comment.

### L16 — `MAX_TABLE_ENTRIES` (4096) silently drops legitimate large switch tables
- Dimension: SOUNDNESS (fail-closed capability limit)
- Confidence: HIGH
- Location: `post_opt/indirect_branch_resolve/mod.rs:77`, `table.rs:180`
- Evidence: A switch with >4096 arms never resolves; the branch defers (reported in
  `unresolved_indirect_branches`). Sound — no wrong edges — just a silent limit.
- Fix: None required.

### L17 — `FunctionArgDetect` `arg_loads[0]` / `span[&cursor]` indexing relies on implicit key-set invariants
- Dimension: SOUNDNESS (panic-safety)
- Confidence: HIGH
- Location: `post_opt/function_args/mod.rs:231-248`
- Evidence: `arg_loads[0]` and `span[&cursor]` are safe only because `groups`/`span`
  share key sets and a present key always has ≥1 element. Holds in practice; an
  implicit invariant, not an enforced one.
- Fix: `arg_loads.first().expect(...)` for clarity; test a disqualified slot between
  two valid groups (gap handling).

---

## Verified SOUND (checked adversarially, not re-flagged)

- **memory-SSA** `MemPhi` join returns the shared value on agreement and the phi
  itself on disagreement (never picks a predecessor); `Resolve` state machine
  (`Unseen→InProgress→Done`) guarantees termination on loop back-edges; `Call`/
  `CallOther`/opaque producers fail-closed as clobbers. (`sp_expr/mem_ssa/mod.rs`)
- **LoadForward** exact-match requires `AliasVerdict::Match` (equal offset) AND
  type-equal-or-wider-integer-store; narrower store bails; space equality enforced
  upstream; never synthesizes a value-Phi; BE shift logic covers the high-byte case.
- **LoadReadOnly** endianness decode places bytes per `Endianness` then masks to
  width; I80 (10-byte) routes to the wide interner; non-constant / >u64 addresses
  fail-closed.
- **KnownBits** every unmodeled op (`Add`/`Mul`/`Neg`/`SShiftRight`/cmps/`Load`/
  `Phi`/`Call`) falls through to fully-unknown; AND/OR/XOR/shift/extend/truncate/
  popcount/lzcount transfers are exact and width-masked; wide types I80..I512 are
  gated out (no u64/u128 over-claim); rewrite re-masks to declared width. No meet
  is needed because all handled kinds form a DAG broken by always-unknown Phis.
- **value_range** has no arithmetic-widening transfers; every arm fails open
  (KnownBits `max_value` is an upper bound; guards only upper-bound; Phi merge is
  union/hull with fail-closed top; cycle/missing-type → top; `Add` back-prop drops
  wrapping). `upper_exclusive` fails open (`None` ⇒ no enumeration).
- **FlagCmpCanonicalize** all flag-tree identities (signed GE/LT/GT/LE via
  Sborrow/Sless, unsigned via Carry/Less, decomposed ARM/Thumb shapes, ja/jbe
  const-folded variants) are semantically equivalent across signed/unsigned
  boundaries; constant guards mask N/M/C1 to operand width; CR-bit `cmp?`
  propagation refuses to rewrite when the tested bit is opaque.
- **IfCondInversion** I1 guard present (via `bool_not`'s pinned-I1 output);
  branch swap uses pre-collected stable `UseId`s (order-independent, no iterator
  invalidation).
- **Predecessor↔phi-slot alignment** correct in `remove_region_predecessors`
  (highest-index-first removal, token at slot 0, phi value slot = `pred+1`),
  `cfg_detach`, `dead_branch`, `region_collapse`, `phi_collapse`.
- **rewrite_rule / apply_rules** fingerprint absorption is superset-only and
  covers the bare-capture identity-fold case; `replace_value` enqueues the orphan
  for `clean`; `apply_rules_count` walks reachable nodes once (O(nodes × rules)).
- **Pipeline** fixed-point cap (`MAX_ITERS = 1024`) guards divergence; `sp_memo`
  cleared at every drain point; post-passes drain after each.
- **ConstantFold** shift ≥ width / div-rem-by-zero / INT_MIN÷-1 / wide-const reads
  / `x^x`-needs-same-ValueId / `(x|A)&B` algebra / reassoc termination all sound.
- **StackOffsetDetect** purely additive, order-independent; base-equality gate
  prevents cross-base offset confusion.
- **CallStackArgCollect** slot cursor strictly advances (`span.max(1) >= 1`),
  terminates at the first non-anchored slot, bounded by graph size.

---

## Top recommendations

1. **M1** — hoist `default_pipeline()` out of the per-value fold loop; memoize fold
   results. Clear, cheap runtime win on the dominant indirect-branch cost.
2. **M2 / M3 / M7** — the three jump-table soundness gaps where a confidently-wrong
   single/multiple target can escape the over-approximation safety margin. Add a
   code-address-plausibility gate and an index-dependence check.
3. **M4 / M5** — backstop the ConstantFold operand-width assumptions with
   `debug_assert`s (the validator does not currently enforce them).
4. **L1 / L4 / L5** — three latent soundness-hygiene fixes (wide-const
   representation, offset overflow fail-open, alignment-mask gating).
