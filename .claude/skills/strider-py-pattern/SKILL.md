---
name: strider-py-pattern
description: Use when a Python user wants to write a strider pattern (strider.pattern.*) to match
  an IR shape on a lifted binary — produces correct Python pattern code given a natural-language
  description. Knows the full Python builder surface (including stack_only, stack_offset,
  CastMask), lift-time canonicalisations, and commutative ops so the generated pattern matches
  the canonical IR shape rather than the source-level shape.
---

# strider-py-pattern

Generate idiomatic `strider.pattern` Python code from a natural-language description of an IR
shape.

> Every builder, flag and `Match` accessor named below was executed against the built extension on
> 2026-07-17.  If you add to this file, run the snippet — the drift this replaced included an
> `OffsetCapture` / `offset_capture` / `captured_offset` API that never existed, an
> `ignore_regions` flag, `cast_to_int` / `cast_to_bool` / `cast_to_float` builders, and
> `reaches` / `not_reaches` constraints that were removed.

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
   Filter SP-relative accesses with `LoadPat`/`StorePat`'s `stack_only()` / `stack_offset(K)`.
5. Mention **commutativity** if it affects the spec — commutative ops automatically try both
   operand orderings.  Use `.ordered()` on a typed builder to suppress this.  Note that a capture
   on a commutative operand therefore yields one hit PER operand it can bind (and makes
   `find_unique` raise); `.ordered()` is the fix when the spec wants a specific slot.
6. Emit the code as a single Python snippet, plus a 1-2 line explanation of why the canonical form
   differs from the source.

## Cheat sheet

### Captures

```python
from strider.pattern import Capture, var, any_int_const, int_xor

# Explicit Capture — use when the same capture appears in multiple slots or
# you need it as a back-reference.
c = Capture()
pat = add(var(c), int_const(8))           # later: h.uint(c)

# String shorthand — each unique string interns to the same Capture per process.
pat = int_xor("v", "v")                    # zero-idiom: must be same value


# Reading captures back from a Match `h`:
h.uint(c)           # → int  (None if not bound or not an IntConst)
h.int(c)            # → int  (signed i128 interpretation)
h.bool(c)           # → bool
h.float_bits(c)     # → u64 bits of a FloatConst
h.vn(c)             # → Vn of captured InitialVar / tagged Phi / FunctionArg
h.has(c)            # → True/False whether the capture is bound
h.node(c)           # → Node | None
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

```

Note: `Match` has no stack-offset accessor.  Filter stack accesses in the PATTERN —
`load().stack_only()` or `load().stack_offset(K)`.

### Available builders

Source of truth: `crates/strider-py/src/pattern.rs` (the `register()` function near the bottom
enumerates every registered name).

| Builder | Rust IR shape produced | Python signature | Commutative? |
|---|---|---|---|
| `p.anything()` | wildcard | `anything() -> Pat` | n/a |
| `p.one_of([a, b])` | alternation (first match wins) | `one_of(pats: list[PatLike]) -> Pat` | n/a |
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
| `p.phi()` | any `Phi` (tagged or anonymous) | builder w/ `.for_vn(vn)` `.input(idx, p)` `.any_input(p)` | n/a |
| `p.phi_for(vn)` | `Phi` tagged with `vn` | `phi_for(vn: Vn) -> PhiPat` | n/a |
| `p.mem_phi()` | `MemPhi` | `mem_phi() -> MemPhiPat` | n/a |
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
| `p.truncate(x)` | `Truncate` | unary | n/a |
| `p.popcount(x)` / `p.lzcount(x)` | popcount / lzcnt | unary | n/a |
| `p.zero_extend(x)` / `p.sign_extend(x)` / `p.extend("zero"\|"sign", x)` | width-extend | unary | n/a |
| `p.load(addr=…)` | `Load(_)` typed builder | `.addr(p) .space(s) .mem_in(p) .bit_width(n) .stack_only() .stack_offset(k)` | n/a |
| `p.store(addr=…, data=…)` | `Store(_)` typed builder | `.addr .data .space .mem_in .next_mem .bit_width .stack_only() .stack_offset(k)` | n/a |
| `p.call(at=…)` | `Call` builder | `.at(addr) .at_any([…]) .target(p) .arg(idx, p) .ret_output(idx, p)` | n/a |
| `p.call_other()` | `CallOther` builder | `.user_op_id(v) .name(s) .arg(i, p) .ret(i, p) .ctrl .mem .ctrl_out .mem_out .next_ctrl .next_mem` | n/a |
| `p.ret()` | `Return` builder | `.preceded_by(p) .ret_val(idx, p)` | n/a |
| `p.if_else(cond=…)` | `If` builder | `.cond(p) .true_branch(p) .false_branch(p) .capture_true(c) .capture_false(c)` — tries compiler-inverted layout too | n/a |
| `p.int_binary("Op", l, r)` | dispatch w/ chainable `.ordered()` | typed builder | per op |
| `p.bool_binary("Op", l, r)` | dispatch w/ `.ordered()` | typed builder | per op |
| `p.float_binary("Op", l, r)` | dispatch w/ `.ordered()` | typed builder | per op |
| `p.int_bin_any(c, l, r)` / `p.int_un_any(c, x)` / `p.int_cmp_any(c, l, r)` / `p.bool_bin_any` / `p.bool_un_any` / `p.float_bin_any` / `p.float_un_any` / `p.float_cmp_any` | variant-agnostic, captures the op | takes a Capture for the op | per concrete variant |

