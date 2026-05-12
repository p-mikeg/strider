# Round 10 — `pattern` crate

**Scope:** All 74 `.rs` files under `crates/pattern/src/` plus tests.

---

## CRITICAL

### C-1: `GuardPat` missing `try_match_node` override — `ret().capture(c).when(f)` silently never matches

- **Severity:** HIGH
- **Where:** `crates/pattern/src/pat/guards.rs:52-81`
- **What's wrong:** `GuardPat` implements `Pattern` with only `try_match` and `kind_spec`. The default `try_match_node` (in `pat/traits.rs:77-86`) iterates the node's outputs and calls `try_match` on each. For zero-output nodes (`Return`, dangling-output `CallOther`), the default loop produces no iterations and returns `false`. The wave-31 docs note this for the `when` zero-value-output case. But `GuardPat` also has no `try_match_node` override that delegates to the inner pattern's `try_match_node` — unlike `CapturePat` (lines 90-116) which DOES override `try_match_node` and handles the zero-output case explicitly. This means `ret().capture(c).when(f)` (which wraps `CapturePat` in `GuardPat`) uses `GuardPat::try_match_node` (default, fails for zero-output), not `CapturePat::try_match_node`. The call chain is: `find_all` → `match_node_id` → `GuardPat::try_match_node` (default) → iterate `Return`'s zero outputs → empty loop → `false`. The inner `CapturePat::try_match_node` is never reached. **Functional bug:** `ret().capture(c).when(f)` will never match any `Return` node when used with `find_all` or `match_at`.
- **Verified against:** `pat/any.rs:90-116` (CapturePat has override), `pat/guards.rs:52-81` (GuardPat has none), `pat/traits.rs:77-86` (default try_match_node iterates outputs).
- **Fix:** Add a `try_match_node` override to `GuardPat` that delegates to `inner.as_dyn().try_match_node(ctx, node, b)` for zero-output nodes (where the guard cannot fire and the inner-only match suffices), and iterates outputs otherwise.
- **Regression test:**
  ```rust
  let c = Capture::new();
  let pat: Pat = ret().capture(c).when(|_, _, _| true);
  let hits = Matcher::new(&graph).find_all(&pat);
  assert!(!hits.is_empty()); // currently fails
  ```

### C-2: `*_any` variant-agnostic captures bind `output: None` — `get_uint`/`get_int` silently return None

- **Severity:** HIGH (Confidence: 85)
- **Where:** `crates/pattern/src/pat/ctor/variant_agnostic.rs:67-73` (all three arm variants)
- **What's wrong:** `impl_variant_any!`'s post_match closure does `b.bind_capture(c, Binding::new(node, None))` — records only the `NodeId`. This is intentional for op-variant extraction via `get_int_binary_op(c, &graph)`. However, a caller writing `int_binary_any(c, lhs, rhs)` who expects `Match::output(c)` or `Match::get_uint(c, &graph)` to return the value silently gets `None`. `Match::get_uint` calls `get_output` first (`bindings.rs:142-143`); a `None` output means `get_uint` returns `None` even when the matched node IS a value-producing op. There is no warning or error.
- **Verified against:** `matcher/bindings.rs:142-143`, `matcher/match_result.rs:64-65`.
- **Fix:** Either (a) document prominently that `*_any` captures are node-only, or (b) populate `Some(value_output_id)` by picking the value output from `ctx.graph.node_outputs(node)` in the post_match closure.

---

## IMPORTANT

### I-1: `RewriteCtxView` exposes `pub graph` field — external rebinding to a different graph allowed

