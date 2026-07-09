---
name: strider-py-pattern
description: Use when a Python user wants to write a strider pattern (strider.pattern.*) to match
  an IR shape on a lifted binary — produces correct Python pattern code given a natural-language
  description. Knows the full Python builder surface (including OffsetCapture, stack_only,
  CastMask), lift-time canonicalisations, and commutative ops so the generated pattern matches
  the canonical IR shape rather than the source-level shape.
---

# strider-py-pattern

Generate idiomatic `strider.pattern` Python code from a natural-language description of an IR
shape.

**Use when** the user asks "give me a pattern that matches X" / "how do I find all loads where the
address is sp+const" / "write a pattern for the indexed-array-load shape" and similar.

**Do NOT use** for Rust-side patterns (write them by hand against
`strider_pattern`) or for graph rewriting (use the
`strider-rewrite-rule-author` skill).

## How to use this skill

1. Read the user's spec carefully.  Identify (a) the **root node kind** (load, store, call, add,
   etc.), (b) the **operand shapes** the user wants to constrain, (c) what should be **captured**
   for post-processing.
2. **Apply lift-time canonicalisations** before writing the pattern.  The lifter rewrites `a - b`
   to `Add(a, Neg(b))` etc., so a pattern for the source-level shape will NEVER match unless you
   rewrite to the canonical form first.  See `### Lift-time canonicalisations` below.
3. Choose the right builder for each level.  Use the `### Available builders` cheat sheet.
4. Decide between **string-shorthand captures** ("x" auto-interns to a `Capture`) and explicit
   `Capture()` objects (when you need the capture passed to multiple places or to `var(c)`).  Use
   `OffsetCapture()` specifically for SP-relative offset binding from `LoadPat.offset_capture` /
   `StorePat.offset_capture`.
5. Mention **commutativity** if it affects the spec — commutative ops automatically try both
   operand orderings.  Use `.ordered()` on a typed builder to suppress this.
6. Emit the code as a single Python snippet, plus a 1-2 line explanation of why the canonical form
   differs from the source.

## Cheat sheet

### Captures

```python
from strider.pattern import Capture, OffsetCapture, var, any_int_const, int_xor

# Explicit Capture — use when the same capture appears in multiple slots or
# you need it as a back-reference.
c = Capture()
pat = add(var(c), int_const(8))           # later: h.uint(c)

# String shorthand — each unique string interns to the same Capture per process.
pat = int_xor("v", "v")                    # zero-idiom: must be same value

# OffsetCapture — binds the i64 SP-relative offset of a matched stack Load/Store.
# Use with LoadPat.offset_capture(oc) or StorePat.offset_capture(oc); retrieve via
# match_.captured_offset(oc) -> int | None.
oc = OffsetCapture()
pat = load().stack_only().offset_capture(oc)
# after match: h.captured_offset(oc) -> int

# Reading captures back from a Match `h`:
h.uint(c)           # → int  (None if not bound or not an IntConst)
h.int_(c)           # → int  (signed i128 interpretation)
h.bool_(c)          # → bool
h.float_bits(c)     # → u64 bits of a FloatConst
h.vn(c)             # → Vn of captured InitialVar / tagged Phi / FunctionArg
h.has(c)            # → True/False whether the capture is bound
h.root              # → NodeId (u32) of the top-level match root (getter)
h.asm_fingerprint(c) # → list[int] of asm addresses

# Op-variant accessors (for *_any captures):
h.int_binary_op(c)  # → "Add" / "Mul" / etc.
h.int_unary_op(c)   # → "Neg" / "BitNot"
h.int_cmp_op(c)     # → "Less" / "Equal" / "Sless" / etc.
h.bool_binary_op(c) # → "And" / "Or" / "Xor"
h.bool_unary_op(c)  # → "Not"
h.float_binary_op(c)# → "Add" / "Mul" / etc.
h.float_unary_op(c) # → "Neg" / "Abs" / etc.
h.float_cmp_op(c)   # → "Equal" / "Less"

# OffsetCapture retrieval (NOT a Capture — different method):
h.captured_offset(oc) # → int | None  (requires OffsetCapture, not Capture)
```

