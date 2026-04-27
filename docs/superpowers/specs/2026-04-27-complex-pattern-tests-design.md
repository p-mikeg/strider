# Complex Pattern System Tests — Design

## Goal

Add ~30 named system tests (× 14 arches ≈ 400 test invocations) to
`crates/analyzer/tests/complex_patterns.rs` that exercise the
`pattern` crate against complex IR shapes representative of the
queries this project's eventual users will run:

* Struct field access offsets (`a->x`, `a.x`).
* Call arguments under a specific control-flow path.
* Bit-test conditional branches (`if ((flags & MASK) == 0) ...`).
* Combinations of the above (e.g. "if a flag bit is clear, call a
  function passing a field of the same struct").
* Scale: a single large complex function that exercises many of these
  shapes interleaved.

Stack-allocation-size queries are deliberately out of scope for this
batch — the IR doesn't directly preserve C type sizes, the inference
needs more thought, and we want to land the high-value cases first.
Tracked as a follow-up.

## Approach

### Fixtures: `fixtures/cases/complex.c` (new)

9 functions plus `main`, declared `__attribute__((noinline))`.
External callees declared `extern` so the analyzer sees real Call
sites with PLT targets. Built across all 14 arches via the existing
Makefile machinery.

| # | Function | Source sketch | Targets |
|---|----------|---------------|---------|
| 1 | `read_struct_fields(struct S *s)` | returns `s->a + s->b + s->c` | distinct `Load(base + K)` shapes for K ∈ {0, 4, 8} |
| 2 | `write_struct_fields(struct S *s, int v)` | `s->a=v; s->b=v; s->c=v;` | distinct `Store(base + K, v)` shapes |
| 3 | `nested_struct_field(struct Outer *o)` | returns `o->inner.x` | combined offset: a single `Load(base + (K1+K2))` after constant-fold |
| 4 | `bit_test_zero(unsigned mask)` | returns `(mask & ANY_SINGLE_BIT) == 0` | matchable as `IntCmpOp(Equal, And(_, single-bit-const), 0)` *without* hardcoding the bit |
| 5 | `if_bit_clear_call(unsigned mask, int *p)` | `if ((mask & 1) == 0) cb_zero(p);` | If's true-branch contains a `Call(arg=p)` |
| 6 | `call_with_field_arg(struct S *s)` | `extern_invoke(s->handler);` | `Call(arg = Load(base + handler_offset))` |
| 7 | `dispatch_on_flag(struct S *s)` | `if ((s->flags & 4) == 0) invoke(s->handler);` | composition of 4+5+6: bit-test → if → call → field-load |
| 8 | `multi_arg_call_in_branch(int cond, int a, int b, int c)` | `if (cond) f3(a, b, c);` else `f3(c, b, a);` | distinguish two Call sites by their arg ordering |
| 9 | `complex_dispatch(struct S *s, unsigned flags, int n, int *out)` | ~50-line function: 8 branches, 2 loops, 4 calls, struct field access throughout, an accumulator | scale smoke test: graph builds, opt-pipeline runs, has ≥30 IR nodes, find_all on a representative pattern returns ≥1 |

Struct definitions in `complex.c`:
```c
struct S      { int a, b, c; int flags; int *handler; };
struct Inner  { int x, y; };
struct Outer  { int padding; struct Inner inner; };
```

### Test file: `crates/analyzer/tests/complex_patterns.rs` (new)

One test per (fixture × distinct query). Approx tally:

| Fixture | Tests | Assertions |
|---|---|---|
| 1 — `read_struct_fields` | 3 | distinct field loads at offsets {0,4,8} present |
| 2 — `write_struct_fields` | 3 | distinct field stores at offsets {0,4,8} with same value |
| 3 — `nested_struct_field` | 1 | a Load at constant offset matching K1+K2 |
| 4 — `bit_test_zero` | 2 | `IntCmpOp(Equal, And(_, single-bit-const), 0)` shape; the captured mask passes a `.when(\|c\| c.is_power_of_two())` predicate |
| 5 — `if_bit_clear_call` | 2 | If exists; If.true_branch contains `Call.arg(0) = function_arg(1)` |
| 6 — `call_with_field_arg` | 2 | Call arg = Load at constant offset; offset captured via `IntVar` and asserted to be in a sane range (0..256) |
| 7 — `dispatch_on_flag` | 3 | Each leg of the composition matches: bit-test, branch contains Call, Call's arg = Load(field) |
| 8 — `multi_arg_call_in_branch` | 2 | Two distinct Calls; one with args (a,b,c), the other with (c,b,a) — pinned by FunctionArg index |
| 9 — `complex_dispatch` | 3 | scale: builds in <30s, ≥30 IR nodes; ≥1 `if(...) { call(...) }` match; ≥1 field-load match |
| **Total** | **21 tests** | |

(My earlier estimate was 30; the actual decomposition is closer to 21.
Still ample coverage, no padding.)

Each test uses `per_arch_test!` so it expands to 14 invocations
unless `ignore = { ArchX: "BUG-NN: …" }` documents a real
arch-specific limitation.

### Pattern conventions

Every test that involves casts or stack stores constructs the
matcher as:
```rust
let m = Matcher::new(g)
    .ignore_casts_mask(CastMask::EXTEND | CastMask::TRUNCATE)
    .ignore_control_states();
```
to absorb width casts and control-join nodes that arch-specific
codegen leaves in the IR. The `.ignore_casts_mask(...)` selectivity
(landed in commit `3158bb8`) means we don't have to fall back to
the all-casts hammer.

### Capturing constants without hardcoding (the bit-mask use case)

The user's stated goal: write the bit-test test as "match a single-bit
mask, whatever its value". Pattern primitives:

```rust
use pattern::{any_int_const, and, int_cmp, int_const, IntCmpOp, IntVar};

let mask = IntVar::new();
let value = Var::new();
let bit_test_eq_zero =
    int_cmp(IntCmpOp::Equal,
        and(var(value), any_int_const(mask)),
        int_const(0))
    .when_match(move |_fg, _ty, b| {
        let v = b.get_int(mask);
        // single-bit mask: power of two and non-zero.
        v != 0 && v.count_ones() == 1
    });
```

The same idiom captures field offsets as `IntVar` without pinning
specific values — the test just asserts the captured offset is
"reasonable" (e.g. < 256, or in a documented range). For fixtures
where layout is fully determined by the C source (no padding ambiguity),
the test can additionally pin the specific offset.

If `.when_match` over a captured single-bit constant turns out to be
boilerplate that recurs in many tests, extract a helper:
```rust
pub fn single_bit_int_const(v: IntVar) -> Pat {
    any_int_const(v).when_match(move |_, _, b| {
        let n = b.get_int(v);
        n != 0 && n.count_ones() == 1
    })
}
```

### Helper extraction policy (driven by the writing, not pre-planned)

Likely candidates after writing the tests:

* `field_load(base, offset_var)` and `field_store(base, offset_var, val)`.
* `bit_test_eq_zero(value, mask_var, /* require single-bit */ true)`.
* `if_true_contains(call_pat)` and `if_false_contains(call_pat)`.

Only added to `crates/pattern/src/` if extraction produces materially
shorter / more readable tests. Otherwise inline in
`crates/analyzer/tests/common/`. The TDD subagent decides per the
"are tests getting unwieldy?" smell.

## Tests

The deliverables ARE the tests — see the "Test file" section above
for the matrix. Verification gates:

1. `make -C fixtures` builds `complex.elf` for all 14 arches.
2. `cargo test -p analyzer --test complex_patterns --no-fail-fast`
   passes; any per-arch ignores carry an explanatory `BUG-NN: …`
   reason.
3. `cargo test --workspace --no-fail-fast` — no regression in any
   existing test.
4. `cargo clippy --workspace --all-targets` clean.

The scale test (`complex_dispatch`) gets a `Duration` budget assertion
(< 30 seconds analysis time on a single arch) to catch performance
regressions. Other tests don't time-bound — they assert structural
properties only.

## Out of scope

* Stack allocation size queries (sizeof a local from access pattern
  span). Tracked separately; non-trivial because the IR doesn't
  preserve C-type sizes and inference depends on access locality.
* Inter-procedural analysis (across Call boundaries).
* Pattern queries against optimized-out variables.

## Files touched

* `fixtures/cases/complex.c` — new (~150 lines).
* `crates/analyzer/tests/complex_patterns.rs` — new (~400-500 lines
  of test code).
* `crates/analyzer/tests/common/mod.rs` — possibly extend with
  helpers (`function_arg_at(idx)`, `find_call_to(name)`) if writing
  the tests reveals shared boilerplate.
* `crates/pattern/src/...` — possibly add ergonomic helpers
  (`field_load`, `bit_test_eq_zero`, etc.) if extraction earns its
  keep.
* `crates/cfg/tests/common/real_binary.rs::category_for_fn` — extend
  the function-name → fixture-file map to include the new functions.
* `docs/superpowers/plans/2026-04-25-analyzer-known-issues.md` —
  add per-arch BUG-NN entries for any genuinely arch-specific failures
  we hit.
