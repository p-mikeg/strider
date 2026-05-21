# Pattern Crate Generalization Audit — Round 14

## Summary

The `pattern` crate is **well-structured** with **minimal genuine duplication** remaining. Recent macro-driven consolidations (BinaryOpPat generics, CmpOpKind trait) and the pat_builder_finalise! macro have already removed most redundancy. Several minor opportunities exist but offer diminishing returns relative to maintenance cost.

---

## 1. Match Accessor Boilerplate — Minor Duplication

**Files:** `crates/pattern/src/matcher/bindings.rs:175–328`, `crates/pattern/src/matcher/match_result.rs:64–165`

**Shape:** Every typed extractor (`.get_int`, `.get_uint`, `.get_bool`, `.get_float_bits`, `.get_int_binary_op`, …) duplicates the same pattern:
- In `Bindings`: fetch node/output → validate NodeKind → return value
- In `Match`: delegate to `Bindings`, then relay to caller

**Example (bindings.rs:191–197):**
```rust
pub fn get_int(&self, c: Capture, graph: &Graph) -> Option<i128> {
    let out = self.get_output(c)?;
    let NodeKind::IntConst(val) = graph.kind_of_output(out) else {
        return None;
    };
    let ty = graph.output_kind(out).as_value()?;
    ty.get_signed_int(*val)
}
```

**Proposal:** Trait-based extractor builder:
```rust
trait TypedExtractor {
    type Target;
    fn node_kind_match(k: &NodeKind) -> Option<Self::Target>;
}
impl TypedExtractor for IntConst { … }
impl<T: TypedExtractor> Bindings {
    fn get_typed<T>(&self, c: Capture, graph: &Graph) -> Option<T::Target>
}
```

**Difficulty:** Moderate — ~15 methods × ~5 LOC each = 75 LOC reduced to ~20 with generic dispatch.

**Estimated LOC delta:** –50 LOC (1 generic dispatch + 8 TypedExtractor impls vs 16 dedicated methods in `match_result.rs`)

**Priority:** Low — readability of explicit methods outweighs ~50 LOC saving; no performance gain; ergonomics unchanged.

---

## 2. Builder Method Duplication — Mostly Eliminated

**Files:** `crates/pattern/src/pat/builders/{memory,call,ret,phi}.rs`

**Analysis:**

- **CallPat** (264 lines): `.target()`, `.arg()`, `.ret_output()`, `.at()`, `.at_any()` — all accept `impl Into<Pat>`
- **LoadPat** / **StorePat** (399 lines total): `.addr()`, `.data()`, `.mem_in()`, `.bit_width()` — identical field patterns
- **StackStorePat** (277 lines): `.offset()`, `.offset_any()`, `.data()` — sparse indexed input handling
- **PhiPat** / **MemPhiPat** / **ValuePhiPat** (131 lines total): `.input()` with uniform offset-shifting

**Finding:** No macro-unified builder base trait exists, but **the From<Builder> conversion logic is already factored**:
- All use `NodePat::matcher(kind_spec, InputsSpec::Indexed(vec))` — shared construction shape
- `.into_pat()` is **uniform** — macro `pat_builder_finalise!` (strider-py) wraps `.capture()` / `.cap()` / `.when()` identically

**Conclusion:** Duplication is **visual not structural**. Each builder has 1–2 custom methods plus ~20 LOC of `impl From` boilerplate. Unifying via a derive macro would require `#[derive(Builder)]` support or code-gen that adds complexity exceeding benefit.

