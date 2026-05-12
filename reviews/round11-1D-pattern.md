# Round 11 — 1D: pattern audit

## Coverage

Read every `*.rs` file under `crates/pattern/src/**` (mod.rs, var.rs, error.rs, macros.rs, rewrite.rs, matcher/{mod, bindings, cast_mask, cast_mask/tests, commutativity, function_arg_handle, match_result, walk, walk_through}.rs, pat/{mod, node_pat, traits, any, guards}.rs, pat/builders/{mod, binary_op, branch, call, cmp_op, function_arg, memory, phi, ret, unary_op, walk_helpers}.rs, pat/ctor/{mod, bool_, casts, consts, control, float, int, variant_agnostic, wildcards}.rs), plus `Cargo.toml`, `README.md`, the example file, and selected test files (`commutativity.rs`, `asm_fingerprint.rs`, `cast_mask_walk.rs`, `control_flow.rs`).

Cross-checked the lift-time canonicalisation aliases against `crates/pcode-lift/src/value/{arithmetic,float,integer,mod}.rs` and node-kind signatures against `crates/ir/src/node_signature.rs` and `crates/ir/src/ops/op_kinds.rs`.

## Findings

### `PhiPat::input` / `MemPhiPat::input` / `ValuePhiPat::input` doc says "predecessor slot", actually addresses raw input slot 0 = phi_token

- **Severity:** MED
- **Where:** `crates/pattern/src/pat/builders/phi.rs:38-43, 73-77, 102-107`
- **What's wrong:** All three builders' `.input(idx, p)` methods are documented as "Constrain the value arriving from predecessor slot `idx`" but the implementation pushes `(idx, p)` directly into `InputsSpec::Indexed`, which addresses the raw `node_inputs(node)` slot. Per IR signature (`crates/ir/src/node_signature.rs:316-322`), `MemPhi` / `VarPhi` / `ValuePhi` inputs are `[phi_token, ...per-predecessor values]` — the phi_token sits at raw input index 0, so `phi().input(0, p)` constrains the phi_token (a `PhiToken`-typed edge from the owning `ControlState`), not the first predecessor value. Predecessor 0's value lives at raw input index 1.
- **Verified against:** `node_signature.rs:316` (`MemPhi => sig!(inputs: [PHI]; in_tail: MEM, …)`) and `:321-323` (`VarPhi(_) | ValuePhi => sig!(inputs: [PHI]; in_tail: IN_PHI, …)`). The `[PHI]` head sits at raw index 0; `in_tail` adds variadic slots starting at index 1.
- **Fix:** Either (1) change the implementation: `self.inputs.push((idx + 1, p.into()));` so `idx=0` addresses predecessor 0 (matches the documented "user-facing predecessor index" semantics); or (2) change the docs to "Constrain raw input slot `idx` (slot 0 is the phi_token from the owning ControlState; per-predecessor values start at slot 1)". Option (1) is the better fix.
- **Regression test:** Build `VarPhi(some_vn)` whose inputs are `[phi_token, IntConst(0), IntConst(1)]` (one phi_token + two predecessors). Assert `phi_for(some_vn).input(0, int_const(0))` matches predecessor 0 (currently fails — would match the phi_token at raw index 0 instead, which is a `PhiToken` edge that no value-pattern can match against, so today this just always fails to match).

### `try_once` swap arithmetic relies on caller-side invariant; subtle footgun if violated

