---
name: strider-py-pattern
description: Use when a Python user wants to write a strider pattern (strider.pattern.*) to match an IR shape on a lifted binary — produces correct Python pattern code given a natural-language description. Knows the full Python builder surface, lift-time canonicalisations, and commutative ops so the generated pattern matches the canonical IR shape rather than the source-level shape.
---

# strider-py-pattern

Generate idiomatic `strider.pattern` Python code from a natural-language
description of an IR shape.

**Use when** the user asks "give me a pattern that matches X" / "how do
I find all loads where the address is sp+const" / "write a pattern for
the indexed-array-load shape" and similar.

**Do NOT use** for Rust-side patterns (use `strider-pattern-author`
instead) or for graph rewriting (use `strider-rewrite-rule-multinode-audit`).

## How to use this skill

1. Read the user's spec carefully.  Identify (a) the **root node kind**
   (load, store, call, add, etc.), (b) the **operand shapes** the
   user wants to constrain, (c) what should be **captured** for
   post-processing.
2. **Apply lift-time canonicalisations** before writing the pattern.
   The lifter rewrites `a - b` to `Add(a, Neg(b))` etc., so a pattern
   for the source-level shape will NEVER match unless you rewrite to
   the canonical form first.  See `### Lift-time canonicalisations`
   below.
3. Choose the right builder for each level.  Use the
   `### Available builders` cheat sheet.
4. Decide between **string-shorthand captures** ("x" auto-interns to a
   `Capture`) and explicit `Capture()` objects (when you need the
   capture passed to multiple places or to `var(c)`).
5. Mention **commutativity** if it affects the spec — commutative
   ops automatically try both operand orderings, so do NOT manually
   write both forms.  Use `.ordered()` on a typed builder if the
   user explicitly wants a non-commutative match.
6. Emit the code as a single Python snippet, plus a 1-2 line
   explanation of why the canonical form differs from the source.

## Cheat sheet

### Captures

```python
from strider.pattern import Capture, var, any_int_const

# Explicit Capture object — use when the same capture appears in
# multiple slots or you need to bind it to a specific name in code.
c = Capture()
pat = add(var(c), int_const(8))           # capture is `c`, look it up later

# String shorthand — each unique string interns to a Capture for this
# pattern.  Re-use the same string for a back-reference (must-be-same).
pat = xor("v", "v")                       # zero-idiom: must be same value

# Reserved wildcard strings raise PatternError if used as a capture
# key.  Use `p.any_()` for an unbound wildcard or `p.var(Capture())`
# explicitly when you need an unnamed capture:
pat = add(p.any_(), "x")                  # first slot wildcard, second captured as "x"

# Reading captures back from a Match `h`:
h.uint("x")        # → int  (None if not bound or not an IntConst)
h.bool_("c")       # → bool
h.float_bits("k")  # → u64 bits of a FloatConst
h.vn("v")          # → Vn of the captured InitialVar / tagged Phi / FunctionArg
h.has("v")         # → True/False whether the capture is bound
h.root             # → NodeId (u32) of the top-level match root (getter)
```

Note: there is no per-capture `node_id` accessor on `Match` — only the
top-level `root` getter exposes a raw NodeId.  Per-capture lookup is
typed (`uint`, `int`, `bool_`, `float_bits`, `vn`, `stack_offset`,
`stack_phi_offsets`, `asm_fingerprint`, plus the op-variant
accessors).

### Available builders

Source of truth: `crates/strider-py/src/pattern.rs` (the `register()`
function near the bottom enumerates every registered name).