**LoadPat and StorePat — stack filters:**

```python
# Match only stack-relative loads (Function.stack_offset(node) is Some).
p.load().stack_only()

# Match only the stack-relative load at exactly SP+K.
p.load().stack_offset(0x10)

# Same verbs on stores:
p.store().stack_only()
p.store().stack_offset(0x10)
```

There is no offset-*capture*: the offset is a filter you supply, not a value bound out.

**CastMask — granular cast walk-through:**

```python
from strider.pattern import CastMask

# Walk through zero-extend and truncate casts, but not sign-extend:
mask = CastMask.zero_extend() | CastMask.truncate()
hits = fn.find_all(pat, ignore_casts_mask=mask)

# Walk through all cast kinds (equivalent to ignore_casts=True):
hits = fn.find_all(pat, ignore_casts_mask=CastMask.all())

# Available factory classmethods:
# CastMask.zero_extend(), .sign_extend(), .extend() (= zext|sext),
# .truncate(), .int_bits_to_float(), .float_bits_to_int(),
# .all(), .none() / .empty()
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
  or `.stack_offset(K)` to filter to stack-relative ops without hard-coding the SP varnode.
- After `LoadForward`, a same-offset load-after-store may become an anonymous `Phi(None)`
  when the forwarding crossed a `MemPhi`.  There is no `value_phi()` builder — use `p.phi()`,
  which matches both the tagged and anonymous forms.

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

`.ordered()` is chainable, not terminal — it returns the same builder, so it
nests as a value operand anywhere a bare builder does (which is exactly where
operand ambiguity lives) and composes with `.capture()` / `.when()`:

```python
p.store(data=p.int_binary("And", p.var(x), p.any_int_const()).ordered().capture(c))
```

#### A commutative op can yield MORE THAN ONE hit per root

Because both orderings are tried, a capture on an operand binds to **each**
operand in turn, and `find_all` reports every distinct binding:

```python
k = p.Capture()
fn.find_all(p.add(p.anything().capture(k), p.anything()))   # TWO hits on add(x, y):
                                                            #   k = x  (natural order)
                                                            #   k = y  (swapped)
```

Dedup is by the capture->binding **map**, so this only happens when the
orderings actually bind differently: `p.add(p.var(x), p.var(x))` and any
pattern with no captures on the operands stay at ONE hit.  Consequences:

- `find_unique` **raises** on the ambiguous pattern above — that is the point.
  Pin the intent with `.ordered()`, or narrow the operands, to make it unique.
- `find_all(...)[0]` is the first hit, and the natural operand order is pinned,
  so index 0 is deterministic rather than incidental.
- `one_of` is unaffected: its arms are an ordered choice (first match wins), so
  a later arm never contributes a second binding.

### Running a pattern

```python
import strider
from strider import pattern as p

lift = strider.lifter(arch=…, mem=…)
cfg, fn, unresolved = lift.analyze(entry=…, cc=…)   # THREE values

c = p.Capture()
pat = p.add(p.var(c), p.int_const(8))
hits = fn.find_all(pat)            # list[Match]
for h in hits:
    print(h.uint(c) if h.uint(c) is not None else h.vn(c))
```

Walk-through flags on `find_all` / `find_joined`:

```python
# Skip intervening cast nodes (all kinds):
fn.find_all(pat, ignore_casts=True)

# Skip specific cast kinds only:
fn.find_all(pat, ignore_casts_mask=p.CastMask.zero_extend() | p.CastMask.truncate())
```

Multi-pattern join on shared captures (pass a `list` to `find_all`):

```python
hits = fn.find_all([pat_a, pat_b, pat_c])
```

Walk-through flags also apply to a joined `find_all`; all flags apply uniformly to all patterns.

**CFG relational join constraints** — filter a joined result by control-flow
relations between captured entities (in addition to shared-capture equality).

They live in their own namespace, **`strider.pattern.constraints`** — NOT in
`strider.pattern`. A *pattern* describes graph SHAPE and goes in the first
argument (`find_all(pats, ...)`); a *constraint* is a relational predicate over
the captures those patterns bind, evaluated after the join and passed as
`constraints=[...]`. There are no back-compat aliases: `p.dominates` does not
exist.

```python
from strider import pattern as p
from strider.pattern import constraints as cons