Note: there is no `h.stack_offset` or `h.stack_phi_offsets` method on `Match`.  The offset of a
matched stack Load/Store is retrieved exclusively via `captured_offset(oc)` where `oc` is an
`OffsetCapture` bound in the pattern.

### Available builders

Source of truth: `crates/strider-py/src/pattern.rs` (the `register()` function near the bottom
enumerates every registered name).

| Builder | Rust IR shape produced | Python signature | Commutative? |
|---|---|---|---|
| `p.anything()` | wildcard | `anything() -> Pat` | n/a |
| `p.var(c)` | wildcard + capture | `var(c: Capture) -> Pat` | n/a |
| `p.int_const(K)` | `IntConst(K)` (strict width) | `int_const(value: int) -> Pat` | n/a |
| `p.signed_int_const(K)` | `IntConst` re-interpreted as signed across widths | `signed_int_const(value: int) -> Pat` | n/a |
| `p.int_const_any_of([…])` | `IntConst ∈ set` | `int_const_any_of(values: list[int]) -> Pat` | n/a |
| `p.bool_const(b)` | `BoolConst(b)` | `bool_const(value: bool) -> Pat` | n/a |
| `p.float_const(bits)` | `FloatConst(bits)` | `float_const(bits: int) -> Pat` | n/a |
| `p.any_int_const(c=None)` | any `IntConst`, capture optional | `any_int_const(c: Capture=None) -> Pat` | n/a |
| `p.any_bool_const(c=None)` | any `BoolConst`, capture optional | `any_bool_const(c=None) -> Pat` | n/a |
| `p.any_float_const(c=None)` | any `FloatConst`, capture optional | `any_float_const(c=None) -> Pat` | n/a |
| `p.initial_var()` | any `InitialVar(_)` | `initial_var() -> Pat` | n/a |
| `p.initial_var_for(vn)` | `InitialVar(vn)` | `initial_var_for(vn: Vn) -> Pat` | n/a |
| `p.function_arg(i)` | `FunctionArg{index=i}` | `function_arg(i: int) -> FunctionArgPat` | n/a |
| `p.function_arg_any()` | any `FunctionArg` | `function_arg_any() -> FunctionArgPat` | n/a |
| `p.function_arg_reg(vn)` | `FunctionArg` for register `vn` | `function_arg_reg(vn: Vn) -> FunctionArgPat` | n/a |
| `p.function_arg_stack(s, off)` | `FunctionArg` for stack arg | `function_arg_stack(space: VnSpace, offset: int) -> FunctionArgPat` | n/a |
| `p.phi()` | any `Phi` (tagged or anonymous) | builder w/ `.for_vn(vn)` `.input(idx, p)` | n/a |
| `p.phi_for(vn)` | `Phi` tagged with `vn` | `phi_for(vn: Vn) -> PhiPat` | n/a |
| `p.mem_phi()` | `MemPhi` | `mem_phi() -> MemPhiPat` | n/a |
| `p.value_phi()` | `Phi(None)` (anonymous, from `LoadForward`) | `value_phi() -> ValuePhiPat` | n/a |
| `p.predicate(f)` | match-any + Python guard | `predicate(f) -> Pat` | n/a |
| `p.add(a, b)` | `IntBinaryOp(Add)` | `add(l, r) -> Pat` | **yes** |
| `p.sub(a, b)` | `Add(a, Neg(b))` lowered | `sub(l, r) -> Pat` | no (lowered) |
| `p.mul(a, b)` | `IntBinaryOp(Mul)` | `mul(l, r) -> Pat` | **yes** |
| `p.div(a,b)` / `p.sdiv(a,b)` | unsigned / signed div | binary | no |
| `p.rem(a,b)` / `p.srem(a,b)` | unsigned / signed rem | binary | no |
| `p.shl(a,b)` / `p.shr(a,b)` / `p.sshr(a,b)` | shifts | binary | no |
| `p.int_and(a, b)` | `IntBinaryOp(And)` | binary | **yes** |
| `p.int_or(a, b)` | `IntBinaryOp(Or)` | binary | **yes** |
| `p.int_xor(a, b)` | `IntBinaryOp(Xor)` | binary | **yes** |
| `p.int_eq(a, b)` | `IntCmpOp(Equal)` | binary | **yes** |
| `p.int_lt(a, b)` / `p.int_slt` | unsigned / signed less-than | binary | no |
| `p.int_le(a, b)` | `BoolNeg(IntLess(b, a))` lowered | binary | no (lowered) |
| `p.int_sle(a, b)` | `BoolNeg(IntSless(b, a))` lowered | binary | no (lowered) |
| `p.int_carry` / `p.int_scarry` / `p.int_sborrow` | carry / overflow / borrow | binary | Carry & Scarry only |
| `p.int_cmp("Op", a, b)` | dispatch on op name | `int_cmp(op, l, r) -> Pat` | per op |
| `p.neg(x)` | `IntUnaryOp(Neg)` | unary | n/a |
| `p.int_not(x)` | `IntUnaryOp(BitNot)` | unary | n/a |
| `p.bool_and` / `p.bool_or` / `p.bool_xor` / `p.bool_not` | bool ops | bin/unary | **bool_and/or/xor commutative** |
| `p.float_add` / `p.float_sub` / `p.float_mul` / `p.float_div` | float arith | binary | Add/Mul **commutative** |
| `p.float_neg` / `p.float_abs` / `p.float_sqrt` / `p.float_ceil` / `p.float_floor` / `p.float_round` | float unary | unary | n/a |
| `p.float_is_nan(x)` | `BoolNeg(FloatEqual(x, x))` lowered | unary | n/a |
| `p.float_eq` / `p.float_ne` / `p.float_lt` / `p.float_le` | float cmp | binary | Equal commutative; le lowered |
| `p.int_to_float` / `p.float_to_int` / `p.float_to_float` | conversions | unary | n/a |
| `p.int_bits_to_float` / `p.float_bits_to_int` | bit-cast | unary | n/a |
| `p.cast_to_int` / `p.cast_to_bool` / `p.cast_to_float` | cast nodes | unary | n/a |
| `p.truncate(x)` | `Truncate` | unary | n/a |
| `p.popcount(x)` / `p.lzcount(x)` | popcount / lzcnt | unary | n/a |
| `p.zero_extend(x)` / `p.sign_extend(x)` / `p.extend("zero"\|"sign", x)` | width-extend | unary | n/a |
| `p.load(addr=…)` | `Load(_)` typed builder | `.addr(p) .space(s) .mem_in(p) .bit_width(n) .stack_only() .offset_capture(oc)` | n/a |
| `p.store(addr=…, data=…)` | `Store(_)` typed builder | `.addr .data .space .mem_in .next_mem .bit_width .stack_only() .offset_capture(oc)` | n/a |
| `p.call(at=…)` | `Call` builder | `.at(addr) .at_any([…]) .target(p) .arg(idx, p) .ret_output(idx, p)` | n/a |
| `p.call_other()` | `CallOther` builder | `.user_op_id(v) .name(s) .arg(i, p) .ret(i, p) .ctrl .mem .ctrl_out .mem_out .next_ctrl .next_mem` | n/a |
| `p.ret()` | `Return` builder | `.preceded_by(p) .ret_val(idx, p)` | n/a |
| `p.if_else(cond=…)` | `If` builder | `.cond(p) .true_branch(p) .false_branch(p)` — tries compiler-inverted layout too | n/a |
| `p.int_binary("Op", l, r)` | dispatch w/ chainable `.ordered()` | typed builder | per op |
| `p.bool_binary("Op", l, r)` | dispatch w/ `.ordered()` | typed builder | per op |
| `p.float_binary("Op", l, r)` | dispatch w/ `.ordered()` | typed builder | per op |
| `p.int_bin_any(c, l, r)` / `p.int_un_any(c, x)` / `p.int_cmp_any(c, l, r)` / `p.bool_bin_any` / `p.bool_un_any` / `p.float_bin_any` / `p.float_un_any` / `p.float_cmp_any` | variant-agnostic, captures the op | takes a Capture for the op | per concrete variant |

