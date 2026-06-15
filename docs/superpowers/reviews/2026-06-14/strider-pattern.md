# Deep audit — `strider-pattern`

Date: 2026-06-14
Scope: `crates/strider-pattern/src` — `Pat`/`Capture`/`Matcher`/`Match`,
fluent builders, `MatchPat`/`TemplatePat`, `find_all`/`find_first`/`find_joined`/
`match_at`, rewrite-rule asm-fingerprint propagation (`instantiate`,
`matched_nodes`), capture-coverage, commutative matching.

Method: read every source file; verified findings against real call paths
(rewrite path lives in `crates/strider-opt/src/rewrite_rule.rs`, audited for
the fingerprint/coverage contract that originates in this crate). Comments
were not treated as ground truth.

Note on the brief: the prompt names `match_at_any` and "multi-sink patterns
matchable". `match_at_any` **no longer exists** in this crate (the jump-table
classifier that used it was rewritten to clone+optimize per the 2026-06-13
memory note); the public entry points are `find_all` / `find_first` /
`find_joined` / `match_at`. Multi-sink patterns are buildable but
**not matchable** — `Pattern::root` errors on >1 sink. Both verified.

---

## Severity summary

- HIGH: 0
- MED: 3
- LOW: 4

The matcher/rewrite core is in good shape: commutativity is data-driven and
has dense test coverage (incl. swap-leak, identical-operand, nested
commutative/non-commutative, child-`when_match` redrive); bindings rollback
is a clean journal truncate with proofs; the match/template split is enforced
at compile time; the asm-fingerprint superset contract holds on the live
rewrite path. Findings below are mostly hardening + simplification.

---

## MED-1 — `find_joined` is an unbounded cartesian product when patterns share no captures

- Dimension: RUNTIME / EDGE CASE
- Severity: MED
- Confidence: HIGH
- Location: `crates/strider-pattern/src/matcher/mod.rs:263-299` (`find_joined`), `:379-395` (`prefix_agrees`)

What & why: `find_joined` cross-products every pattern's hit list and filters
each tuple with `prefix_agrees`. `prefix_agrees` only rejects a tuple when a
**shared** capture disagrees — for two patterns that share *no* capture it
returns `true` unconditionally (the inner loop finds no `m_binding` to
compare). So joining two patterns with N₁ and N₂ hits and no shared capture
yields the full N₁×N₂ tuples, all "agreeing". The doc-comment advertises the
O(∏ Nᵢ) worst case as if it were the join-failure case, but here it is the
*success* case: the result is a meaningless cartesian explosion. On a real
function a kind-`Any`-ish pattern can have hundreds of hits; a 3-way join
with no shared captures is hundreds-cubed tuples held in memory
(`acc.clone()` per surviving prefix at each step).

The intent of a "join" is shared-capture correlation; a join with zero shared
captures is almost always a caller bug, not a request for a cartesian
product.

Proposed fix: at construction, detect whether each subsequent pattern shares
at least one capture with the accumulated prefix's capture set; if it shares
none, either (a) return an error (`anyhow!("find_joined: pattern {i} shares
no capture with the others")`), or (b) document-and-keep but add an
explicit guard so the explosion is opt-in. (a) is the safer default and turns
a silent blowup into a loud authoring error.

Suggested edge-case test (not yet present): `find_joined_no_shared_capture_is_rejected`
— two independent `add(...)` patterns with disjoint captures over a function
with several adds; assert `find_joined` errors (or returns empty) rather than
returning |adds|² tuples.

---

## MED-2 — `find_joined` can emit duplicate tuples; equal-tuple dedup absent

- Dimension: SOUNDNESS (result correctness) / RUNTIME
- Severity: MED
- Confidence: MED
- Location: `crates/strider-pattern/src/matcher/mod.rs:282-297`

What & why: the incremental cross-product appends one tuple per
`(prefix, m)` that agrees, with no dedup. When two distinct hits of pattern *i*
bind the shared capture to the same node but differ on a *non-shared* binding
(e.g. an internal `any()` leaf matched two ways, or a multi-output node where
two outputs satisfy the same shared-node capture), the consumer receives two
tuples that are equivalent for every shared capture. Combined with the
node-vs-value agreement relaxation (`prefix_agrees` compares at NODE
granularity for mixed `Node`/`Value` bindings, line 385-388), a downstream
consumer iterating joined tuples and acting per shared node will double-act.
The rewrite/classifier consumers in `strider-opt` historically assumed one
tuple per correlated site.

Proposed fix: after building `acc`, dedup tuples by their *shared-capture*
binding signature (resolved to nodes), or document explicitly that
`find_joined` results are not deduplicated and consumers must key on shared
captures themselves. Verify the jump-table / Python `find_joined` callers
don't already rely on uniqueness.

Suggested edge-case test: `find_joined_dedups_on_shared_capture_when_internal_leaf_varies`.

---

## MED-3 — `match_branch_consumer` swallows malformed-branch errors and drops branch captures

- Dimension: SOUNDNESS / EDGE CASE
- Severity: MED
- Confidence: HIGH
- Location: `crates/strider-pattern/src/node_builders/flow.rs:378-400` (`match_branch_consumer`), `:297-312` (`with_true`/`with_false`)

