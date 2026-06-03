---
name: strider-rewrite-rule-author
description: Use when authoring a new pattern-based IR rewrite rule (typically inside an opt pass like ConstantFold, KnownBits, or FlagCmpCanonicalize) — covers the rewrite_rule(lhs, rhs) shape, single-value-output constraint, int_const_with! / bool_const_with! / float_const_with! closures, when_match/when_match guards, the BoxedRule grouping pattern, apply_rules_in_order driver, automatic asm-fingerprint propagation via the per-interior-node walk, and when to escape to Graph::create_node_attributed.
---

# strider-rewrite-rule-author

Author a new `pattern::rewrite_rule(lhs, rhs)` for an existing
optimization pass, or for a new pass you're scaffolding.

**Use when** the user says "add a rewrite for `(x & C1) & C2 →
x & (C1 & C2)`" / "fold this into ConstantFold" / "rewrite
`Truncate(ZeroExtend(x))` to `x`" / similar lhs→rhs prescriptions.

**Do NOT use** for:
- Matching-only patterns (no replacement) → write directly against
  `strider_pattern` (Rust) or use the `strider-py-pattern`
  skill (Python).

FlagCmpCanonicalize uses the same `rewrite_rule` shape as every other
pass — add the new rule to its `RULES: LazyLock<Vec<BoxedRule>>`
slice the same way you'd add to ConstantFold.

## How to use this skill

1. **Identify the LHS pattern root.** It MUST be a single-value-output
   node — see `Constraints` below.  The rewrite redirects that one
   value-output's uses to the RHS-built value.
2. **Identify the RHS shape.** Either (a) a single capture
   (`var(x)`) — pure identity / value-forwarding, (b) a tree of
   builder calls referencing captured operands and constants
   computed via `int_const_with!` / `bool_const_with!` /
   `float_const_with!` macros, or (c) a multi-node tree (the
   engine propagates fingerprints to every fresh interior node).
3. **Pick the rule group** in the target pass.  ConstantFold groups
   by semantics: identity, const-eval, bool/float, reassoc/mask,
   bitcast/extend.  Add to the closest-fitting group.
4. **Wrap with `boxed_rule(rewrite_rule(lhs, rhs))`** and push into
   the group's `Vec<BoxedRule>`.  The `LazyLock` static and
   `apply_rules_in_order` driver pick up new entries automatically.
5. **Add `when_match` guards** for type / width / bit-pattern
   conditions you can't express structurally.
6. **Add a doc comment** above the rule with the source-level form
   (`(x + C1) + C2 → x + (C1 + C2)`).
7. **Verify** with `cargo test -p strider-opt` and add a focused
   test (often a `TestGraph` mock + a hand-built input shape).

## Constraints

### Single-value-output LHS root

`crates/strider-pattern/src/rewrite/mod.rs:36-43` states the
contract: the LHS root must have exactly one value output.  Rooting
on `Call`, `Load`, `Store`, or any control-flow node (`If`,
`Region`, `Return`, `Phi`) returns an `IrError` from
`node_outputs_exact::<1>()`.  Workaround: match the slot-consumer,
not the producer.

### Asm-fingerprint propagation (automatic)

The `rewrite_rule` engine walks every freshly-allocated interior
node of the RHS subtree and absorbs the rewritten root's
fingerprint via `extend_asm_fingerprint_from`.  Walk bounds:

- `pre_build_node_id = ctx.graph.next_node_id()` is captured before
  the build.
- Any node with `id < pre_build_node_id` is pre-existing (a
  captured LHS operand, a dedup-cache hit, a pre-existing constant).
  These are untouched — they already carry their own fingerprints.
- Any node with `id >= pre_build_node_id` is freshly-allocated
  during the build.  These all inherit the rewritten root's
  fingerprint via union semantics.

**You do not need to thread fingerprints manually.**  Source:
`crates/strider-pattern/src/rewrite/mod.rs:86-136`.

### When the automatic walk doesn't reach a node

Two edge cases require manual attribution:

1. **Side-table writes** that don't go through the build-tree walk
   (`stack_phi_offsets`, `call_clobbered_overrides`, `wide_consts`).
   The walk only follows graph inputs; side-table writes are
   invisible to it.
2. **Direct `Graph::create_node_attributed` calls** from a closure
   body (rare — most rules build via the builders).  Pass the
   contributor fingerprint explicitly to the constructor.

If your rule materialises a node that doesn't appear in any
captured input chain AND doesn't go through the builder DSL, audit
the new node's fingerprint by hand — call
`extend_asm_fingerprint_from(new_node, root)` explicitly so the
contributor-asm chain stays a superset.

## Anatomy of a rewrite rule

```rust
use crate::pattern::{
    BoxedRule, Capture, add, and, any_int_const, boxed_rule,
    rewrite_rule, var,
};
use crate::pattern::macros::int_const_with;

