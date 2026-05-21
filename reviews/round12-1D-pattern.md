# Round 12 Audit 1D — `pattern` crate

Branch: `review/ai6` | Date: 2026-05-11 | Scope: `crates/pattern/src/**/*.rs`, `crates/pattern/tests/**/*.rs`, `Cargo.toml`, `README.md`

## Verdict

**No HIGH-confidence (≥80) findings.** The `pattern` crate is in solid shape on the targeted focus areas. One LOW-confidence (≈70) clean-up observation is recorded at the end.

## Focus-area verification

### 1. Commutativity tables — single source of truth (`matcher/commutativity.rs`)

`crates/pattern/src/matcher/commutativity.rs:5-36` defines four `is_commutative_*` predicates. Every binary/cmp ctor that needs the answer routes through them:

- Typed builders: `pat/builders/binary_op.rs:19-21` imports `is_commutative_{bool,float,int}_op`; the `BinaryOpKind` impls at lines 37–53 delegate to them.
- Typed cmp builder: `pat/builders/cmp_op.rs:12` imports `is_commutative_{int,float}_cmp_op`; the `CmpOpKind` impls at lines 24–32 delegate.
- Variant-agnostic `*_any` ctors: `pat/ctor/variant_agnostic.rs:17-20` imports all four commutativity helpers; macro instantiations at lines 170–216 pass them as `$commutative` into the `InputsSpec::fixed_maybe_commutative` runtime decider.

No alternative table or local commutativity check exists in the pattern crate. Single source of truth confirmed.

### 2. `PhiPat::input` / `MemPhiPat::input` / `ValuePhiPat::input` — `idx + 1` offset

`crates/pattern/src/pat/builders/phi.rs:44`, `:84`, `:119` all push `(idx + 1, p)` into the indexed-inputs list.

Verified against IR phi layout: `crates/ir/src/node_signature.rs:315-323`:

```
NodeKind::MemPhi  => sig!(inputs: [PHI]; in_tail: MEM, outputs: [MEM]),
NodeKind::VarPhi(_) | NodeKind::ValuePhi => sig!(inputs: [PHI]; in_tail: IN_PHI, outputs: [ANY_VAL]),
```

Input 0 is the `PhiToken` edge from the owning `ControlState`; per-predecessor value/memory inputs live at indices 1, 2, …. The `idx + 1` shift correctly routes `pat.input(0, …)` to the first predecessor slot. All three builders agree (`PhiPat`, `MemPhiPat`, `ValuePhiPat`).

### 3. `*_any` set-membership empty-set semantics

- `int_const_any_of([])` — `pat/ctor/wildcards.rs:98-125`. `values_unsigned` is empty, `.iter().any(...)` returns `false` for every node. Vacuously fails. ✓
- `CallPat::at_any([])` — `pat/builders/call.rs:61-66`. Routes through `int_const_any_of(addrs)`. Empty set → vacuous false. ✓
- `StackStorePat::offset_any([])` — `pat/builders/memory.rs:264-270` stores `Some(vec![])`. The `kind` closure at line 307–310 (`!set.contains(actual_offset)` → always true for empty set) returns `false`. Vacuously fails. ✓

All three implementations agree on "empty set ⇒ match nothing" and the docstrings (`pat/ctor/wildcards.rs:94`, `pat/builders/call.rs:58`, `pat/builders/memory.rs:261`) document the contract.

### 4. Lift-time canonicalisation aliases

Cross-checked the pattern crate's lowered-shape aliases against the lifter's exact emission shape:

| Alias (file:line)                                  | Pattern emits                                                       | Lifter emits (file:line)                                                | Match |
|----------------------------------------------------|---------------------------------------------------------------------|-------------------------------------------------------------------------|-------|
| `sub` (`pat/ctor/int.rs:56-60`)                    | `Add(lhs, Neg(rhs))`                                                | `IntAdd(lhs, IntUnaryOp::Neg(rhs))` (`pcode-lift/.../arithmetic.rs:151-180`) | ✓ |
| `int_le` (`pat/ctor/int.rs:107-110`)               | `BoolNeg(IntLess(rhs, lhs))`                                        | `BoolNeg(IntLess(rhs, lhs))` (`arithmetic.rs:103-117`)                  | ✓ |
| `int_sle` (`pat/ctor/int.rs:116-119`)              | `BoolNeg(IntSless(rhs, lhs))`                                       | `BoolNeg(IntSless(rhs, lhs))` (`arithmetic.rs:124-138`)                 | ✓ |
| `float_sub` (`pat/ctor/float.rs:39-43`)            | `FloatAdd(lhs, FloatUnaryOp::Neg(rhs))`                             | `FloatAdd(lhs, FloatUnaryOp::Neg(rhs))` (`float.rs:99-107`)             | ✓ |
| `float_ne` (`pat/ctor/float.rs:88-91`)             | `BoolNeg(FloatEqual(lhs, rhs))`                                     | `BoolNeg(FloatEqual(lhs, rhs))` (`float.rs:115-122`)                    | ✓ |
| `float_le` (`pat/ctor/float.rs:100-109`)           | `Or(FloatLess(lhs, rhs), FloatEqual(lhs, rhs))` (NaN-aware)         | `Or(FloatLess(lhs, rhs), FloatEqual(lhs, rhs))` (`float.rs:132-140`)    | ✓ |

`float_is_nan` — there is no such ctor (`grep` confirms zero occurrences in `crates/pattern/`). The lifter at `pcode-lift/.../float.rs:78-90` lowers `FLOAT_NAN(x)` to `BoolNeg(FloatEqual(x, x))`, but no pattern alias is exposed. This is not a bug — callers compose the lowered shape manually. Worth a future spec/README note but not a finding.

### 5. `Match` accessors — control-flow safety (`Match::*` in `matcher/match_result.rs`)

For control-flow captures (output=`None`):

- `output(c)` → `bindings.get_output(c)` → `.and_then(|b| b.output)` returns `None`. ✓
- `get_uint/get_int/get_bool/get_float_bits` start with `self.get_output(c)?;` — `?` short-circuits to `None`. ✓
- `get_*_op` use `self.get_node(c)?` then pattern-match the kind; control-flow nodes (`Call`, `If`, `Return`, `CallOther`) fail the pattern and return `None`. ✓
- `stack_offset` / `stack_phi_offsets` / `get_wide_bytes` use `bindings.get_node(c)?` then pattern-match `StackStore` / `StackStorePhi` / `IntConstWide`; control-flow nodes don't match → `None`. ✓
- `get_vn` handles `Call` and `CallOther` outputs by clobber-slot index; returns `None` for control outputs (slot 0/1) by the `if slot < clobber_start` guard at `match_result.rs:229-234`. For non-Call/CallOther control bindings the function falls through to `match graph.graph.node_kind(binding.node)` and returns `None` for any kind other than `InitialVar`. ✓
- `asm_fingerprint` returns `graph.asm_fingerprint(node)` for a bound capture; empty slice for unbound (`match_result.rs:315-318`). No panic for control captures. ✓

No accessor panics on control-flow captures.

### 6. `RewriteCtx` / `RewriteCtxView` — accessor consistency

`crates/pattern/src/rewrite.rs:161-231` (`RewriteCtx`) and `:239-285` (`RewriteCtxView`):