What & why: `with_true` / `with_false` take a finished `Pattern` and match it
node-wise against the branch's single control-output consumer via
`matcher.match_at(first, pat).ok().flatten().is_some()`. Two issues:

1. `.ok()` discards a `Pattern::root` error. If the supplied branch pattern is
   multi-sink (a valid graph a user can build with shared captures) or rootless,
   `match_at` returns `Err`, which is silently converted to "branch did not
   match". A user typo in the branch pattern therefore *silently never fires*
   instead of surfacing the build error — exactly the kind of silent
   match-failure the matcher elsewhere goes out of its way to avoid (cf.
   `try_match_node`'s deliberate rejection of value roots on `Return`, walk.rs:74-85).

2. Branch captures are not propagated into the outer match (documented at
   flow.rs:376-377). The branch sub-pattern binds against an *isolated*
   `Bindings` inside `match_at`; those bindings are dropped. A user who writes
   `if_node().with_true(call().arg(0, var(x)).build())` and later reads `x`
   from the outer `Match` gets `None` with no diagnostic. This is a real
   foot-gun: `var(x)` strongly implies the capture is observable.

Proposed fix: (1) propagate the `Pattern::root` error out of the branch walk
instead of `.ok()`-swallowing it (thread a `Result` through the `BranchWalk`
boxed-fn, or validate the branch pattern's root once at `with_true`/`with_false`
build time and panic/error eagerly like `check_capture_coverage` does). (2)
Either wire branch captures into the outer bindings, or rename the branch API
to make the isolation explicit and reject capture-bearing branch patterns at
build time.

Suggested edge-case tests: `with_true_multi_sink_branch_pattern_errors_not_silently_skips`;
`with_true_branch_capture_is_unbound_in_outer_match_is_documented_or_propagated`.

---

## LOW-1 — Cast-walk-through skipped nodes are absent from the match footprint (latent superset-only gap)

- Dimension: SOUNDNESS (asm-fingerprint superset contract)
- Severity: LOW (latent — not reachable on the current rewrite path)
- Confidence: HIGH
- Location: `crates/strider-pattern/src/matcher/walk.rs:229-241`; `cast_walk_through.rs:26-45`; `Bindings::record_matched` walk.rs:303

What & why: when the matcher unwraps a cast via `skip_casts` (cast-mask
fallback), the skipped cast `NodeId`s are **never** passed to
`record_matched`, so they do not appear in `Match::matched_nodes()`. The
rewrite engine (`rewrite_rule_impl`, strider-opt rewrite_rule.rs:189-192)
absorbs `matched_nodes` fingerprints into the RHS, then `replace_value`
culls the dead cone. A skipped cast whose only use was the rewritten root
becomes dead and is culled — but its asm-fingerprint was never absorbed, so
the survivor's fingerprint loses that cast's address, **violating the
superset-only contract**.

Why LOW: the rewrite LHS always comes from `MatchPat::into_pattern()`, which
seals `cast_mask: empty` (matcher/graph.rs:46-51); `ignore_casts` is only ever
applied to query `Pattern`s, and no `rewrite_rule` / `rewrite_rule_runtime`
LHS in `strider-opt` carries a cast mask. So today the bug is unreachable.
It becomes live the moment someone builds a cast-mask `Pattern` and feeds it
to `rewrite_rule_runtime` (a supported FFI path).

Proposed fix: in walk.rs, when the cast-skip fallback succeeds, record the
skipped cast node(s) into the footprint (have `skip_casts` return the chain of
unwrapped nodes, or re-derive them, and `bindings.record_matched(...)` each).
Cheap insurance even though currently unreachable; the footprint is *defined*
as "every IR node the match relied on".

Suggested edge-case test: `cast_walk_through_records_skipped_cast_in_footprint`.

---

## LOW-2 — `instantiate` silently closes input-slot gaps and overwrites duplicate slots for raw-built templates

- Dimension: SOUNDNESS (author-owned, raw builder only) / EDGE CASE
- Severity: LOW
- Confidence: HIGH
- Location: `crates/strider-pattern/src/template/mod.rs:271-278`

What & why: inputs are collected into a `BTreeMap<usize, ValueId>` then
`.into_values()`. A gap in the slot set (raw `TemplateBuilder` wires slots 0
and 2 but not 1) is silently *closed* — slot 2's producer lands at IR input
index 1 — and a duplicate slot silently overwrites the earlier edge. The
behaviour is documented (mod.rs:266-270, rewrite_rule.rs:97-116) and the typed
builders never trigger it, so this is author-owned for raw-verb templates. But
the rewrite path deliberately skips `validate`, so a structurally-wrong RHS
produces silently-wrong IR with no diagnostic anywhere.

Proposed fix: cheap hardening — after collecting, assert the `BTreeMap` keys
are exactly `0..len` (contiguous, no gaps) and error otherwise; a duplicate
slot is already lossy and could `debug_assert`. Turns a silent mis-wire into a
loud error for the one unsafe construction path, at negligible cost.

Suggested edge-case test: `instantiate_noncontiguous_raw_template_slots_errors`.

---

## LOW-3 — Deep nested commutative patterns have no memoization → worst-case exponential backtracking

- Dimension: RUNTIME
- Severity: LOW
- Confidence: MED
- Location: `crates/strider-pattern/src/matcher/walk.rs:198-257`

What & why: `try_match_at` retries the swapped operand order on each arity-2
commutative node with no memo of `(pat_node, ir_node) → fail`. A pattern that
is a chain of K nested commutative ops where each level *almost* matches both
ways can re-descend the same IR sub-cone 2^K times. The bench
(`benches/matcher.rs`) measures a single 3-node pattern with many hits, not a
deep commutative pattern, so this isn't exercised. Real authored patterns are
shallow (≤ ~5 levels), so this is LOW today; it matters only if pattern depth
grows or a pattern is run over a large matching cone.

Why not higher: per the project scalability rule, *graph-size* traversal is
O(n) (kind-index prefilter + single reachable walk); the exponential factor is
in *pattern depth*, which is author-controlled and small. The memory note on
the matcher confirms the prefilter design; no memo is by design for
simplicity.

Proposed fix: none required now — note for the record. If pattern depth ever
grows, add a per-query `FxHashSet<(PatNodeId, NodeId)>` negative-cache scoped
to one `try_at_node` attempt. Document the depth assumption near
`try_match_at`.

Suggested edge-case test (perf guard, not a unit test): a depth-12 nested
`add(add(...))` near-miss pattern with a bounded wall-clock assertion.

---

## LOW-4 — `Capture` ids are a process-global monotonic atomic; never recycled

- Dimension: SOUNDNESS (robustness) / minor
- Severity: LOW
- Confidence: HIGH
- Location: `crates/strider-pattern/src/capture.rs:14-49`

What & why: `Capture::new()` draws from a process-wide `AtomicU32` that only
ever increments and is never reset. The design relies on global uniqueness so
`Bindings`' append-only `Vec` can key entries unambiguously. This is sound for
all realistic use, but: (1) a long-lived process (e.g. a server embedding
strider-py that builds patterns per request) can exhaust `u32` (~4.3B captures)
and wrap, after which `next_id` silently produces a *colliding* id — two
distinct captures compare equal, and `bind_capture` would treat them as one,
corrupting matches with no error. (2) The id is `pub` (for PyO3 hashing) but
documented as opaque; nothing enforces that.

Why LOW: 4.3B captures is unrealistic for current usage. Worth a note because
the failure mode is a *silent wrong match*, not a panic.

Proposed fix: either widen to `AtomicU64`, or add a `debug_assert` / saturating
check that detects wrap (`fetch_add` returning a value lower than a recorded
high-water mark) and panics rather than silently colliding. `AtomicU64` is the
zero-cost fix.

Suggested edge-case test: not unit-testable cheaply; document the assumption.

---

## Things checked and found SOUND (no finding)

- Commutative matching is fully data-driven by `NodeKind::is_commutative`
  (kind.rs:453) and applied uniformly in walk.rs:199-201 for every arity-2
  node; non-commutative cmps (`Less`/`Sless`/`Sub`/`Div`/shifts) correctly do
  not swap. Coverage in `tests/pattern_matching/commutativity.rs` is dense
  (swap-leak rollback, identical operands → one match, nested
  commutative/non-commutative, child vs root `when_match` redrive semantics).
- `Bindings` rollback is a pure double-`Vec::truncate` (bindings.rs:107-110);
  `matched` footprint shares the journal lifecycle, so failed commutative
  orderings don't leak nodes into the footprint (proven by
  `commutative_swap_does_not_leak_bindings`). Node-vs-Value binding conflict is
  correctly a hard conflict in both directions (test at bindings.rs:561).