| Builder | Rust IR shape produced | Python signature | Commutative? |
|---|---|---|---|
| `p.any_()` | wildcard | `any_() -> Pat` | n/a |
| `p.var(c)` | wildcard + capture | `var(c: Capture) -> Pat` | n/a |
| `p.int_const(K)` | `IntConst(K)` (strict width) | `int_const(value: int) -> Pat` | n/a |
| `p.signed_int_const(K)` | `IntConst` re-interpreted as signed | `signed_int_const(value: int) -> Pat` | n/a |
| `p.int_const_any_of([…])` | `IntConst ∈ set` | `int_const_any_of(values: list[int]) -> Pat` | n/a |
| `p.bool_const(b)` | `BoolConst(b)` | `bool_const(value: bool) -> Pat` | n/a |
| `p.float_const(bits)` | `FloatConst(bits)` | `float_const(bits: int) -> Pat` | n/a |
| `p.any_int_const(c)` | any `IntConst`, capture | `any_int_const(c: Capture) -> Pat` | n/a |
| `p.any_bool_const(c)` | any `BoolConst`, capture | `any_bool_const(c) -> Pat` | n/a |
| `p.any_float_const(c)` | any `FloatConst`, capture | `any_float_const(c) -> Pat` | n/a |
| `p.initial_var()` | `InitialVar(_)` | `initial_var() -> Pat` | n/a |
| `p.initial_var_for(vn)` | `InitialVar(vn)` | `initial_var_for(vn: Vn) -> Pat` | n/a |
| `p.function_arg(i)` | `FunctionArg{index=i}` | `function_arg(i: int) -> FunctionArgPat` | n/a |
| `p.function_arg_any()` | any `FunctionArg` | `function_arg_any() -> FunctionArgPat` | n/a |
| `p.function_arg_reg(vn)` | `FunctionArg` for register | `function_arg_reg(vn: Vn) -> FunctionArgPat` | n/a |
| `p.function_arg_stack(s, off)` | `FunctionArg` for stack arg | `function_arg_stack(space: VnSpace, offset: int) -> FunctionArgPat` | n/a |
| `p.phi()` / `p.phi_for(vn)` | `Phi(Some(vn))` / any | builder w/ `.for_vn(vn)` `.input(idx, p)` | n/a |
| `p.mem_phi()` | `MemPhi` | `mem_phi() -> MemPhiPat` | n/a |
| `p.value_phi()` | `Phi(None)` | `value_phi() -> ValuePhiPat` | n/a |
| `p.predicate(f)` | match-any + Python guard | `predicate(f) -> Pat` | n/a |
| `p.add(a, b)` | `IntBinaryOp(Add)` | `add(l, r) -> Pat` | **yes** |
| `p.sub(a, b)` | `Add(a, Neg(b))` lowered | `sub(l, r) -> Pat` | no (lowered) |
| `p.mul(a, b)` | `IntBinaryOp(Mul)` | `mul(l, r) -> Pat` | **yes** |
| `p.div(a,b)` / `p.sdiv(a,b)` | unsigned / signed div | binary | no |
| `p.rem(a,b)` / `p.srem(a,b)` | unsigned / signed rem | binary | no |
| `p.shl(a,b)` / `p.shr(a,b)` / `p.sshr(a,b)` | shifts | binary | no |
| `p.and_(a, b)` | `IntBinaryOp(And)` (`and` is a Python kw) | binary | **yes** |
| `p.or_(a, b)` | `IntBinaryOp(Or)` | binary | **yes** |
| `p.xor(a, b)` | `IntBinaryOp(Xor)` | binary | **yes** |
| `p.int_eq(a, b)` | `IntCmpOp(Equal)` | binary | **yes** (Equal/Carry/Scarry) |
| `p.int_lt(a, b)` / `p.int_slt` | unsigned / signed less-than | binary | no |
| `p.int_le(a, b)` | `BoolNeg(IntLess(b, a))` lowered | binary | no (lowered) |
| `p.int_sle(a, b)` | `BoolNeg(IntSless(b, a))` lowered | binary | no (lowered) |
| `p.int_carry` / `p.int_scarry` / `p.int_sborrow` | carry / overflow / borrow flags | binary | Carry & Scarry only |
| `p.int_cmp("Op", a, b)` | dispatch on op name | `int_cmp(op, l, r) -> Pat` | per op |
| `p.neg(x)` | `IntUnaryOp(Neg)` | unary | n/a |
| `p.bit_not(x)` / `p.not_(x)` | `IntUnaryOp(BitNot)` | unary | n/a |
| `p.bool_and` / `p.bool_or` / `p.bool_xor` / `p.bool_not` | bool ops | bin/unary | **bool_and/or/xor commutative** |
| `p.float_add` / `p.float_sub` / `p.float_mul` / `p.float_div` | float arith | binary | Add/Mul **commutative** |
| `p.float_neg` / `p.float_abs` / `p.float_sqrt` / `p.float_ceil` / `p.float_floor` / `p.float_round` | float unary | unary | n/a |
| `p.float_is_nan(x)` | `BoolNeg(FloatEqual(x, x))` lowered | unary | n/a (lowered) |
| `p.float_eq` / `p.float_ne` / `p.float_lt` / `p.float_le` | float cmp | binary | Equal **commutative**; le is lowered to `Or(Less, Equal)` |
| `p.int_to_float` / `p.float_to_int` / `p.float_to_float` | conversions | unary | n/a |
| `p.int_bits_to_float` / `p.float_bits_to_int` | bit-cast | unary | n/a |
| `p.cast_to_int` / `p.cast_to_bool` / `p.cast_to_float` | cast nodes | unary | n/a |
| `p.truncate(x)` | `Truncate` | unary | n/a |
| `p.popcount(x)` / `p.lzcount(x)` | popcount / lzcnt | unary | n/a |
| `p.zero_extend(x)` / `p.sign_extend(x)` / `p.extend("zero"\|"sign", x)` | width-extend | unary | n/a |
| `p.load(addr=…)` | `Load(_)` typed builder w/ `.addr(p) .space(s) .mem_in(p) .bit_width(n)` | builder | n/a |
| `p.store(addr=…, data=…)` | `Store(_)` typed builder w/ `.addr .data .space .mem_in .next_mem .bit_width` | builder | n/a |
| `p.stack_store(offset=…, data=…)` | `StackStore{offset}` builder w/ `.offset .offset_any([…]) .data .space` | builder | n/a |
| `p.stack_store_phi(data=…)` | `StackStorePhi` builder w/ `.data .space .offsets([…])` | builder | n/a |
| `p.call(at=…)` | `Call` builder w/ `.at(addr) .at_any([…]) .target(p) .arg(idx, p) .ret_output(idx, p)` | builder | n/a |
| `p.call_other()` | `CallOther` builder w/ `.user_op_id(v) .name(s) .arg(i, p) .ret(i, p) .ctrl .mem .ctrl_out .mem_out .next_ctrl .next_mem` | builder | n/a |
| `p.ret()` | `Return` builder w/ `.preceded_by(p) .ret_val(idx, p)` | builder | n/a |
| `p.if_(cond=…)` | `If` builder w/ `.cond(p) .true_branch(p) .false_branch(p)` | builder | matcher tries compiler-inverted layout too |
| `p.int_binary("Op", l, r)` | dispatch w/ chainable `.ordered()` | typed builder | per op |
| `p.bool_binary("Op", l, r)` | dispatch w/ `.ordered()` | typed builder | per op |
| `p.float_binary("Op", l, r)` | dispatch w/ `.ordered()` | typed builder | per op |
| `p.int_bin_any(c, l, r)` / `p.int_un_any(c, x)` / `p.int_cmp_any(c, l, r)` / `p.bool_bin_any` / `p.bool_un_any` / `p.float_bin_any` / `p.float_un_any` / `p.float_cmp_any` | variant-agnostic: capture the op variant | takes a Capture for the op | per concrete variant |