**LoadPat and StorePat — stack filter and offset capture:**

```python
from strider.pattern import OffsetCapture

oc = OffsetCapture()

# Match only stack-relative loads (Function.stack_offset(node) is Some).
p.load().stack_only()

# Match stack-relative loads and bind the offset for retrieval.
pat = p.load().offset_capture(oc)   # implies stack_only
# after match: h.captured_offset(oc) -> int | None

# Same for stores:
pat = p.store().stack_only().offset_capture(oc)
```

**CastMask — granular cast walk-through:**

```python
from strider.pattern import CastMask

# Walk through zero-extend and truncate casts, but not sign-extend:
mask = CastMask.zero_extend() | CastMask.truncate()
hits = graph.find_all(pat, ignore_casts_mask=mask)

# Walk through all cast kinds (equivalent to ignore_casts=True):
hits = graph.find_all(pat, ignore_casts_mask=CastMask.all())

# Available factory classmethods:
# CastMask.zero_extend(), .sign_extend(), .extend() (= zext|sext),
# .truncate(), .cast_to_int(), .cast_to_bool(), .cast_to_float(),
# .int_bits_to_float(), .float_bits_to_int(), .all(), .none() / .empty()
```

**Universal builder methods** (every typed builder has these):
`.capture(c)` (bind a `Capture`), `.cap("name")` (bind via auto-interned name), `.when(f)` (Python
predicate guard, signature `f(match: Match) -> bool` — there is no separate partial-match type;
`.when` sees the same `Match` handle `find_all` hands back, just mid-walk), `.into_pat()` (finalise to `Pat`).
Typed builders also accept being passed directly anywhere a `Pat` is expected — `into_pat()` is
implicit at use-site via the `PatLike` trait.

