# `pattern` — IR graph pattern matching

Typed, compositional pattern queries over a [`ir::BuiltFunctionGraph`](../ir).
A pattern is a structural constraint on a subgraph; the [`Matcher`] finds
every site where it holds and returns [`Match`] objects with the captured
values. Patterns cover every `NodeKind` the IR can produce.

## Public surface

- `Pat` — the central pattern value type. Cheaply clonable (`Arc`-wrapped).
  `IntoPat` for ergonomic conversion in builder-method arguments.
- `Capture` — unified capture variable. Globally unique via an atomic counter.
  Binds both a `NodeId` and (for value-producing patterns) a `NodeOutputId`.
- `Matcher<'g>` — wraps `&BuiltFunctionGraph`. `find_all(&pat)`,
  `match_at(node, &pat)`, `find_all_multi(&[…])`,
  `find_all_requirements(&[…])`. Walk-through flags via `MatcherOptions`,
  `ignore_casts`, `ignore_casts_mask(mask)`, `ignore_control_states`.
- `Match` — successful match. `root()`, `node(c)`, `output(c)`, plus typed
  extractors `get_int(c, &graph)`, `get_uint(c, &graph)`, `get_bool(c, &graph)`,
  `get_float_bits(c, &graph)`, `get_vn(c, &graph)`, `stack_offset(c, &graph)`,
  `stack_phi_offsets(c, &graph)`, `asm_fingerprint(c, &graph)`. `Clone` so
  cross-product joins fan-out cleanly.
- `Bindings`, `Binding`, `CastMask`, `MatcherOptions`.
- `BuildCtx` — pattern-construction context (handed to `.when(f)` predicates).
- Builder structs: `IntBinaryOpPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat`,
  `LoadPat`, `StorePat`, `StackStorePat`, `StackStorePhiPat`, `PhiPat`,
  `FunctionArgPat`, `CallPat`, `CallOtherPat`, `RetPat`, `IfPat`. All expose
  `.capture(c)`, `.when(f)`; binary-op builders also `.ordered()`.
- Free constructors covering every NodeKind: `add`, `sub`, `mul`, `div`,
  `int_eq`, `int_lt`, `int_le`, `int_slt`, `int_sle`, `int_carry`,
  `int_scarry`, `int_sborrow`, `bit_not`, `neg`, `popcount`, `lzcount`,
  `shl`, `shr`, `sshr`, `and`, `or`, `xor`, `rem`, `srem`, `sdiv`;
  `bool_and`, `bool_or`, `bool_xor`, `bool_not`; `float_add`, `float_sub`,
  `float_mul`, `float_div`, `float_neg`, `float_abs`, `float_sqrt`,
  `float_ceil`, `float_floor`, `float_round`, `float_eq`, `float_lt`,
  `float_le`, `float_ne`; `cast_to_bool`, `cast_to_int`, `cast_to_float`,
  `truncate`, `extend`, `zero_extend`, `sign_extend`, `int_to_float`,
  `float_to_int`, `float_to_float`, `int_bits_to_float`, `float_bits_to_int`;
  `load`, `store`, `stack_store`, `stack_store_phi`, `phi`, `phi_for(vn)`,
  `function_arg`, `function_arg_reg`, `function_arg_stack`, `function_arg_any`;
  `call`, `call_other`, `if_node`, `ret`; `initial_var`, `initial_var_for(vn)`;
  `int_const`, `signed_int_const`, `int_const_any_of(values)`,
  `bool_const`, `float_const`, `any_int_const`, `any_bool_const`,
  `any_float_const`; `var(c)`, `any()`, `predicate(f)`. Variant-agnostic
  dispatchers `int_binary`, `int_binary_any`, `bool_binary`, `bool_binary_any`,
  `float_binary`, `float_binary_any`, `int_cmp`, `int_cmp_any`, `float_cmp`,
  `float_cmp_any`, `int_unary`, `int_unary_any`, `bool_unary`,
  `bool_unary_any`, `float_unary`, `float_unary_any`.
- Rewrite engine: `rewrite_rule`, `apply_rules_in_order`, `boxed_rule`,
  `BoxedRule`.