- **Severity:** LOW
- **Where:** `crates/pattern/src/pat/node_pat.rs:451-452`
- **What's wrong:** `let inp_idx = if swap { 1 - pat_idx } else { pat_idx };`. If a future caller invokes `try_once(... swap=true)` with a non-arity-2 `Fixed` pattern (`pats.len() ≥ 3`), `1 - pat_idx` underflows `usize` for `pat_idx ≥ 2` — debug-mode panic, release-mode wraparound. Today the only caller (`try_match_common` at `:418-426`) guards with `pats.len() == 2 && commutative(...)` before passing `swap=true`, so the invariant holds; the inline comment at `:450-452` even acknowledges this. Still, the next-line `inputs.get(inp_idx)?` was apparently meant to defend against the wrap — but on debug builds the underflow panics before we ever touch `.get`.
- **Verified against:** Caller pattern at node_pat.rs:418-426 (`if let InputsSpec::Fixed { pats, commutative } = ... && pats.len() == 2 ...`).
- **Fix:** Make it explicit: `let inp_idx = if swap { pat_idx ^ 1 } else { pat_idx };` (XOR-with-1 is only meaningful for `pat_idx ∈ {0,1}` but won't panic on underflow for any input). Or `assert_eq!(pats.len(), 2)` at the top of `try_once` when `swap=true`.

### `int_const_any_of` and `signed_int_const` discriminant sentinel uses `IntConst(0u128)` payload — DOC NIT

- **Severity:** LOW
- **Where:** `crates/pattern/src/pat/ctor/wildcards.rs:50, 105, 161` (and similar in other ctors using `KindSpec::variant`)
- **What's wrong:** The pattern uses `KindSpec::variant(&NodeKind::IntConst(0u128))` to build a discriminant-only kind spec. Comparing against the actual node uses the discriminant only, ignoring payload. **This is correct** but the inline comment `// Discriminant-only prefilter; the width-aware equality is done in post_match...` is opaque to readers — the payload-zero is structurally meaningful only to `KindSpec::variant`'s internal discriminant extraction, not as a "default value to compare against".
- **Verified against:** `pat/node_pat.rs:80-82` (`KindSpec::variant` discards the exemplar's payload, retaining only `std::mem::discriminant(exemplar)`).
- **Fix:** No code change needed; consider clarifying comments next to each `KindSpec::variant(&NodeKind::IntConst(0u128))` site to say "(payload `0u128` is a structurally-irrelevant sentinel — `KindSpec::variant` extracts only the discriminant)".

### `RewriteCtxView::graph` and `RewriteCtxView::entry` are publicly mutable fields; struct-literal mutation breaks the `Copy` contract's intent

- **Severity:** LOW
- **Where:** `crates/pattern/src/rewrite.rs:215-229`
- **What's wrong:** `RewriteCtxView<'g>` is `#[derive(Clone, Copy)]` with `pub graph: &'g Graph` and `pub entry: NodeId`. The doc-comment prefixes acknowledge this with `**Caution:**` warnings, but the fields stay `pub` — so any external caller can write `view.graph = &other_graph;` (where the lifetimes line up) and silently re-anchor a view shared via `Copy`. The `Copy` semantics make the rebinding deceptive: `let v2 = view; view.graph = &other; …v2.graph` still reads the old graph (since `v2` is a copy), but a `&mut RewriteCtxView<'_>` referent does change. This is a pre-existing design tradeoff explicitly called out in the doc-comment ("read-only by convention"), not a bug — but it's worth flagging.
- **Verified against:** `rewrite.rs:215-229` (struct definition + caution comments).
- **Fix:** Make the fields `pub(crate)` and expose readers (`pub fn graph(&self) -> &'g Graph` and `pub fn entry(&self) -> NodeId`). The existing `Deref<Target=Graph>` impl already covers most ergonomics; the `entry` accessor would be the only newly-needed one. **However** the comment notes the `pub graph` field exists "for the existing `fg.graph` access pattern across opt passes" — opt presently reads `view.graph` directly. Migrating those call sites would unblock the encapsulation. Optional finding.

### `Bindings::iter` exposes `(Capture, Binding)` even though the public surface treats Bindings as read-only — but `Bindings::bind_capture` is also public

- **Severity:** LOW
- **Where:** `crates/pattern/src/matcher/bindings.rs:81-89, 127-129`
- **What's wrong:** The doc-comment at `:50-54` says: "External callers see `Bindings` as read-only: construction is via `Default::default()`, mutation goes through `bind_capture`, and the `mark`/`restore` journal API is `pub(crate)`". But `bind_capture` is `pub` (not `pub(crate)`), and any external caller can construct an empty `Bindings::default()`, then `bind_capture(c, Binding::new(some_node_id, None))` to forge an arbitrary binding. Combined with `Match::new_for_test` (test-only, but `pub`), this lets external code synthesise `Match` objects whose bindings don't reflect a real graph state. Today this is exploited by `Match::new_for_test` for legitimate test-building purposes — there's no obvious abuse vector — but the docstring contradicts the visibility.
- **Verified against:** `bindings.rs:81` (`pub fn bind_capture`), `match_result.rs:31-34` (`pub fn new_for_test`).
- **Fix:** Either (a) demote `bind_capture` to `pub(crate)` and expose a `pub` test-only constructor on `Bindings` that mirrors `Match::new_for_test`, or (b) update the docstring at `:50-54` to acknowledge the `pub` exposure of `bind_capture`. Option (a) is cleaner — the synthesis path already routes through `Match::new_for_test`, so no use-case is harmed by tightening `bind_capture`'s visibility.

## Confirmed-correct (positive findings)

- **No production panics.**  Every `unwrap()` / `expect()` / `panic!()` / `unreachable!()` lives inside a doctest (`crates/pattern/src/lib.rs:28-39, 124-129; matcher/mod.rs:170-175`) or behind `#[cfg(test)]` (`matcher/walk.rs:55, 65, 83`).  Production paths return `Result<T>` (via `crate::error::missing_binding` / `not_buildable`) or `Option<T>` for missing bindings.
- **Commutativity tables agree with constructors.**  `is_commutative_*` predicates in `commutativity.rs:5-36` are the **single source of truth** consulted by concrete-op ctors (via `BinaryOpKind::is_commutative` / `CmpOpKind::is_commutative` impls routed to `InputsSpec::fixed_commutative` vs `fixed_ordered`) and by variant-agnostic `*_any` ctors.  The build-RHS path uses the same predicates.  Carry/Scarry are commutative; Sborrow correctly excluded.
- **Lift-time canonicalisation aliases match lifter shapes.**  `pattern::sub`, `int_le`, `int_sle`, `float_sub`, `float_ne`, `float_le` all produce the exact operand order the lifter emits in `pcode-lift::value::{arithmetic,float}.rs`.
- **Empty `*_any` set-membership ctors vacuously fail.**  `int_const_any_of([])` → false; `at_any([])` → delegates to int_const_any_of → false; `offset_any([])` → empty Vec's contains is false → returns false.
- **`Match::asm_fingerprint` / `stack_offset` / `stack_phi_offsets` / typed extractors return `None` (or `&[]`) on unbound captures and shape mismatches.**  Every accessor starts with `self.bindings.get_node(c)?` or `get_output(c)?`.  Unbound captures, control-flow bindings, shape mismatches all return None without panicking.  `get_vn`'s clobber-slot arithmetic is guarded against underflow.
- **`PhiPat` / `MemPhiPat` / `ValuePhiPat` discriminate by NodeKind.**  Three distinct discriminants → no cross-kind false positives.
- **`find_all_requirements` cross-product join logic correct.**  Empty pats → empty Vec; any pattern with zero matches → empty Vec; cross-product is incremental with `prefix_agrees` shared-capture check.
- **`IntoPat::when` zero-output-base limitation documented.**  `GuardPat::try_match` requires value-typed output; default `try_match_node` bails for zero-output kinds (Return, dangling-output CallOther) with documented workarounds.
- **`Capture::from_id` cross-process safety documented.**  PyO3 binding-layer use-case acknowledged.
- **`IfPat` direct-layout-only matching with `IfCondInversion` upstream.**  `IfPattern::try_match_at` checks `NodeKind::If` and applies cond/true_branch/false_branch in canonical direct layout.

## Coverage summary

Read every file under `crates/pattern/src/**` and the crate's `Cargo.toml` and `README.md`.  Cross-checked the lift-time canonicalisation aliases against `pcode-lift::value::{arithmetic,float}.rs`.  Verified node-kind signatures against `crates/ir/src/node_signature.rs`.

No HIGH-severity findings.  1 MED (PhiPat::input doc/impl mismatch).  4 LOW (try_once swap arith, KindSpec::variant doc nit, RewriteCtxView pub fields, Bindings::bind_capture visibility).