### Lift-time canonicalisations

The lifter rewrites these shapes at lift time, so a pattern for the source-level form will NEVER
match the IR.  When the user describes a shape using the source form, translate to the canonical
form before writing the pattern.

| Source-level shape | Canonical IR shape | Pattern helper that already produces it |
|---|---|---|
| `IntSub(a, b)` | `Add(a, Neg(b))` | `p.sub(a, b)` |
| `IntLessEqual(a, b)` | `BoolNeg(IntLess(b, a))` (args swapped) | `p.int_le(a, b)` |
| `IntSlessEqual(a, b)` | `BoolNeg(IntSless(b, a))` | `p.int_sle(a, b)` |
| `IntNotEqual(a, b)` | `BoolNeg(IntEqual(a, b))` | `p.bool_not(p.int_cmp("Equal", a, b))` |
| `FloatSub(a, b)` | `FloatAdd(a, FloatNeg(b))` | `p.float_sub(a, b)` |
| `FloatNotEqual(a, b)` | `BoolNeg(FloatEqual(a, b))` | `p.float_ne(a, b)` |
| `FloatLessEqual(a, b)` | `Or(FloatLess(a, b), FloatEqual(a, b))` | `p.float_le(a, b)` |
| `FLOAT_NAN(x)` | `BoolNeg(FloatEqual(x, x))` | `p.float_is_nan(x)` |
| `If(BoolNeg(C)){A}{B}` | `If(C){B}{A}` (after `IfCondInversion` opt pass) | `p.if_else(cond=C)` — matcher tries both layouts |

**Optimizer-induced shape changes** (after `Strider` runs the stable pipeline):

