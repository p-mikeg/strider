---
name: strider-pattern-author
description: Author a strider pattern::Pat (Rust or Python) for a specified asm or IR shape, choosing the right root builder, lift-time-canonicalised aliases, captures, and matcher entry point.
---

# strider-pattern-author

## When to use

User wants to write a new IR pattern that matches a specified shape: a call site, a memory access, a return-value flow, a branch condition, an arithmetic chain, etc. Triggers include "write a pattern that matches...", "match a load at SP+offset", "find every If whose cond is...", "match a `malloc` call where size > 1024". Both Rust (`crates/pattern`) and Python (`crates/strider-py/src/pattern.rs`) authoring fall under this skill.

## When NOT to use

- The user already has a pattern and asks "why doesn't it match?" — route to `strider-debug-pattern`.
- The user wants to substitute matched nodes with fresh ones — that is `pattern::rewrite_rule` and is covered by `strider-opt-pass-author`.
- The user asks only about Python ergonomics on top of an existing Rust pattern — route to `strider-py-binding`.

## Inputs the skill expects

- The asm shape (snippet, mnemonic, or natural-language description).
- Target arch + calling convention (commutativity is arch-independent at the IR level, but the CC's stack-pointer Vn determines `StackStorePat` matching).
- Whether the pattern runs against optimised IR (default — full pipeline including `IfCondInversion` and `StackStoreDetect`) or pre-opt IR (rare).
- Rust vs Python authoring target.

## Procedure

1. Pick the root builder by IR root kind. Use `CallPat` (`crates/pattern/src/call.rs`) for call sites with `.at(addr)`, `.at_any([...])`, `.target(p)`, `.arg(idx, p)`. Use `RetPat` for returns; `LoadPat` / `StorePat` for generic memory; `StackStorePat` / `StackStorePhiPat` only after `StackStoreDetect` has run; `IfPat` for branches (canonical layout only); `PhiPat` / `phi_for(vn)` for `VarPhi` only. Leaf builders: `int_const(n)`, `signed_int_const(n)`, `int_const_any_of([...])`, `var(c)`, `any()`, `predicate(f)`.
2. Use the lift-time-canonicalisation aliases — these match the lowered IR, not the source-level op. `sub(a, b)` lowers to `Add(a, Neg(b))`. `int_le(a, b)` / `int_sle(a, b)` lower to `BoolNeg(IntLess(b, a))` (operand swap is intentional). `float_sub`, `float_ne`, `float_le` follow the table in `crates/strider/CLAUDE.md`. Do NOT write `IntCmpOp::NotEqual` / `LessEqual` / `SlessEqual` / `Borrow`, `IntBinaryOp::Sub`, or `FloatBinaryOp::Sub` — they are not IR primitives.
3. Bind captures via `Capture` (`crates/pattern/src/capture.rs`); a Capture reused across patterns means the same `(NodeId, NodeOutputId)` must bind everywhere it appears. In Python, prefer the str-keyed form (`add("x", "x")`) — strings intern globally via `pattern.rs::intern_capture`.
4. Choose a matcher entry point on `Matcher<'g>` (`crates/pattern/src/matcher.rs`). Single pattern: `find_all(&pat)`. N patterns over one preorder walk, no shared captures: `find_all_multi(&[&p1, &p2, ...])`. N patterns with shared captures (cross-pattern join): `find_all_requirements(&[...])` — applies the cross-product filter so shared captures bind identically across the whole tuple.
5. Add `.when(predicate)` guards on any `Pat` for value-class conditions (e.g. `size > 1024`). Use typed extractors on `Match`: `m.get_uint(c, &graph)`, `m.get_int(c, &graph)`, `m.get_bool(c, &graph)`, `m.get_float_bits(c, &graph)`, `m.get_vn(c, &graph)`. `.when` runs after structural matching.
6. Mind commutativity. `add` / `mul` / `and` / `or` / `xor` / `IntCmpOp::{Equal, Carry, Scarry}` / `FloatCmpOp::Equal` and bool equivalents try both orderings automatically. To force LTR, use the typed dispatcher `int_binary("Add", a, b).ordered()`. Free ctors do not honour `.ordered()`.
7. For stack-store offsets: `StackStorePat::offset(K)` is exact; `offset_any({K1, K2})` is set-membership AND-combined with `.offset(K)`. `StackStorePhiPat::offsets([K1, K2])` requires exact-multiset match against `Graph::stack_phi_offsets`.

## Verification

- Rust: write a unit test in the consumer crate. Run `cargo test --package <consumer> <test_name>`.
- Python: place the test in `crates/strider-py/tests/python/test_pattern_<topic>.py` and run `uv run pytest crates/strider-py/tests/python/test_pattern_<topic>.py -k <name>`.
- Lint: `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- The pattern matches at least one lifted graph from `fixtures/`.
- A negative test asserts it does NOT match a control case.
- `cargo clippy --workspace -- -D warnings` is clean.
- All consumer-crate tests still pass.

## Pitfalls

- `IfCondInversion` must have run before `IfPat` is used — `IfPat` is direct-layout only, so patterns over un-optimised IR silently miss the inverted shape.
- `IntCmpOp::Equal` / `Carry` / `Scarry` are commutative; do not assume slot 0 vs slot 1.
- `int_le(a, b)` swaps operands internally to `BoolNeg(IntLess(b, a))`. When adding extra captures, place them on the original `a` / `b` positions; the alias does the swap for you.
- `PhiPat` matches only `VarPhi` today. For `MemPhi` / `ValuePhi`, build the raw `Pat::Phi(vn)` shape directly.
- `PyPat::ordered()` on a free-ctor result is a no-op. Use `int_binary("Add", a, b).ordered()` for ordered Python matching.
- `StackStorePat` requires `StackStoreDetect` to have run; against bare lifted IR you must match `StorePat` instead.

## Related skills

- `strider-debug-pattern` — when the new pattern returns zero matches.
- `strider-py-binding` — when wrapping a new Rust pattern ctor for Python.
- `strider-opt-pass-author` — when the goal is rewrite, not query.
