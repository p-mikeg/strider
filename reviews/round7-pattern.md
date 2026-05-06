# Round 7 — pattern Crate Audit

Independent review (code-only, no prior reviews trusted) of `crates/pattern/src/**` and `crates/strider-py/src/pattern.rs`.

---

## CRITICAL Issues

### 1. `if_node()` docstring falsely advertises symmetric matching — HIGH (conf 95)
- **Where:** `crates/pattern/src/pat/ctor/control.rs:120-127`; mirrored at `crates/strider-py/src/pattern.rs:1564-1568`
- **Evidence (from code, not from docstring):** `IfPattern::try_match_at` calls `self.try_layout(ctx, node, b)` exactly once with no retry. `builders/branch.rs` correctly documents "Direct-layout-only" because `IfCondInversion` canonicalises `If(BoolNeg(C)){A}{B} → If(C){B}{A}`.
- **Bug:** `control.rs` ctor doc says: "When `.cond(C)` is set, the matcher also tries the compiler-inverted layout: input `Not(C)` with branches swapped." That is false. Users querying *unoptimized* IR (or test fixtures that bypass `IfCondInversion`) will silently miss matches.
- **Fix:** Remove the "Symmetric matching" paragraph from both Rust ctor and Python `if_` docstring. Replace with a note that canonical direct layout is enforced by `IfCondInversion` in optimized IR; for unoptimised IR, write two patterns or run `IfCondInversion` first.

### 2. `PhiPat` / `phi()` / `phi_for()` only match `VarPhi` — HIGH (conf 85)
- **Where:** `crates/pattern/src/pat/builders/phi.rs:40`; `crates/pattern/src/pat/ctor/control.rs:40-48`
- **Evidence:** `PhiPat::from` builds `KindSpec::variant(&NodeKind::VarPhi(exemplar_vn()))`. No constructor exists for `MemPhi` or `ValuePhi`.
- **Bug:** `phi()` ctor doc claims "Matches any phi node" — but only `VarPhi` matches. Queries that expect to match memory-token phis (for memory-flow tracking at join points) or value phis introduced by `StackLoadForward` silently find nothing.
- **Fix:** Either rename `phi()` → `var_phi()` and add `mem_phi()` + `value_phi()` ctors, or make `phi()` a union over all three kinds with a kind-discriminating method.

### 3. `float_cmp_any` docstring mentions nonexistent `NotEqual` — MED (conf 90)
- **Where:** `crates/pattern/src/pat/ctor/variant_agnostic.rs:197`
- **Evidence:** Docstring claims "Commutative comparisons (`Equal`, `NotEqual`) try both operand orderings automatically." `FloatCmpOp::NotEqual` does not exist in the IR (lowered at lift time). `is_commutative_float_cmp_op` returns true only for `Equal`.
- **Bug:** Documentation lie that points users at a non-existent variant.
- **Fix:** Replace with "`Equal` is commutative; `NotEqual` and `LessEqual` are not IR primitives — use `float_ne` / `float_le` aliases for those."

### 4. `PyPat::ordered()` silent no-op trap — MED (conf 80)
- **Where:** `crates/strider-py/src/pattern.rs:427-438`
- **Evidence:** Method body is `self.clone()`. Free functions like `pattern.add(x, y)` return finalized `PyPat` (commutativity baked in). Calling `.ordered()` on the result does nothing.
- **Bug:** Trap method. Python user writing `add("a","b").ordered()` expects ordered matching but still gets commutative.
- **Fix:** Remove `PyPat.ordered()` and route ordering through typed `int_binary("Add", a, b).ordered()`. Alternatively, raise `PatternError` from `PyPat.ordered()` with a message pointing to the typed builder path.

### 5. Stale `IfCase` reference in fingerprint exemption comment — LOW (conf 80)
- **Where:** `crates/ir/src/graph/mod.rs:96`
- **Evidence:** Comment lists `IfCase` as exempt; `NodeKind` does not contain `IfCase`. (Adjacent to the pattern audit because it surfaces during pattern-graph cross-checks.)
- **Fix:** Remove `IfCase` from the exemption comment.

