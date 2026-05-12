# Round 9 / 1D — `pattern` crate audit

**Branch:** `review/ai3`. Independent audit; round-7 / round-8 reports not consulted.

## Findings

### MED — `Tb::neg` test helper dispatches to `BitNot` instead of `Neg`

- **Confidence:** 90.
- **Severity:** MED (latent: zero current callers, but trap for any future test author).
- **Where:** `crates/pattern/tests/matching/support/graph.rs:182-184`.
- **What's wrong:**
  ```rust
  pub fn neg(&mut self, v: NodeOutputId) -> NodeOutputId {
      self.int_un(v, IntUnaryOp::BitNot)   // builds ~v, not -v
  }
  ```
  `BitNot` is bitwise complement (`~x`); `Neg` is two's-complement negation (`-x`). Method name is unambiguous. `grep` finds zero call sites today, but a future test author writing `t.neg(b)` to construct `Add(a, Neg(b))` for `sub`-canonicalisation tests would silently build `Add(a, BitNot(b))`.
- **Fix:** `self.int_un(v, IntUnaryOp::Neg)` — or rename to `bit_not` and add a separate `neg` wrapping `Neg`.

### LOW — `lib.rs` doc misleadingly groups `float_ne` with first-class commutative patterns

- **Confidence:** 85.
- **Severity:** LOW (doc only, no behaviour bug).
- **Where:** `crates/pattern/src/lib.rs:83-84`.
- **What's wrong:** Doc lists `float_ne` alongside `float_eq` as comparisons that "retry with swapped operands", implying engine-level commutativity. `float_ne` is not in any commutativity table; it's a composed alias `BoolNeg(FloatEqual(a, b))` that inherits ordering invariance from the inner `FloatEqual`. Confuses Python users who may try to recover `FloatCmpOp::NotEqual` via `get_float_cmp_op` (it's not a primitive).
- **Fix:** Reword: "`float_eq` retries with swapped operands. `float_ne` achieves the same effect through its inner commutative `FloatEqual`; it is a composed lowered alias, not a primitive."

### LOW — `GuardPat` zero-output silent failure not documented at the public `.when()` constructors

- **Confidence:** 82.
- **Severity:** LOW (round-8 left this open; deferred again).
- **Where:** `crates/pattern/src/pat/mod.rs:148-157` (`IntoPat::when`) and `:112-123` (`Pat::when_match`).
- **What's wrong:** The limitation is documented on the `GuardPat` struct (`guards.rs:33-45`) — `ret().when(predicate)` silently never matches because `Return`/dangling-`CallOther` have no value outputs and `try_match_node`'s default loop never fires. But the public call-site constructors don't reference this. Developers see no warning and observe zero matches without an error signal.
- **Fix:** Add a `# Caveat` note to both `IntoPat::when` and `Pat::when_match` pointing to the limitation: "For zero-output bases (`Return`, dangling `CallOther`), this predicate is never evaluated — the pattern silently returns no matches. Wrap a value-producing sub-pattern instead, e.g. `ret().preceded_by(call().when(p))`."

## Areas verified correct

- **`pat_builder_finalise!` macro** at `crates/strider-py/src/pattern.rs:47-76` — emits exactly one `#[pymethods] impl $BuilderTy { … }` block with the four methods. All 15 invocations at `:2068-2082` cover the required builders. `multiple-pymethods` PyO3 feature declared in `Cargo.toml`.
- **`RewriteCtx<'g>`** newtype in `rewrite.rs:109-132`. Sound separation of pure-rewrite path from CC-bearing fields. `rewrite_rule` creates `Matcher::for_graph` in a tight scope, drops the borrow, then mutates `ctx.graph` — no borrow conflict.
- **Commutativity tables** in `crates/pattern/src/matcher/commutativity.rs`: all 5 functions verified.

  | Function | Commutative | Correctly excluded |
  |----------|-------------|---------------------|
  | `is_commutative_int_op` | Add, Mul, And, Or, Xor | Sub, Div, Sdiv, Rem, Srem, Shl, Shr, SShr |
  | `is_commutative_bool_op` | And, Or, Xor | — |
  | `is_commutative_float_op` | Add, Mul | Div |
  | `is_commutative_int_cmp_op` | Equal, Carry, Scarry | Less, Sless, Sborrow |
  | `is_commutative_float_cmp_op` | Equal | Less |

- **`Match::get_vn` per-CallOther override length** at `match_result.rs:218-221` — uses `map_or(default, |ov| ov.len())` correctly. Three tests in `tests/get_vn_with_callother_clobber.rs` cover function-default, value-bearing form, and per-CallOther override.
- **Lift-time canonicalisation aliases** all 6 verified:

  | Alias | Shape constructed | Matches lifter |
  |-------|-------------------|----------------|
  | `sub(a, b)` | `Add(a, Neg(b))` | ✓ |
  | `int_le(a, b)` | `BoolNeg(Less(b, a))` | ✓ |
  | `int_sle(a, b)` | `BoolNeg(Sless(b, a))` | ✓ |
  | `float_sub(a, b)` | `FloatAdd(a, FloatNeg(b))` | ✓ |
  | `float_ne(a, b)` | `BoolNeg(FloatEqual(a, b))` | ✓ |
  | `float_le(a, b)` | `Or(FloatLess(a,b), FloatEqual(a,b))` | ✓ NaN-aware |

- **Empty-set vacuous failure**: `int_const_any_of([])`, `at_any([])`, `offset_any([])` all correctly return `false` vacuously.
- **`.when()` predicate signature**: `PredicateFn` takes `&ir::Graph` not `&BuiltFunctionGraph` — matches CLAUDE.md.
- **`find_all_requirements`** cross-product + early-exit + `prefix_agrees` direction all correct.

## Coverage

All 42 source files under `crates/pattern/src/` read in full. From `crates/pattern/tests/`: `get_vn_with_callother_clobber.rs`, `get_vn_with_call_override.rs`, `matching/arithmetic.rs`, `matching/commutativity.rs`, `matching/support/graph.rs` read in full. Remaining 29 test files not read (source-level analysis sufficient).

## Summary

- **0 HIGH**
- **1 MED** — `Tb::neg` helper dispatches to `BitNot` (latent test-helper bug)
- **2 LOW** — doc drift on `float_ne` commutativity, undocumented zero-output limitation in `.when()` constructors

All round-9 special-focus items verified correct.