- **Severity:** MED
- **Where:** `crates/pattern/src/rewrite.rs:216-219`
- **What's wrong:** `RewriteCtxView` is described as "Read-only `(&Graph, NodeId)` view." It has `pub graph: &'g Graph` and `Deref<Target=Graph>`. No `DerefMut` (correct). However, the `pub` field allows callers to do `view.graph = &other_graph` to rebind the field, redirecting the view to a different graph at a distance. Same lifetime, different graph would silently confuse downstream code reading `view.graph`.
- **Fix:** Change `pub graph` to `pub(crate) graph` and provide only the existing `Deref` accessor (or a dedicated `graph()` method).

### I-2: `find_all_multi` non-deterministic ordering across patterns

- **Severity:** MED (Confidence: 80)
- **Where:** `crates/pattern/src/matcher/mod.rs:320-361`
- **What's wrong:** Comment claims "No post-sort: `kind_index` was populated by iterating `preorder_cached()` once, so each bucket's `Vec<NodeId>` is already in preorder." Per-pattern results within `results[i]` are deterministic. However the outer loop iterates `by_discriminant` (a `FxHashMap`) — not insertion-ordered. Across pattern slots, processing order is non-deterministic. No correctness impact (results are independent per-pattern slot) but makes debugging harder.
- **Fix:** Document that inter-pattern processing order is unspecified; only intra-pattern order is preorder.

### I-3: `Piece` / `Extract` / `Insert` pattern constructors not exported despite CLAUDE.md listing them

- **Severity:** MED
- **Where:** `crates/pattern/src/pat/ctor/casts.rs` (missing), `crates/pattern/src/lib.rs:206-211` (no exports)
- **What's wrong:** CLAUDE.md states the `pattern` crate covers "`Piece`, `Extract`, `Insert`" under cast ops. Neither `casts.rs` nor `lib.rs` exports `piece` / `extract` / `insert` constructors. The `cast_mask.rs` exhaustive match also has no arm for them — confirming these `NodeKind` variants either don't exist in the current IR or are absent from the cast-mask logic.
- **Fix:** Verify whether `NodeKind::Piece` / `Extract` / `Insert` exist in the IR. If so, add ctors and export them. If not, remove from CLAUDE.md's pattern-crate description.

### I-4: `try_walk_through_control_state` doc comment is a stale paste — describes deleted cast walk-through

- **Severity:** LOW
- **Where:** `crates/pattern/src/matcher/walk_through.rs:29-43`
- **What's wrong:** Block comment contains two concatenated doc comments — the first describes cast walk-through, then mid-paragraph switches to ControlState walk-through. The function only implements ControlState; the cast walk-through helper was inlined into `match_output_with_walk_through`.
- **Fix:** Remove the cast paragraph; keep only the ControlState description.

### I-5: `prefix_agrees` in `find_all_requirements` is O(N×M) inner loop

- **Severity:** LOW (performance only)
- **Where:** `crates/pattern/src/matcher/mod.rs:690-701`
- **What's wrong:** `prefix_agrees(prefix, m)` iterates every `(Capture, Binding)` in every `prev` match in `prefix`, probing `m.bindings.get_binding(cap)` (O(K) linear scan over `m.bindings.entries`). For typical queries negligible; for large join sets degrades.
- **Fix:** Pre-merge prefix bindings into a `HashMap<Capture, Binding>` before the inner join loop.

### I-6: `int_const` post_match scan is over-general for single-output IntConst

- **Severity:** LOW
- **Where:** `crates/pattern/src/pat/ctor/wildcards.rs:53-66`
- **What's wrong:** `find_map(|out| ctx.graph.output_kind(out).as_value())` scans outputs of an `IntConst` node which always has exactly one output. Always O(1) but conceptually wasteful.
- **Fix:** `let out = ctx.graph.node_outputs(node).into_iter().next()?;`.

### I-7: `Capture::from_id` cross-process unsoundness

- **Severity:** LOW
- **Where:** `crates/pattern/src/var.rs:68-70`
- **What's wrong:** Documented as "id must come from a prior `id()` call inside the same process." Cannot be enforced by the type. Multi-tenant test environments or PyO3 deserialisation across runs could collide. Within a single process this is safe (atomic counter monotonically increasing).
- **Fix:** No code change needed; documentation accurate. Consider a debug assertion that `id < NEXT.load(Ordering::Relaxed)`.

---

## LOW

- **L-1:** `bool_not` maps to `BoolUnaryOp::Neg` — naming inconsistency (boolean NOT vs IR's "Neg") but functionally correct.
- **L-2:** `IfPat` true_branch=output 0, false_branch=output 1 — convention agreement with IR not directly verified, but production tests exercise it.
- **L-3:** `CapturePat` correctly delegates to `match_output` (no walk-through at root output) — sound design, just non-obvious.

---

## Coverage

| File | Status |
|---|---|
| `src/lib.rs` | fully |
| `src/error.rs` | fully |
| `src/macros.rs` | fully |
| `src/var.rs` | fully |
| `src/rewrite.rs` | fully |
| `src/matcher/mod.rs` | fully |
| `src/matcher/bindings.rs` | fully |
| `src/matcher/cast_mask.rs` | fully |
| `src/matcher/cast_mask/tests.rs` | partially |
| `src/matcher/commutativity.rs` | fully |
| `src/matcher/function_arg_handle.rs` | partially |
| `src/matcher/match_result.rs` | fully |
| `src/matcher/walk.rs` | fully |
| `src/matcher/walk_through.rs` | fully |
| `src/pat/mod.rs` | fully |
| `src/pat/any.rs` | fully |
| `src/pat/guards.rs` | fully |
| `src/pat/node_pat.rs` | fully |
| `src/pat/traits.rs` | fully |
| `src/pat/builders/binary_op.rs` | fully |
| `src/pat/builders/branch.rs` | fully |
| `src/pat/builders/call.rs` | fully |
| `src/pat/builders/cmp_op.rs` | fully |
| `src/pat/builders/function_arg.rs` | fully |
| `src/pat/builders/memory.rs` | fully |
| `src/pat/builders/mod.rs` | partially |
| `src/pat/builders/phi.rs` | fully |
| `src/pat/builders/ret.rs` | fully |
| `src/pat/builders/unary_op.rs` | fully |
| `src/pat/builders/walk_helpers.rs` | fully |
| `src/pat/ctor/mod.rs` | partially |
| `src/pat/ctor/bool_.rs` | fully |
| `src/pat/ctor/casts.rs` | fully |
| `src/pat/ctor/consts.rs` | fully |
| `src/pat/ctor/control.rs` | fully |
| `src/pat/ctor/float.rs` | fully |
| `src/pat/ctor/int.rs` | fully |
| `src/pat/ctor/variant_agnostic.rs` | fully |
| `src/pat/ctor/wildcards.rs` | fully |
| `tests/**` | partially (selected key files) |