- Both expose `pub graph` / `pub entry` fields (W4 tradeoff documented at `rewrite.rs:155-160` for `RewriteCtx`, `:240-252` for `RewriteCtxView`).
- Both provide `graph_ref()`, `entry()`, `preorder()`, `preorder_kind(P)`.
- `RewriteCtx::graph_mut()` returns `&mut Graph` (only `RewriteCtx` is mutable).
- Both `Deref<Target=Graph>` (`rewrite.rs:299-304, 311-316`); `RewriteCtx` additionally `DerefMut`.
- Lifetime mismatch: `RewriteCtx::graph_ref()` returns `&Graph` (borrow lifetime), `RewriteCtxView::graph_ref()` returns `&'g Graph` (the view's stored lifetime). This is *intentional and correct* — `RewriteCtxView` is `Copy` and holds the longer borrow.
- `RewriteCtxView` is `From<&BuiltFunctionGraph>` and `From<&RewriteCtx>`; both go through `as_view()` or struct-literal construction. Consistent.

No inconsistency; the dual API is symmetric where it can be and asymmetric where the type's mutability demands.

### 7. `find_all_requirements` cross-product join

`crates/pattern/src/matcher/mod.rs:449-486`. Logic:

1. Empty `pats` → empty result.
2. Any pattern with zero matches → empty result (no cross-product term to anchor on).
3. Seed `acc` with single-element tuples from pattern 0's hits.
4. For each subsequent pattern's hits, cross-product against `acc` and keep tuples where `prefix_agrees(prefix, m)`.
5. Early-break when `acc` becomes empty.

`prefix_agrees` at `mod.rs:690-701`: iterates every binding in every `prev` Match in the prefix; for every capture also bound in `m`, requires equality. Induction holds: when a new Match is admitted to the prefix, it agreed with each earlier prev — so any two prevs share consistent bindings (transitively, via the seed). Captures unique to a single pattern impose no constraint, which is the documented semantics.

The implementation is correct and matches the README description.

### 8. `Bindings::bind_capture` visibility (W14)

`crates/pattern/src/matcher/bindings.rs:88` declares `pub(crate) fn bind_capture`. Production callers inside `pattern`:

- `pat/any.rs:34, 82, 110` (VarPat / CapturePat).
- `pat/ctor/variant_agnostic.rs:75, 119, 157` (the three `*_any` macros).

`bind_capture_for_test` is `pub` and tests use it (`tests/get_vn_with_*.rs`, `tests/matching/bindings.rs`). A repo-wide grep (`grep -rn bind_capture /mnt/c/Users/mikeg/Documents/strider/ --include="*.rs"` excluding `/pattern/`) returns zero matches — no production caller outside `pattern` bypasses the matcher path. ✓

### 9. Production panics

`grep -rn 'panic!\|unreachable!\|\.unwrap()\|\.expect(' crates/pattern/src/` yields three hits, all in `matcher/walk.rs:55, 65, 83`, all inside `#[cfg(test)] mod tests` (lines 28–88). No production-path panics or unwraps in the crate's source. ✓

## Low-confidence findings (≥51, <80) — informational only, not flagged

- **`ethnum` declared but unused (confidence 70)** — `crates/pattern/Cargo.toml:12` declares `ethnum.workspace = true` but a recursive grep across `crates/pattern/` shows no `ethnum::` or `use ethnum` reference. Likely left over from a wide-constant refactor (wide constants now live in `ir::wide_const`). Removing the line would shave a dependency edge but doesn't affect correctness.

- **`float_is_nan` not exposed as an alias (confidence 55)** — The lifter lowers `FLOAT_NAN(x)` to `BoolNeg(FloatEqual(x, x))` (`pcode-lift/.../float.rs:78-90`), and the section on "Lift-time canonicalisation aliases" in the CLAUDE.md prompt mentions `float_is_nan`, but no ctor exists. Callers compose the shape manually. Worth a future ergonomic alias but not a defect.

## Summary

- Commutativity tables centralised: ✓
- `+1` phi-input offset correct against IR layout: ✓
- `*_any` empty-set vacuous-fail semantics consistent: ✓
- Lift-alias shapes match lifter output exactly: ✓ (6/6)
- `Match` accessors do not panic on control-flow captures: ✓
- `RewriteCtx`/`RewriteCtxView` accessor API consistent: ✓
- `find_all_requirements` cross-product join correct: ✓
- `Bindings::bind_capture` properly scoped to `pub(crate)`: ✓
- Zero production-path panics: ✓
- `cargo build -p pattern` clean.

The pattern crate has no HIGH-confidence findings this round.