**Universal builder methods** (every typed builder has these):
`.capture(c)` (bind a `Capture`), `.cap("name")` (bind via auto-interned
name), `.when(f)` (Python predicate guard, signature
`f(match: PartialMatch) -> bool`), `.into_pat()` (finalise to `Pat`).
Typed builders also accept being passed directly anywhere a `Pat` is
expected — `into_pat()` is implicit at use-site via the `PatLike`
trait.

### Lift-time canonicalisations

The lifter rewrites these shapes at lift time, so a pattern for the
source-level form will NEVER match the IR.  When the user describes a
shape using the source form, translate to the canonical form before
writing the pattern.

| Source-level shape | Canonical IR shape | Pattern helper that already produces it |
|---|---|---|
| `IntSub(a, b)` | `Add(a, Neg(b))` | `p.sub(a, b)` (produces lowered form) |
| `IntLessEqual(a, b)` | `BoolNeg(IntLess(b, a))` (args swapped) | `p.int_le(a, b)` |
| `IntSlessEqual(a, b)` | `BoolNeg(IntSless(b, a))` | `p.int_sle(a, b)` |
| `IntNotEqual(a, b)` | `BoolNeg(IntEqual(a, b))` | `p.bool_not(p.int_cmp("Equal", a, b))` |
| `FloatSub(a, b)` | `FloatAdd(a, FloatNeg(b))` | `p.float_sub(a, b)` |
| `FloatNotEqual(a, b)` | `BoolNeg(FloatEqual(a, b))` | `p.float_ne(a, b)` |
| `FloatLessEqual(a, b)` | `Or(FloatLess(a, b), FloatEqual(a, b))` | `p.float_le(a, b)` |
| `FLOAT_NAN(x)` | `BoolNeg(FloatEqual(x, x))` | `p.float_is_nan(x)` |
| `If(BoolNeg(C)){A}{B}` | `If(C){B}{A}` (after `IfCondInversion` opt pass) | `p.if_(cond=C)` — matcher tries both layouts |