g, t, f, c, fop = (p.Capture() for _ in range(5))
guard = p.if_else(cond=p.int_ne(p.load(p.add(p.var(fop), p.any_int_const())), p.int_const(0))) \
         .capture(g).capture_true(t).capture_false(f)
call  = p.call().capture(c)

# "call gated on the TRUE branch of the guard, exclusively":
hits = fn.find_all([guard, call], constraints=[cons.dominated_by_branch(t, c)])
```

- `cons.dominates(a, b)` — node `a` dominates node `b` in the control subgraph.
- `cons.dominated_by_branch(branch, node)` — every path from the function entry to
  `node` traverses the branch EDGE, i.e. `node` is in that block *exclusively*: the
  sibling arm AND the post-merge tail are both excluded.
  One `dominated_by_branch(true_edge, c)` = "`c` is in the true block".
  `node` must be a CONTROL node (a `call()`, region, return — not a data value):
  data nodes are absent from the control subgraph and so match nothing.
  NOTE the polarity is the IR's, not the C source's: `je L` lifts to `CBRANCH L, ZF`,
  so the `If`'s TRUE edge is the jump-TAKEN edge and the fallthrough is the FALSE
  edge — the opposite of the source-level `if` body.
- `cons.phi_input_from_edge(phi, edge, value)` — the `phi` capture's data input on the
  predecessor fed by control `edge` is `value`: "the value merged from THIS
  branch is X". `edge` binds an `If`'s `capture_true`/`capture_false`.
  Also works for a `mem_phi()` — a memory token (e.g. a `store().capture(sv)`
  output) to ask "the memory merged from THIS branch".

  **Which arms an edge reaches.** An arm qualifies when the branch edge DOMINATES
  its predecessor's control edge — every path traversing that predecessor first
  traversed the branch edge. The arm's predecessor simply BEING the edge is the
  same rule (a zero-length path), so a merge across a `call` — or any other
  intervening block between the branch and the join — still pins, which is the
  common shape in real code (a call terminates its basic block, so the `If`'s edge
  is usually *not* the merge region's direct predecessor). A **guarded loop**
  (`if (c) { while (...) {...} }`) pins too: the loop header having a second
  predecessor (its own back-edge) does not make the guard optional.
  Reach is **exclusive**: an arm reachable from BOTH sides of the branch belongs
  to neither edge. A branch whose block splits and reaches the merge twice yields
  one match **per qualifying arm** — `find_all` enumerates them, it never picks one.

  **An empty result is AMBIGUOUS — this bites people.** `[]` means EITHER
  *`edge` reaches no arm of this phi* OR *it does, and the arm merges a different
  value*. The two are indistinguishable from the result alone, so a probe that
  reads like real discrimination ("the true edge merges 12, the false edge
  doesn't") may just be blind on both. Re-probe with `anything()` as the value:
  a wildcard **cannot fail on value grounds**, so `[]` from it proves the edge is
  not visible.

  ```python
  # Is this phi/edge pair even related? A wildcard cannot fail on value grounds.
  visible = fn.find_all([guard, phi], constraints=[
      cons.phi_input_from_edge(ph, t, p.anything())])
  if not visible:
      ...  # the edge does not reach this phi AT ALL — not a value mismatch
  ```

  Do this before concluding anything from a negative result.

  `value` takes either spelling:

  * **A pattern, matched inline at the arm value — prefer this.** The fact stays
    local: `find_all([if_else().capture_true(t), phi().capture(ph)],
    constraints=[phi_input_from_edge(ph, t, int_const(K))])`. Captures inside it
    bind and read back off the match (`any_int_const(v)` inline still gives
    `hit.uint(v)`), unifying with — never overwriting — anything the rest of the
    join already bound.
  * **A `Capture`**, which some other pattern in the list must bind; compared by
    identity: `find_all([if_else().capture_true(t), phi().capture(ph),
    any_int_const(v)], constraints=[phi_input_from_edge(ph, t, v)])`.

  Reach for the capture form only when the value genuinely IS a separate site you
  want matched in its own right. Otherwise it costs you: the extra root floats free,
  matching anywhere in the function and joining as a cartesian product against the
  phi (`find_all` enumerates all distinct bindings), with the constraint pruning
  only afterwards — plus each extra root needs its own capture hygiene. The inline
  form replaces that whole-graph root search with one match at a known value.

- `cons.negate(c)` — the negation of any constraint: a tuple survives iff `c` does
  NOT hold. (`negate`, not `not_` — the Python surface has no trailing-underscore
  keyword dodges, and a boolean-connective name would read as a sibling of the
  IR value ops `int_and` / `int_or` / `int_not`, which it is not.)

  **RANGE RESTRICTION — the rule that makes it sound.** Every capture `c`
  mentions must be bound by a *positive* pattern in the same `find_all` list.
  This is not a style rule; it is what stops negation from lying. A constraint
  fails when a capture is unbound — it never saw anything — and under negation
  that failure would flip to a vacuous TRUE and match EVERYTHING. So a `negate`
  over an unbound capture raises `StriderError` rather than silently matching:

  ```python
  # Exactly the calls NOT gated on the true edge (false arm + post-merge tail):
  fn.find_all([guard, call], constraints=[cons.negate(cons.dominated_by_branch(t, c))])

  # StriderError: `unbound` is bound by no pattern, so this would be vacuously
  # true for every tuple.
  fn.find_all([guard, call],
              constraints=[cons.negate(cons.dominated_by_branch(t, p.Capture()))])
  ```

  `negate(negate(c))` is the identity. `negate` of a `phi_input_from_edge` whose
  `value` is an **inline pattern** is rejected: that form BINDS captures rather
  than deciding a predicate, and there is nothing to bind on the false branch —
  use the `Capture` value spelling if you need to negate such a fact.

  Note this does NOT resolve the "empty result is ambiguous" trap above: `negate`
  negates the *constraint*, not the *visibility* of the edge. Probe with
  `anything()` first, then negate.

Constraints range over **control nodes** (`Call`/`Store`/`Region`/`If`/…); a
captured value resolves to its producer node. Prefer `capture_true`/`capture_false`
(the branch-edge *value*, stable under region collapse) over anchoring on the
successor region. Two patterns linked only by a constraint still count as
correlated for the connectivity check.

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

### Example 2: "match stack-relative loads"

The SP-offset annotation lives in `Function::stack_offsets` after `StackOffsetDetect`.  Filter to
SP-relative accesses with `stack_only()`, or pin a specific slot with `stack_offset(K)`:

```python
from strider import pattern as p