// LHS: (x & C1) & C2
// RHS: x & (C1 & C2)
let (a, c1, c2) = (Capture::new(), Capture::new(), Capture::new());
let rule_and_merge = boxed_rule(rewrite_rule(
    and(and(var(a), any_int_const(c1)), any_int_const(c2)),
    and(var(a), int_const_with!([c1: uint, c2: uint] => c1 & c2)),
));
```

Field-by-field:

- `Capture::new()` mints a fresh capture token.  Three captures here:
  `a` (the deepest LHS operand), `c1` (inner const), `c2` (outer const).
  Each capture is bound once on the LHS and read on the RHS.
- `var(a)` on the LHS matches any value node and binds it to `a`;
  `var(a)` on the RHS retrieves the bound node and reuses it
  (no new node is allocated for `a`).
- `any_int_const(c1)` matches any `IntConst` and captures both the
  node and its constant value.
- `int_const_with!([c1: uint, c2: uint] => c1 & c2)` is a macro that
  unpacks captures by type (`uint`, `int`, `bool`, `float_bits`,
  `node`) and computes the new constant from a closure body.  The
  resulting `IntConst` node is materialised at build time.

## Constant-computation macros

Three macros materialise a typed const from captures:

| Macro | Result type | Capture accessors |
|---|---|---|
| `int_const_with!([c1: uint, c2: uint] => …)` | `IntConst(u128)` | `uint` → u128, `int` → i128 |
| `bool_const_with!([c: bool] => …)` | `BoolConst(bool)` | `bool` → bool |
| `float_const_with!([c: float_bits] => …)` | `FloatConst(u64 bits)` | `float_bits` → u64 |

The closure body returns a value of the macro's result type.  Failure
modes inside the closure (e.g. an invariant violation) should
propagate as `anyhow::Error` — anyhow's blanket `From<E>` wraps
custom error types and the test infrastructure can downcast to
recover them.  To **opt out** of the rewrite without a hard error,
return `Err(crate::pattern::error::skip())`; the engine detects the
sentinel and returns `Ok(false)` (no change, no error).

## `when_match` for structural guards

For conditions the pattern DSL can't express structurally — type
equality, width comparisons, bit-pattern tests — chain `.when_match`
on the LHS pattern.  The closure signature is `(&Graph,
NodeOutputType, &Bindings) -> bool`.

```rust
// Truncate(ZeroExtend(x)) → x — only when x's type matches the
// Truncate's output type (the round-trip is a pure identity in
// that case).
let zext_round_trip = {
    let x = Capture::new();
    let pat = truncate(zero_extend(var(x))).when_match(
        move |ctx, ty, b| {
            b.get(x)
                .and_then(|out| ctx.output_kind(out).as_value())
                .is_some_and(|x_ty| x_ty == ty)
        }
    );
    boxed_rule(rewrite_rule(pat, var(x)))
};
```

The `ctx` parameter is `&Graph`; `ty` is the rule-root's value type
(`NodeOutputType`); `b: &Bindings` is the partial-match binding
table.  Use `b.get(c)` to look up a captured `NodeOutputId`.

## Grouping rules

Within a single pass, related rules are grouped into
`Vec<BoxedRule>` statics built by a builder function and wrapped in
`LazyLock`.  Example from ConstantFold
(`crates/strider-opt/src/constant_fold/rules.rs:120-122`):

```rust
static REASSOC_AND_MASK_RULES: LazyLock<Vec<BoxedRule>> =
    LazyLock::new(build_reassoc_and_mask_rules);
```

The `apply_all_rules` driver runs every group via
`apply_rules_in_order` and OR-s the per-group `changed` flags.

## Tests

- Mock-graph tests using `make_empty_fn` / `make_fn_with_var` /
  `RegisterSet` from `strider-ir-test-utils`.
- Assert: the rule fires (returns `Ok(true)` from `optimize`), and
  the resulting graph passes `validate(&graph, entry)`.
- Assert: the RHS root carries the rewritten root's fingerprint
  (use `graph.asm_fingerprint(new_node)` and check the original
  asm address is present).
- Cross-arch coverage via per-arch tests if the rule is arch-
  sensitive (most arithmetic identities are not).

## Common pitfalls

- Forgetting `boxed_rule(…)` wrapper — `rewrite_rule` returns
  `impl Fn(...) -> Result<bool>`, not `BoxedRule`.
- Re-using `Capture::new()` tokens across rules — they must be
  fresh per-rule (the binding table is per-rule).
- LHS root is a multi-output node — engine returns `IrError`.
- Using `any_int_const(c)` vs `int_const(K)` — the former captures,
  the latter requires exact value.
- Forgetting `c.into()` when the rule shape is heterogeneous (the
  builders take `impl Into<Pat>`).

## Python parity

Rewrite rules are Rust-only.  No corresponding Python authoring
surface.  (Patterns + matching DO mirror to Python; rewrite rules
do not, by design.)