- Root derivation is structural (unique sink) and re-validated acyclic via
  `reachable_topo` on every `root()` call (graph.rs:62-67); multi-sink and
  rootless both error with distinct messages (graph_ext.rs:93-103).
- The match/template split is enforced at compile time: `rewrite_rule`'s RHS
  bound `T: TemplatePat` makes a wildcard RHS a compile error; the only runtime
  RHS check is `check_capture_coverage` (rewrite_rule.rs:222-237), which reads
  both node- and value-side LHS captures via `Pattern::bound_captures`.
- asm-fingerprint superset on the live rewrite path: `instantiate` unions
  `proof_nodes` (= `matched_nodes`) into every fresh RHS node at creation, and
  the bare-capture identity-fold case is separately patched
  (rewrite_rule.rs:177-192) so an interior matched node's address is not lost
  when the RHS returns an LHS value verbatim. Verified the footprint is ground
  truth (recorded in `try_match_at` only after full commit, walk.rs:298-304).
- `root_output_vertex_for` / `root_requires_value_output` correctly reject a
  value-demanding root against a zero-output IR node (walk.rs:74-95) — the
  documented `bool_value()`-matching-`Return` bug is closed.
- `Match::get_vn` resolves Call/CallOther clobber outputs via `value_vn` and
  falls back to `InitialVar` — sound and slot-arithmetic-free.