- Op enums re-exported from `ir`: `IntBinaryOp`, `IntUnaryOp`, `IntCmpOp`,
  `BoolBinaryOp`, `BoolUnaryOp`, `FloatBinaryOp`, `FloatUnaryOp`,
  `FloatCmpOp`, `ExtendOp`.

## Architecture

`src/pat/` defines the pattern AST (`PatKind` and the per-NodeKind builder
structs) plus the free constructors. `src/matcher/` is the runtime: it walks
candidate roots in pre-order, dispatches on `PatKind`, and tracks bindings in
a `Bindings` map keyed by `Capture`.

The matcher walks both **direct** layout (the pattern matches the node at
hand) and — when `ignore_casts` / `ignore_casts_mask` / `ignore_control_states`
are set — through value-passthrough cast nodes (`Extend`, `Truncate`,
`CastToInt`, `CastToFloat`, `CastToBool`, `IntBitsToFloat`, `FloatBitsToInt`)
or through `ControlState` region-join nodes. Direct match is always tried
first, so strict patterns keep working unchanged.

`find_all_requirements(&[pat1, pat2, …])` runs N patterns in one pre-order
walk and returns the cross-product of their matches, filtered to tuples
whose **shared captures** (Captures appearing in ≥2 patterns) bind to the
same `Binding` (node + value-output). This is the join primitive for
queries like "find K such that `store(<base>+K, 0)` AND `call(at=F).arg(0,
<base>)` both match with the same `<base>`".

`src/rewrite.rs` provides a small `rewrite_rule` engine for graph rewrites
driven by a pattern. The strider crate's `GraphRewriter` is a thin façade on
top of it.

## Key invariants

- **Pattern-to-NodeKind coverage**: every `NodeKind` the IR can produce has a
  builder. Patterns missing this coverage cause `NotBuildable` errors at
  construction time.
- **Capture re-binding**: if the same `Capture` appears in multiple positions
  in a single pattern, all occurrences must bind to the same `Binding`.
- **Commutative ops auto-swap**: `add`, `mul`, `and`, `or`, `xor`,
  `bool_and`, `bool_or`, `bool_xor`, `float_add`, `float_mul`, `int_eq`,
  `int_carry`, `int_scarry`, `float_eq`, `float_ne` automatically retry with
  swapped operands. Use `.ordered()` on the typed builders to opt out.
- **Lift-time canonicalisation**: see the [`ir` README](../ir/README.md).
  Pattern crate aliases (`sub`, `int_le`, `int_sle`, `float_sub`, `float_ne`,
  `float_le`) construct the lowered shapes directly so call-sites still read
  naturally — but the underlying graph contains no `Sub` / `LessEqual` /
  `SlessEqual` / `NotEqual` / `FloatSub` / `FloatNotEqual` / `FloatLessEqual`
  variants.
- `IfPat` matches **direct layout only** — the `opt::IfCondInversion` pass
  guarantees every `If` is in canonical direct layout (cond is not a
  `BoolNeg`) before patterns run.

## Tests

Inline `mod tests` in some submodules (e.g. `matcher/cast_mask/tests.rs`).
End-to-end query tests live in `crates/pattern/tests/` (pattern composability,
captures across joins, walk-through flags, big realistic query examples).

```
cargo test --package pattern
cargo test --package pattern <test_name>
```

## Gotchas

- A capture used in only one pattern is fine; only **shared** captures
  (across two or more patterns in a `find_all_requirements` call) participate
  in the cross-product join.
- `Match::stack_phi_offsets` collapses `Some(&[])` to `None`, so an
  unpopulated side-table doesn't masquerade as a real phi shape.
- `Matcher::ignore_casts` is sticky on the matcher instance — set it once and
  every subsequent `find_all` / `match_at` honours it.
- `find_all_multi` returns all matches per pattern independently (no join);
  use `find_all_requirements` if you want shared-capture filtering.
- Depends on [`ir`](../ir) and `rsleigh`. The dev-dependencies pull
  [`opt`](../opt) and [`target`](../target) for end-to-end tests on lifted
  graphs.