hits = fn.find_all(p.load().stack_only())        # every SP-relative load
at_10 = fn.find_all(p.load().stack_offset(0x10)) # only the load at SP+0x10
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
load_pat = p.load().stack_only()
hits = fn.find_all(load_pat, ignore_casts=True)
```

## Anti-patterns

- **`h.stack_offset` / `h.captured_offset` / `OffsetCapture` don't exist.**  The SP offset is a
  pattern-side FILTER, not a bound value: `load().stack_only()` / `load().stack_offset(K)`.
- **Writing the source-level shape.** `p.int_cmp("LessEqual", a, b)` raises — use `p.int_le`.
- **Manually trying both commutative orderings.** `add` already tries both.
- **Forgetting `.into_pat()` when chaining.** Typed builders are `PatLike` — pass them straight.
- **Passing a string where a `Capture` is required.**  Strings are accepted in *operand*
  positions (`p.add("x", "y")`), NOT as the capture argument of `p.var(...)` /
  `p.any_int_const(...)` / `.capture(...)` — those take a `Capture()` and raise `TypeError`
  on a string.  Both back-reference forms work and are equivalent: `p.int_xor("v", "v")` and
  `p.int_xor(p.var(c), p.var(c))` each enforce same-value.
- **Matching post-optimization shapes when running pre-opt.**  `sub(x, K)` produces
  `Add(x, Neg(IntConst(K)))` pre-opt; after `ConstantFold`, `Neg(IntConst(K))` folds to
  `IntConst(-K)`.  Match with `add(x, signed_int_const(-K))` against optimised graphs.
- **Assuming a cast/bool cast builder exists.**  There is no `cast_to_int` / `cast_to_bool` /
  `cast_to_float`: booleans are the 1-bit integer `I1`, so a bool→int widening is
  `zero_extend(x)` and there is no int→bool cast.

## When to defer to other skills

- Rust-side pattern authoring → write by hand against
  `strider_pattern` (the Rust builder surface).
- Writing a rewrite rule (RHS-builds-new-graph) →
  `strider-rewrite-rule-author`.
- Adding a new pattern builder to the surface → extend the Rust-side
  builder in `strider-pattern`, then update the PyO3 mirror
  (or its emission via `strider-pattern-macros`) in `strider-py`.
- Assembly → IR → pattern translation → `strider-asm-to-pattern`.