**Optimizer-induced shape changes** (after `Strider` runs the stable
pipeline):

- `Add(a, Neg(IntConst(K)))` constant-folds to `Add(a, IntConst(-K))`.
  So `sub(x, int_const(8))` may not match if `ConstantFold` ran;
  prefer `add(x, signed_int_const(-8))` against optimised graphs.
- `Load(IntConst(addr))` may fold to a value via `LoadReadOnly` if a
  ROM was passed.
- SP-relative `Store` becomes `StackStore{offset}` after
  `StackStoreDetect`; match the latter with `p.stack_store(offset=K)`
  for offset-keyed stores.

### Commutative ops (matcher tries both operand orderings)

Single source of truth: `NodeKind::is_commutative` in
`crates/strider-ir/src/node/kind.rs:560`.

- `IntBinaryOp`: `Add`, `Mul`, `And`, `Or`, `Xor`
- `BoolBinaryOp`: `And`, `Or`, `Xor`
- `FloatBinaryOp`: `Add`, `Mul`
- `IntCmpOp`: `Equal`, `Carry`, `Scarry`
- `FloatCmpOp`: `Equal`

For these, do NOT manually write both orderings.  To DISABLE
commutativity on a specific match, use the typed family dispatcher
with `.ordered()`:

```python
# Match `Add(IntConst(5), x)` but NOT `Add(x, IntConst(5))`:
p.int_binary("Add", p.int_const(5), p.var(c)).ordered()
```

### Running a pattern

```python
import strider
from strider import pattern as p

result = strider.run(arch=…, cc=…, mem=…, entry=…)
graph = result.graph

c = p.Capture()
pat = p.add(p.var(c), p.int_const(8))
hits = graph.find_all(pat)            # list[Match]
for h in hits:
    print(h.uint(c) if h.uint(c) is not None else h.vn(c))
```

Multi-pattern join on shared captures:

```python
hits = graph.find_all_requirements([pat_a, pat_b, pat_c])
```

`ignore_casts=True` on `find_all` skips intervening `CastToInt` /
`CastToBool` / `CastToFloat` nodes — useful when matching across
implicit width conversions.

## Worked examples

### Example 1: "match `Load` where the address is `sp + K`"

The lifter doesn't promote SP-relative loads to a special node (it
does for stores).  An SP-relative load is just
`Load(Add(InitialVar(sp), IntConst(K)))`.  After `ConstantFold` the
constant survives; after no other passes does the shape change.

```python
from strider import pattern as p

sp_const = p.Capture()
pat = p.load(addr=p.add(p.initial_var(), p.any_int_const(sp_const)))
```

Notes:

- `p.initial_var()` matches *any* `InitialVar(_)`, not specifically
  the SP register.  If you want SP precisely, use
  `p.initial_var_for(sleigh.reg("RSP"))` (x86_64) / `("ESP")` (x86) /
  `("SP")` (ARM/AArch64).
- `add` is commutative — this pattern also matches
  `Load(Add(IntConst(K), InitialVar(sp)))`.  No need to write both.

### Example 2: "Load where address is sp+K, result is then truncated"

The trick: `truncate(p.load(...))` doesn't work directly because the
typed `LoadPat` is a builder, not a `Pat`.  Pass the builder where a
`Pat` is expected and `PatLike` auto-finalises:

```python
from strider import pattern as p

k = p.Capture()
val_cap = p.Capture()

# The outer truncate wraps the load.  Pass the LoadPat builder as
# operand — PatLike converts it to Pat at use-site.
pat = p.truncate(
    p.load(addr=p.add(p.initial_var(), p.any_int_const(k))).capture(val_cap)
)
```

### Example 3: "xor x, x" (zero idiom)

Same value in both operand slots → use string back-reference:

```python
pat = p.xor("v", "v")
```

`xor` is commutative but order doesn't matter when both operands are
the same.  `"v"` interns to a `Capture` and the back-reference
enforces structural equality.