- `Add(a, Neg(IntConst(K)))` constant-folds to `Add(a, IntConst(-K))`.
  So `sub(x, int_const(8))` may not match if `ConstantFold` ran;
  prefer `add(x, signed_int_const(-8))` against optimised graphs.
- `Load(IntConst(addr))` may fold to a value via `LoadReadOnly` if a ROM was passed.
- SP-relative `Store` / `Load` annotation lives in `Function::stack_offsets` after
  `StackOffsetDetect`; for `Load`/`Store`, use `p.load().stack_only()` / `p.store().stack_only()`
  or `.offset_capture(oc)` to filter to stack-relative ops without hard-coding the SP varnode.
- After `LoadForward`, a same-offset load-after-store may become a `Phi(None)` (`value_phi`)
  when the forwarding crossed a `MemPhi`.

### Commutative ops (matcher tries both operand orderings)

Single source of truth: `NodeKind::is_commutative` in
`crates/strider-ir/src/node/kind.rs`.

- `IntBinaryOp`: `Add`, `Mul`, `And`, `Or`, `Xor`
- `BoolBinaryOp`: `And`, `Or`, `Xor`
- `FloatBinaryOp`: `Add`, `Mul`
- `IntCmpOp`: `Equal`, `Carry`, `Scarry`
- `FloatCmpOp`: `Equal`

For these, do NOT manually write both orderings.  To DISABLE commutativity on a specific match,
use the typed family dispatcher with `.ordered()`:

```python
# Match `Add(IntConst(5), x)` but NOT `Add(x, IntConst(5))`:
p.int_binary("Add", p.int_const(5), p.var(c)).ordered()
```

### Running a pattern

```python
import strider
from strider import pattern as p

lift = strider.lifter(arch=…, mem=…)
graph, unresolved = lift.analyze(entry=…, cc=…)

c = p.Capture()
pat = p.add(p.var(c), p.int_const(8))
hits = graph.find_all(pat)            # list[Match]
for h in hits:
    print(h.uint(c) if h.uint(c) is not None else h.vn(c))
```

Walk-through flags on `find_all` / `find_joined`:

```python
# Skip intervening cast nodes (all kinds):
graph.find_all(pat, ignore_casts=True)

# Skip specific cast kinds only:
graph.find_all(pat, ignore_casts_mask=p.CastMask.zero_extend() | p.CastMask.truncate())

# Skip phi/region nodes (match across control-flow joins):
graph.find_all(pat, ignore_regions=True)
```

Multi-pattern join on shared captures:

```python
hits = graph.find_joined([pat_a, pat_b, pat_c])
```

Walk-through flags also apply to `find_joined`; all flags apply uniformly to all patterns.

## Worked examples

### Example 1: "match `Load` where the address is `sp + K`"

```python
from strider import pattern as p

sp_const = p.Capture()
pat = p.load(addr=p.add(p.initial_var(), p.any_int_const(sp_const)))
```

Notes:
- `p.initial_var()` matches *any* `InitialVar(_)`.  For SP precisely, use
  `p.initial_var_for(sleigh.reg("RSP"))` (x86_64).
- `add` is commutative — also matches `Load(Add(IntConst(K), InitialVar(sp)))`.

### Example 2: "match stack-relative loads and retrieve the offset"

The SP-offset annotation lives in `Function::stack_offsets` after `StackOffsetDetect`.  Use
`stack_only()` to filter and `offset_capture` to retrieve the SP-relative offset:

```python
from strider import pattern as p

oc = p.OffsetCapture()
pat = p.load().stack_only().offset_capture(oc)  # offset_capture implies stack_only
hits = graph.find_all(pat)
for h in hits:
    offset = h.captured_offset(oc)  # int | None — always int here (offset_capture implies stack)
    print(f"stack load at sp+{offset}")
```

### Example 3: "Load where address is sp+K, result is then truncated"

```python
from strider import pattern as p

k = p.Capture()
val_cap = p.Capture()

pat = p.truncate(
    p.load(addr=p.add(p.initial_var(), p.any_int_const(k))).capture(val_cap)
)
```