**Priority:** Very Low — readability + type safety (each builder's methods are specific to its node kind) outweigh ~30 LOC uniformity.

---

## 3. Field-Method Polymorphism — Addressed by `impl Into<Pat>`

**Shape:** `.addr()`, `.arg()`, `.cond()` etc. all accept generic `impl Into<Pat>` — this **already factors** the polymorphism.

**Conclusion:** **No action needed** — this pattern is the right design.

---

## 4. Commutative Matching — Centralized

**Files:** `crates/pattern/src/matcher/commutativity.rs:1–36`

**Shape:** Four free functions (`is_commutative_int_op`, `is_commutative_bool_op`, `is_commutative_float_op`, `is_commutative_int_cmp_op`, `is_commutative_float_cmp_op`) — one per op family.

**Usage:** Consulted by:
- `BinaryOpKind` trait impls (binary_op.rs:37–52)
- `CmpOpKind` trait impls (cmp_op.rs:24–31)

**Conclusion:** **Already optimal** — unified dispatch via trait. No refactoring needed.

---

## 5. Lift-Time Canonicalisation Aliases — Minimal Overhead

**Files:** `crates/pattern/src/pat/ctor/int.rs:47–119` (and float equivalents)

**Constructors:** `sub()`, `int_le()`, `int_sle()` are documented aliases that construct lowered shapes.

**Pattern:** Each builds the canonical composition manually:
```rust
pub fn sub(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    let neg_rhs = unary_pat(IntUnaryOp::Neg, rhs.into());
    BinaryOpPat::new(IntBinaryOp::Add, lhs.into(), neg_rhs).into()
}
```

**Equivalent for float:** `float_sub()`, `float_ne()`, `float_le()` (crates/pattern/src/pat/ctor/float.rs)

**Analysis:** These are **intentionally explicit** — each one documents what lift-time lowering produces. Sharing via a macro would obscure the mapping.

**Conclusion:** **No refactoring needed** — current shape is correct. ~3 LOC × ~5 aliases = 15 LOC; not worth abstraction.

---

## 6. `find_all_multi` vs `find_all_requirements` — Shared Preorder Walk

**Files:** `crates/pattern/src/matcher/mod.rs:296–486`

**Shapes:**
- **`find_all_multi(pats: &[&Pat])`** (296–362): One preorder walk, bucket patterns by discriminant, match all patterns at each node
- **`find_all_requirements(pats: &[&Pat])`** (449–486): Call `find_all_multi()` (line 453), then filter cross-product by shared-capture agreement

**Finding:** `find_all_requirements` **already reuses** `find_all_multi` — no duplication.

**Line count:** `find_all_requirements` is ~37 lines; the actual join logic (prefix_agrees helper) is ~3 lines (not shown in excerpt but visible at call site). **Already factored optimally.**

**Conclusion:** **No action needed**.

---

## 7. `rewrite_rule` vs FlagCmpCanonicalize Bespoke RHS Builder

**Files:** `crates/pattern/src/rewrite.rs:43–160`, `crates/opt/src/flag_cmp_canonicalize/mod.rs:53–200+`

**Shapes:**

**`rewrite_rule(lhs, rhs)`** (rewrite.rs):
- Matches LHS, builds RHS via `Pattern::try_build`, absorbs asm-fingerprints into outermost node only
- Multi-node rules leak orphan interior nodes without fingerprints until `validate_with_options(check_asm_fingerprints: true)`

**`FlagCmpCanonicalize`** (flag_cmp_canonicalize/mod.rs):
- Matches LHS via `Matcher::match_at`, manually constructs RHS with custom `build_rhs: fn(…) -> NodeOutputId` callbacks
- **Extends asm-fingerprint on every new interior node** (lines 180–200+) to pass the fingerprint validator immediately

**Why the split exists:** CLAUDE.md (lines 89–96) documents that `rewrite_rule`'s fingerprint absorption handles only the outermost node. Multi-node RHS rules like `flag_cmp_canonicalize`'s `rule_and_dist` build fresh interior nodes that would have empty fingerprints.

**Proposal:** Extend `rewrite_rule` to absorb fingerprints into all freshly-created nodes (not just the root):
```rust
// In rewrite.rs::rewrite_rule, after BuildOutcome::Out(new_out)
// walk from new_out back to pre_build_node_id, extend each on the path
```

**Difficulty:** Moderate — requires a reverse-walk from RHS output back to LHS captures, carefully marking which nodes are fresh vs cached pre-existing. ~30–40 LOC addition to rewrite.rs.

**Estimated LOC delta:** –100 LOC (retire custom `Rule` struct + per-rule `build_rhs` fn), +40 LOC (fingerprint walk in rewrite_rule)

**Blocker:** Ensure reverse-walk doesn't double-count reuses. Test heavily against `ConstantFold` and other multi-node rules to confirm fingerprints stay correct.

**Priority:** Medium — unifies two rewrite patterns but requires validation against existing rules and adds complexity to the already-subtle fingerprint logic. **Defer unless fingerprint maintenance becomes a pattern-crate maintenance pain point.**

---

## 8. PatKind Exhaustive Coverage — Complete

**Finding:** Spot-check against NodeKind enum (ir/src/node/kind.rs:27+):
- ✓ Entry, InitialMemory, InitialVar → `Any`
- ✓ FunctionArg → FunctionArgPat
- ✓ ControlState, MemPhi, VarPhi, ValuePhi → PhiPat / MemPhiPat / ValuePhiPat
- ✓ If → IfPat
- ✓ Call, CallOther, Return → CallPat, CallOtherPat, RetPat
- ✓ IndirectBranch → `any()` (no builder; rare in optimised IR)
- ✓ Load, Store, StackStore, StackStorePhi → LoadPat, StorePat, StackStorePat, StackStorePhiPat
- ✓ IntConst, IntConstWide, IntBinaryOp, IntUnaryOp, IntCmpOp, Truncate, Extend, … → builders / ctors in pat/ctor/

**Conclusion:** **Coverage is complete.** No missing NodeKind variants.

---

## 9. PyO3 Binding Mirror — Duplicated but Acceptable

**Files:** `crates/strider-py/src/pattern.rs:2122 lines`

**Findings:**

- **String-keyed captures** (pattern.rs:109–139): Global `OnceLock<Mutex<HashMap<String, Capture>>>` intern table — correctly collapsed into single process-wide cache
- **pat_builder_finalise! macro** (pattern.rs:47–76): **Shared** across all PyO3 builder classes — `.capture()`, `.cap()`, `.when()`, `.into_pat()` unified
- **Individual builder wrappers** (PyCallPat, PyLoadPat, etc.): Each has its own `#[pyclass]` + primary `#[pymethods]` block with builder-specific methods, then `pat_builder_finalise!(PyCallPat);` — ~30 LOC per builder (before macro expansion)

**Duplication count:** 11 builders × 30 LOC = ~330 LOC of builder-class boilerplate. Macro reduces by eliminating the 4 common methods, saving ~44 LOC per builder = ~484 LOC total if not for macro. **Macro is earning its keep.**

**Remaining duplication:** Per-builder type wrappers in `PatLike` enum (pattern.rs:239–259) — 15 enum variants. Adds 15 lines to the match arms in `.into_pat()` (262–292). **Not worth eliminating** — the 15 variants are load-bearing for PyO3's type extraction.

**Conclusion:** **No action needed** — the macro already unified the shared shape; per-builder wrappers are required by PyO3's type system.

---

## 10. Capture Interning — Correctly Split

**Finding:** Two separate tables:
- **Rust pattern crate** (var.rs): Process-wide atomic counter (`Capture::new()`)
- **Python binding** (pattern.rs:121–139): Process-wide `OnceLock<Mutex<HashMap>>` for string → Capture mapping

**Are they collapsed correctly?** Yes:
- `pattern.Capture()` in Python calls `PyCapture::new()` → `pattern::Capture::new()` — uses atomic counter
- `add("x", "x")` interns "x" → fetches or inserts into the `HashMap` → returns stable `pattern::Capture`
- The two layers don't interfere; Rust captures are atomic-allocated, Python strings are separately mapped

**Conclusion:** **Correctly designed.** The separation is intentional (Python string interning != Rust atomic allocation).

---

## Minor Cosmetic Opportunities

1. **`walk_helpers::match_unique_output_consumer`** — called by StorePat / CallPat / CallOtherPat. Could be inlined, but adds clarity as a helper. **Keep as-is.**

2. **`NodePat` configuration DSL** — KindSpec / InputsSpec / OutputsSpec / BuildTy are well-designed. No refactoring needed.

3. **Testing scaffolds** — `Match::new_for_test` / `Bindings::bind_capture_for_test` are minimal and appropriate. Keep as-is.

---

## Recommendations

| Rank | Item | Effort | Payoff | Status |
|------|------|--------|--------|--------|
| ✓ | Commutative matching centralized | — | — | Complete |
| ✓ | Binary/cmp ops generalized via traits | — | — | Complete |
| ✓ | Pat_builder_finalise macro (Py) | — | — | Complete |
| ✓ | find_all_multi/requirements factored | — | — | Complete |
| ⊘ | Lift-time aliases (sub, int_le) | Very low | None | Keep explicit |
| ⊘ | Match accessor dispatch macro | Moderate | 50 LOC | Low priority; readability trade-off |
| ⊘ | Builder From<> consolidation | Major | 30 LOC | Diminishing returns; keep per-builder |
| ⚠ | Extend rewrite_rule fingerprints | Moderate | 100 LOC | **Defer:** needs validation; monitor maintenance burden |

---

## Conclusion

The pattern crate is **well-generalized**. Remaining duplication is either:
1. **Already factored** (commutative matching, binary ops via generics, pat_builder_finalise macro)
2. **Intentionally explicit** (lift-time aliases document IR shape; per-builder From<> preserve type safety)
3. **Not worth the added complexity** (typed accessor macros trade readability for 50 LOC savings)

No immediate action required. The `rewrite_rule` fingerprint extension is the only moderate-effort opportunity, but should be deferred until it becomes a maintenance bottleneck.