---

## Verified-Correct (no issues found)

### A. Pat / Capture correctness
- `Capture` uses `AtomicU32` — theoretical wraparound at 4B; not a real bug.
- Multi-occurrence binding agreement enforced by `bind_capture` equality check.

### B. Lift-time canonicalisation aliases (HIGH-IMPORTANCE — verified)
All 6 lowered aliases match what the IR emits at lift time:
- `sub(a,b)` → `Add(a, Neg(b))` ✓
- `int_le(a,b)` → `BoolNeg(IntLess(b,a))` (operand swap) ✓ — verified at `pattern/src/pat/ctor/int.rs:107-109`
- `int_sle(a,b)` → `BoolNeg(IntSless(b,a))` ✓
- `float_sub(a,b)` → `FloatAdd(a, FloatNeg(b))` ✓
- `float_ne(a,b)` → `BoolNeg(FloatEqual(a,b))` ✓
- `float_le(a,b)` → `Or(FloatLess(a,b), FloatEqual(a,b))` (NaN-aware) ✓

### C. Commutativity
- `is_commutative_*` helpers correct.
- `try_match_common` snapshot-restore correctly isolates commutative retry.
- `ordered()` flips `InputsSpec::fixed_commutative` → `fixed_ordered` correctly in Rust builders.

### D. Match API
- `Match: Clone` confirmed (used by find_all_requirements).
- `stack_phi_offsets` correctly collapses `Some(&[])` → `None`.
- `asm_fingerprint(c)` returns `&[]` for unbound captures.
- `get_uint` / `get_int` correctly return `None` for Bool-typed nodes (`is_integer()` excludes Bool).

### E. Matcher / walk / find_all
- `find_all` uses kind-indexed prefilter.
- `find_all_multi` shares same index.
- `find_all_requirements` cross-product filter uses `prefix_agrees`; ignores captures local to one pattern; correctly enforces shared-capture agreement.
- No unbounded-blowup guard exists; documented O(N1×…×Nm) worst case.

### F. Set-membership ctors
- `int_const_any_of([])` vacuously fails (empty `values_unsigned`).
- `StackStorePat.offset_any` AND-combined with `.offset(K)` works via the `VariantWith` closure at `pattern/src/pat/builders/memory.rs:307-311`.

### G. PatKind NodeKind coverage
- All NodeKind variants reachable via pattern ctors except `MemPhi`, `ValuePhi` (issue #2 above), `IndirectBranch`, `SegmentOp`, `CPoolRef`, `New`, `ControlState`, `Entry`, `InitialMemory` — structural/opaque and intentionally not matchable by end users.

### H. Production panics
- None in non-test pattern crate code. All fallible paths return `Result`.

### I. Python parity (gaps, not bugs)
- Python surface covers all Rust free ctors.
- **Missing:** op-variant extractors on Python `Match` (acknowledged in module comment at `strider-py/src/pattern.rs:21` — TODO).
- **Missing:** `float_cmp` typed dispatcher in Python (only `float_binary` exists; per-op helpers `float_eq`, `float_lt` are present).
- `if_node` correctly renamed `if_` in Python (keyword collision).
- `and_` / `or_` / `not_` keyword-collision renames consistent.

---

## Critical Issues Summary

| Sev | # | Issue |
|-----|---|-------|
| HIGH | 1 | `if_node()` symmetric-match docstring lies |
| HIGH | 2 | `phi()` only matches VarPhi (no MemPhi/ValuePhi ctors) |
| MED | 3 | `float_cmp_any` mentions nonexistent NotEqual |
| MED | 4 | `PyPat.ordered()` silent no-op trap |
| LOW | 5 | Stale `IfCase` in graph fingerprint comment |
