# Round 13 — 1D: `pattern` crate audit

Branch: `review/ai7` · Scope: `crates/pattern/src/**` (39 .rs read fully), `crates/pattern/tests/**` (5 key tests).

## Verdict

**No findings at or above the 80-confidence threshold.** All ten focus areas verified clean.

## Categories verified clean

✓ **Commutativity tables — single source of truth.** `matcher/commutativity.rs` defines all four helpers. Consistent with CLAUDE.md and every call site (`BinaryOpKind for IntBinaryOp`, `BinaryOpKind for BoolBinaryOp`, `BinaryOpKind for FloatBinaryOp`, `is_commutative_int_cmp_op`, `is_commutative_float_cmp_op`).  No alternate table.

✓ **`find_all_requirements` cross-product correctness.** `prefix_agrees` (matcher/mod.rs:690-700) iterates every `(cap, binding)` pair from every prev Match in the accumulated prefix and checks against `m`'s bindings — pairwise constraints enforced transitively.  Single-pattern edge case handled (`skip(1)` produces zero iterations).

✓ **Empty-set vacuous failure.** `int_const_any_of([])` (wildcards.rs:122), `call().at_any([])` (call.rs:61-66), `stack_store().offset_any([])` (memory.rs:307-311).  All three return `false` for any candidate.

✓ **`.when()` predicate scope.** `GuardPat::try_match` (guards.rs:59-80) snapshots the journal before inner match, runs predicate, rolls back on failure.  Commutative ops with a guard correctly retry swapped operands via outer `try_match_common` snapshot.

✓ **Lift-time canonicalisation aliases (all 6):**
- `sub(a,b)` → `Add(a, Neg(b))` (int.rs:56-59)
- `int_le(a,b)` → `BoolNeg(IntLess(b,a))` (int.rs:107-109)
- `int_sle(a,b)` → `BoolNeg(IntSless(b,a))` (int.rs:116-118)
- `float_sub(a,b)` → `FloatAdd(a, FloatNeg(b))` (float.rs:39-43)
- `float_ne(a,b)` → `BoolNeg(FloatEqual(a,b))` (float.rs:88-91)
- `float_le(a,b)` → `BoolOr(FloatLess(a,b), FloatEqual(a,b))` (float.rs:100-108)

Note: `float_le` converts `BoolBinaryOpPat → Pat` via `.into()`, losing `.ordered()` on the outer Or.  Confirmed harmless: both arms use the same `lhs_p`/`rhs_p` clones so the commutative swap only changes which cmp goes left vs right of the Or; comparison operand order is unchanged.

✓ **`Match` accessors.** `asm_fingerprint` returns slice (empty for unbound).  `stack_offset` reads `NodeKind::StackStore { offset }`.  `stack_phi_offsets` returns `None` for empty side-table (prevents silent zero-length iteration).  `get_wide_bytes` reads `IntConstWide(id)` via `graph.wide_const(id).to_le_bytes()`.

✓ **`GuardPat` semantics.** Two predicate flavours (`Output`/`Bindings`) via `GuardFn` enum.  `kind_spec()` inherits from inner (preserves find_all prefilter).  Bindings rolled back on failure.  Zero-output caveat documented.

✓ **`RewriteCtx` / `RewriteCtxView` (R12-T-A consistency).** Both `graph` and `entry` fields are `pub(crate)`.  Public API: `graph_ref()`, `graph_mut()`, `entry()`, `as_view()`.  `Deref<Target=Graph>` + `DerefMut` on `RewriteCtx`; `Deref<Target=Graph>` on `RewriteCtxView`.  `From<&BuiltFunctionGraph>` and `From<&RewriteCtx>` for view.  Prevents external rebinding-at-distance.

✓ **`Binding` (R12-T-N consistency).** `pub(crate)` fields.  Public `Binding::new(node, output)` ctor + `node()` / `output()` accessors.  `bind_capture` is `pub(crate)`; `bind_capture_for_test` is `pub` for test scaffolds.

✓ **Production panics.** Grep across all `src/` finds `unwrap()` / `expect()` / `panic!` only inside `#[cfg(test)]` (matcher/walk.rs:55,65,83) and doc-test code.  Production paths use `Option` + `?` + `anyhow::Result`.

## Coverage table

| File group | Status |
|---|---|
| `src/lib.rs`, `src/var.rs`, `src/error.rs`, `src/macros.rs` | Fully read |
| `src/matcher/{commutativity,bindings,match_result,mod,walk,walk_through,cast_mask,function_arg_handle}.rs` | Fully read |
| `src/rewrite.rs` | Fully read |
| `src/pat/{mod,guards,node_pat,any,traits}.rs` | Fully read |
| `src/pat/builders/{binary_op,cmp_op,unary_op,call,memory,branch,phi,ret,function_arg,walk_helpers}.rs` | Fully read |
| `src/pat/ctor/{int,float,bool_,casts,consts,wildcards,control,variant_agnostic}.rs` | Fully read |
| `tests/matching/{commutativity,bindings,rewrite}.rs` | Fully read |
| `tests/matching/asm_fingerprint.rs` | Partial |
| Other `tests/matching/*.rs` | Skipped (behaviour covered) |

**Total: 39 source + 5 key test files reviewed; ~2 400 LOC.**