### Example 4: "xor x, x" (zero idiom)

```python
pat = p.int_xor("v", "v")
```

### Example 5: "indexed array load: base + idx * stride"

```python
pat = p.load(addr=p.add("base", p.mul("idx", "stride")))
```

### Example 6: "Call to address 0x4010a0 with arg0 == 8"

```python
pat = p.call(at=0x4010a0).arg(0, p.int_const(8))
```

### Example 7: "Call to any of several known thunk addresses"

```python
THUNKS = [0x401000, 0x401020, 0x401040]
pat = p.call().at_any(THUNKS)
```

### Example 8: "Capture the op variant of an arbitrary integer binop"

```python
op_cap = p.Capture()
l_cap = p.Capture()
r_cap = p.Capture()
pat = p.int_bin_any(op_cap, p.var(l_cap), p.var(r_cap))
# Then: h.int_binary_op(op_cap) → "Add" / "Mul" / …
```

### Example 9: "If branch with `a < b` condition" (compiler may have inverted)

```python
pat = p.if_else(cond=p.int_lt("a", "b"))
```

### Example 10: "Predicate guard — match `add(x, K)` only when K > 0"

```python
k = p.Capture()
def positive(h):
    val = h.int_(k)
    return val is not None and val > 0

pat = p.add("x", p.any_int_const(k)).when(positive)
```

### Example 11: "a >= b" (signed)

```python
# a >= b  (signed) — same as b <= a, which lifts to BoolNeg(IntSless(a, b))
pat = p.int_sle("b", "a")
# or equivalently:
pat = p.bool_not(p.int_cmp("Sless", "a", "b"))
```

### Example 12: "Match loads through cast nodes"

When the architecture emits width casts between a load and its consumer, use `ignore_casts_mask`
or `ignore_casts=True` so the pattern matches the load regardless of intervening cast nodes:

```python
oc = p.OffsetCapture()
load_pat = p.load().offset_capture(oc)
hits = graph.find_all(load_pat, ignore_casts=True)
```

## Anti-patterns

- **`h.stack_offset` / `h.stack_phi_offsets` don't exist.**  Use `h.captured_offset(oc)` where
  `oc` is an `OffsetCapture` bound in the pattern via `.offset_capture(oc)`.
- **Writing the source-level shape.** `p.int_cmp("LessEqual", a, b)` raises — use `p.int_le`.
- **Manually trying both commutative orderings.** `add` already tries both.
- **Forgetting `.into_pat()` when chaining.** Typed builders are `PatLike` — pass them straight.
- **Using `capture` as a back-reference key.**  String back-references go through the **same
  string**: `p.int_xor("v", "v")` enforces same-value.  `p.int_xor(p.var(c), p.var(c))` does NOT.
- **Matching post-optimization shapes when running pre-opt.**  `sub(x, K)` produces
  `Add(x, Neg(IntConst(K)))` pre-opt; after `ConstantFold`, `Neg(IntConst(K))` folds to
  `IntConst(-K)`.  Match with `add(x, signed_int_const(-K))` against optimised graphs.
- **Confusing `OffsetCapture` with `Capture`.**  They are different types.  `OffsetCapture` is
  used exclusively with `.offset_capture(oc)` on `LoadPat`/`StorePat`; retrieved via
  `h.captured_offset(oc)`.  `Capture` is used everywhere else.

## When to defer to other skills

- Rust-side pattern authoring → write by hand against
  `strider_pattern` (the Rust builder surface).
- Writing a rewrite rule (RHS-builds-new-graph) →
  `strider-rewrite-rule-author`.
- Adding a new pattern builder to the surface → extend the Rust-side
  builder in `strider-pattern`, then update the PyO3 mirror
  (or its emission via `strider-pattern-macros`) in `strider-py`.
- Assembly → IR → pattern translation → `strider-asm-to-pattern`.
