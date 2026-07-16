# Python matcher API redesign — binding-centric queries

## Goal

Make the Python pattern-query API *binding-centric*: the caller cares about the
captures a match binds, not about the match's position or the number of ways it
was reached. Collapse the four query entry points into three, and make "join"
a property of the input rather than a separate method.

## Surface

Three methods on the `Function` returned by `analyze`. The result object keeps
its name `Match` (it still carries `root(s)`, so it is a match at a position, not
just a bag of captures — the redesign is binding-*centric* in its query
semantics, not a rename):

```python
fn.find_all(pat, *, ignore_root=False, ignore_casts=False, ignore_casts_mask=None) -> list[Match]
fn.find_one(pat, *, ...)     -> Match | None    # first binding, or None
fn.find_unique(pat, *, ...)  -> Match           # exactly one binding, else error
```

`pat` is `Pattern | list[Pattern]` (the existing `PatLike` boundary, extended to
accept a list):

- a single pattern → bindings of that pattern.
- a list → **merged** bindings — every pattern is matched and their captures are
  unified on shared `Capture` objects (the old `find_joined`). A single pattern
  is just the 1-element join with nothing to unify, so there is no special case
  in the return shape.

Removed: `find_joined`, `find_joined_unique`. Join-ness is expressed by *what*
you pass, not *which method* you call.

## `Match`

Same capture-keyed accessors as today, with the collection now deduped and a
list input producing merged captures:

- `b[c]`, `c in b`, `b.has(c)`
- `b.node(c)`, `b.vn(c)`, `b.asm_fingerprint(c)`
- `b.uint(c)`, `b.int(c)`, `b.bool(c)`, `b.float_bits(c)`
- op-name forwarders: `b.int_binary_op(c)`, `int_unary_op`, `int_cmp_op`,
  `bool_binary_op`, `float_binary_op`, `float_unary_op`, `float_cmp_op`

A `Match` carries the union of captures from whatever was passed in (one
pattern's captures, or all patterns' captures merged). It also keeps the
position it matched at: `roots` (a list — one per input pattern) with `root` as a
convenience for the single-pattern case. Roots are not the headline of the API
(rewriting is graph-level via `Function.rewrite`, never keyed off a returned
`Match`), but they are retained as the default dedup key and remain accessible.

## Uniqueness / dedup

`find_all` returns a **deduplicated list** (a list, not a Python `set`: stays
ordered and indexable, still guarantees uniqueness).

Dedup key is controlled by `ignore_root`:

- `ignore_root=False` (default): unique by **captures + root(s)**. Keeps distinct
  sites apart; removes only true duplicates — commutative-symmetry hits and
  multi-path hits at the same root.
- `ignore_root=True`: unique by **captures only**. Additionally collapses a
  binding reached from two different roots (the sea-of-nodes diamond), and
  collapses a capture-less pattern to a single binding.

For a list input the "root" is the tuple of per-pattern roots; `ignore_root=True`
ignores all of them.

## `find_unique` semantics

Assert exactly one binding: error on **0** and on **>1**, with distinct messages.
Returns the single `Match`. Replaces `hits = find_all(...); assert len==1; hits[0]`.

## Edge cases

| input | `find_all` | `find_one` | `find_unique` |
|---|---|---|---|
| `find_all(p)` vs `find_all([p])` | identical results | identical | identical |
| empty list `[]` | `[]` | `None` | error (0 ≠ 1) |
| one pattern, N sites | N unique bindings | first | error if N ≠ 1 |
| list, no shared captures | cross-product of per-pattern hits | first | error if ≠ 1 |

## Implementation notes

- **Python-layer only** — no `strider-pattern` (Rust matcher) changes. Dedup and
  merge happen in the `strider-py` wrappers over the existing matcher results
  (`Capture` is `Hash + Eq`; node ids are hashable, so `(root, captures)` and
  `captures` are both usable dedup keys).
- `PatLike` gains a list arm; each query builds one `Pattern` per input, runs the
  matcher (single → `find_all`/`find_first`; list → the existing joined path),
  then dedups by the selected key.
- Merged `Match` for a list = union of the per-pattern matches' capture
  accessors (the matcher already unified the shared captures during the join).

## Tests to add

- `find_all(p) == find_all([p])` (the 1-element-join invariant).
- dedup: a pattern that matches the same binding at two roots yields 1 binding
  with `ignore_root=True`, 2 with `ignore_root=False`.
- `find_unique` raises on 0 and on >1 (distinct messages).
- empty-list behavior for all three.
- a real join (shared capture across two patterns) returns merged bindings.