### Example 4: "indexed array load: base + idx * stride"

```python
pat = p.load(addr=p.add("base", p.mul("idx", "stride")))
```

`add` and `mul` are both commutative, so this also matches
`Load((idx*stride) + base)` and `Load(base + stride*idx)` — all four
orderings.  No need to enumerate.

### Example 5: "a >= b" (signed)

Source `a >= b` lowers via the lifter to two different shapes
depending on whether the compiler emitted `>=` or `!(<)`.  The strider
canonical form for `>=` is identical to `<=` with swapped operands:
`a >= b` is `b <= a`, which lifts to `BoolNeg(IntSless(a, b))` for
signed comparisons.

```python
# a >= b  (signed)
pat = p.int_sle("b", "a")          # produces BoolNeg(IntSless(a, b))
# or equivalently:
pat = p.bool_not(p.int_cmp("Sless", "a", "b"))
```

### Example 6: "Call to address 0x4010a0 with arg0 == 8"

```python
pat = p.call(at=0x4010a0).arg(0, p.int_const(8))
```

The typed `CallPat` builder is its own `PatLike`, so you can hand it
straight to `graph.find_all(pat)` without `into_pat()`.

### Example 7: "Capture the op variant of an arbitrary integer binop"

```python
op_cap = p.Capture()
l_cap = p.Capture()
r_cap = p.Capture()
pat = p.int_bin_any(op_cap, p.var(l_cap), p.var(r_cap))
# Then after match: h.int_binary_op(op_cap) → "Add" / "Mul" / …
```

### Example 8: "stack store of constant 0 at offset 16, followed by load at same offset"

```python
# This requires the `StackLoadForward` opt pass to have NOT yet run,
# OR you want to find the unforwarded form.
pat = p.stack_store(offset=16, data=p.int_const(0))
```

If you want store-load forwarding chains across a `MemPhi`, look for
`Phi(None)` (`p.value_phi()`) — the optimizer inserts these.

### Example 9: "If branch with `a < b` condition" (compiler may have inverted)

```python
pat = p.if_(cond=p.int_lt("a", "b"))
```

The `IfPat` matcher automatically tries the
`If(BoolNeg(IntLess(a, b))){false}{true}` layout too — no need to
write both forms.

### Example 10: "Predicate guard — match `add(x, K)` only when K > 0"

```python
k = p.Capture()
def positive(h):
    val = h.int_(k)
    return val is not None and val > 0

pat = p.add("x", p.any_int_const(k)).when(positive)
```

## Anti-patterns

- **Writing the source-level shape.** `p.int_cmp("LessEqual", a, b)`
  raises — the IR has no `LessEqual` primitive.  Use `p.int_le(a, b)`
  (which builds the lowered shape) or `p.bool_not(p.int_cmp("Less",
  b, a))` explicitly.
- **Manually trying both commutative orderings.** Don't write
  `p.or_(p.add(a, b), p.add(b, a))` — `add` already tries both.
- **Forgetting `.into_pat()` when chaining.** Typed builders are
  `PatLike` — pass them straight to `find_all` / inner pattern slots.
  Only call `.into_pat()` when storing the result and reusing it.
- **Using `capture` as a back-reference key.**  String shorthand
  back-references go through the **same string** — not through a
  `Capture` object that was passed twice.  `p.xor(p.var(c),
  p.var(c))` does NOT enforce same-value; `p.xor("v", "v")` does.
  (The Rust pattern crate treats `Capture` similarly, but in Python
  the explicit `Capture` and string shorthand take slightly different
  paths — prefer strings for back-references.)
- **Matching post-optimization shapes when running pre-opt.**  If you
  call `find_all` on the unoptimised IR, `sub(x, K)` produces
  `Add(x, Neg(IntConst(K)))` and your `add(x, signed_int_const(-K))`
  won't match until `ConstantFold` runs.  Conversely, after the
  stable pipeline runs, `Neg(IntConst(K))` is gone.  Decide which
  layer you're querying.

## When to defer to other skills

- Rust-side pattern authoring → `strider-pattern-author`.
- Debugging a pattern that returns zero matches → `strider-debug-pattern`.
- Writing a rewrite rule (RHS-builds-new-graph) →
  `strider-rewrite-rule-multinode-audit`.
- Adding a new pattern builder to the surface → `strider-py-binding`
  + `strider-pattern-author`.
