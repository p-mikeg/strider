# Code-Quality Refactor

**Status:** design
**Date:** 2026-04-17

## Goal

Reduce duplication and boilerplate across the workspace so that adding new IR
features, optimization passes, and pattern queries requires less code and reads
more clearly. Readability is the primary success metric.

The refactor preserves all existing functionality and keeps every existing test
passing unchanged. There are no user-visible behavior changes to the tool.

## Non-goals

- Architectural rewrites. Crate boundaries stay as they are.
- New optimization passes or new pattern features — only restructuring of what
  exists today.
- Python bindings (still pending in a separate effort).
- Performance work. This is a readability refactor; benchmarks should be
  unchanged.

## High-level phase list

Five phases, landed in order. Each phase is independently reviewable; at every
phase boundary the tree builds and every test still passes.

1. **Phase 0** — `ir::ops` foundation: shared graph helpers, error-assertion
   helpers, and `NodeKind::is_phi`.
2. **Phase 1** — `NodeOutputType` reflection table.
3. **Phase 2** — `rewrite_rules!` proc-macro DSL and `opt/constant_fold.rs`
   rewrite.
4. **Phase 3** — `pattern` crate unification (builder trait, matcher helpers).
5. **Phase 4** — clean-up of `opt/known_bits.rs` and remaining passes using the
   newly-available helpers.

Ordering rationale: Phase 0 unblocks every other phase. Phase 1 is a small
standalone win. Phase 2 is the marquee refactor on the file most explicitly
flagged (`constant_fold.rs`). Phase 3 is independent of Phase 2 but benefits
from whatever macro-expansion experience was gained writing it. Phase 4 is
cleanup.

Cast-to-float lowering in `constant_fold.rs` stays as a hand-written helper
after Phase 2 (it is a four-way state machine producing different node kinds,
not a rewrite rule). Every other rewrite in `constant_fold.rs` becomes a rule.

## Phase 0 — `ir::ops` foundation

New module `crates/ir/src/ops/` with three submodules. All helpers are added as
methods on `BuiltFunctionGraph`, not as free functions, so call-sites read
`fg.int_const_val(x)` instead of `int_const_val(fg, x)`.

```rust
// crates/ir/src/ops/mod.rs
pub mod consts;
pub mod rewrite;
pub mod builder;

// crates/ir/src/ops/consts.rs
impl BuiltFunctionGraph {
    /// Returns the integer constant value of `out`, masked to its declared
    /// type, or `None` if the output is not an integer constant.
    pub fn int_const_val(&self, out: NodeOutputId) -> Option<u64>;
    /// Returns the boolean constant value of `out`, or `None`.
    pub fn bool_const_val(&self, out: NodeOutputId) -> Option<bool>;
    /// Returns the raw bits of a float constant, or `None`.
    pub fn float_const_val(&self, out: NodeOutputId) -> Option<u64>;

    pub fn make_int_const(&mut self, val: u64, ty: NodeOutputType)
        -> Result<NodeOutputId, ir::Error>;
    pub fn make_bool_const(&mut self, val: bool)
        -> Result<NodeOutputId, ir::Error>;
    pub fn make_float_const(&mut self, bits: u64, ty: NodeOutputType)
        -> Result<NodeOutputId, ir::Error>;
}

// crates/ir/src/ops/rewrite.rs
impl BuiltFunctionGraph {
    /// Redirects every consumer of `old` to `new_val`. Returns true if at
    /// least one use was replaced.
    pub fn replace_all_uses(
        &mut self,
        old: NodeOutputId,
        new_val: NodeOutputId,
    ) -> Result<bool, ir::Error>;
}

// crates/ir/src/ops/builder.rs
impl BuiltFunctionGraph {
    /// Creates a node with a single value output of `ty` and returns the
    /// output id directly. Shortcut for the common pattern
    ///   let n = g.create_node(kind, inputs, [NodeOutputKind::OutputType(ty)]);
    ///   let [out] = g.node_outputs_exact::<1>(n)?;
    pub fn make_value_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = NodeOutputId>,
        ty: NodeOutputType,
    ) -> Result<NodeOutputId, ir::Error>;
}
```

**Error-assertion helpers** on `NodeOutputKind` (used ~60 times across `opt/`,
currently spelled `.as_value().ok_or(ErrorKind::ExpectedValueOutput(...))?`):

```rust
// crates/ir/src/node.rs (added to existing NodeOutputKind impl)
impl NodeOutputKind {
    /// Returns the value type or an error whose payload is `self`.
    pub fn as_value_or_err(self) -> Result<NodeOutputType, ir::Error>;
    /// Asserts the value type is integer; returns the type or an error.
    pub fn as_integer_or_err(self) -> Result<NodeOutputType, ir::Error>;
    /// Asserts the value type is float; returns the type or an error.
    pub fn as_float_or_err(self) -> Result<NodeOutputType, ir::Error>;
}
```

The corresponding error variant is added to `ir::Error` (`ExpectedValueOutput`,
`ExpectedIntegerType`, `ExpectedFloatType`) if not already present; `opt::Error`
keeps its existing variants and converts through the `ir::Error` via the
existing `From` impl.

**`NodeKind::is_phi` helper** (used 6+ times across validate and redundant_phis):

```rust
// crates/ir/src/node.rs (added to NodeKind impl)
impl NodeKind {
    pub fn is_phi(&self) -> bool {
        matches!(
            self,
            NodeKind::ControlPhi(_) | NodeKind::MemPhi | NodeKind::StackStorePhi { .. }
        )
    }
}
```

### Migration

Every call site in `opt/known_bits.rs`, `opt/dead_branch.rs`,
`opt/redundant_phis.rs`, `opt/load_readonly.rs`, and `opt/stack_store.rs`
switches to the new methods. `opt/constant_fold.rs` is intentionally left
alone in Phase 0 because Phase 2 rewrites it wholesale. `opt/utils.rs` is
deleted at the end of this phase. `opt::OptimizationResult::from_changed(bool)`
is added so existing `replace_all_uses` callers still get an
`OptimizationResult` easily.

The `as_value_or_err` / `as_integer_or_err` / `as_float_or_err` methods are
introduced in Phase 0 but most call-site conversions happen in Phase 4 (a
pure mechanical sweep). Phase 0 converts only the call-sites touched while
moving `opt/utils.rs` contents into `ir::ops`.

### Savings

~150 LOC across `opt/` from `NodeOutputKind` helpers, ~60 LOC from
`replace_all_uses`/const helpers moving (net — the helpers themselves exist in
one place now).

## Phase 1 — `NodeOutputType` reflection table

In `crates/ir/src/node.rs`, `NodeOutputType` has 9 variants and several methods
(`byte_size`, `bit_width`, `as_str`, `is_integer`, `is_bool`, `is_float`) that
each repeat the same 9-arm match. Replace with one table and category enum.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category { Bool, Int, Float }

struct TypeInfo {
    name: &'static str,
    byte_size: u8,
    category: Category,
}

// Indexed by `NodeOutputType as usize` (order must match enum declaration).
const TYPE_INFO: &[TypeInfo] = &[
    TypeInfo { name: "bool",  byte_size: 1,  category: Category::Bool },
    TypeInfo { name: "u8",    byte_size: 1,  category: Category::Int },
    TypeInfo { name: "u16",   byte_size: 2,  category: Category::Int },
    TypeInfo { name: "u32",   byte_size: 4,  category: Category::Int },
    TypeInfo { name: "u64",   byte_size: 8,  category: Category::Int },
    TypeInfo { name: "u128",  byte_size: 16, category: Category::Int },
    TypeInfo { name: "u256",  byte_size: 32, category: Category::Int },
    TypeInfo { name: "f32",   byte_size: 4,  category: Category::Float },
    TypeInfo { name: "f64",   byte_size: 8,  category: Category::Float },
];

impl NodeOutputType {
    #[inline]
    fn info(self) -> &'static TypeInfo { &TYPE_INFO[self as usize] }
    #[inline] pub fn as_str(self)     -> &'static str { self.info().name }
    #[inline] pub fn byte_size(self)  -> usize       { self.info().byte_size as usize }
    #[inline] pub fn bit_width(self)  -> usize       { self.byte_size() * 8 }
    #[inline] pub fn is_bool(self)    -> bool        { matches!(self.info().category, Category::Bool) }
    #[inline] pub fn is_integer(self) -> bool        { matches!(self.info().category, Category::Int) }
    #[inline] pub fn is_float(self)   -> bool        { matches!(self.info().category, Category::Float) }
}
```

`get_unsigned_int`, `get_signed_int`, and `to_natural_int_type` retain their
match bodies (they do per-variant bit-fiddling that doesn't table-drive
naturally; each match is small and not repeated elsewhere).

A debug-assert is added in tests to prove the table order matches the variant
discriminant order.

### Savings

~100 LOC from eliminated per-method 9-arm matches.

## Phase 2 — `rewrite_rules!` proc-macro DSL

New proc-macro `rewrite_rules!` in `crates/ir-macros/src/lib.rs` (alongside the
existing `match_value!`). `opt/constant_fold.rs` is rewritten to use it.

### Syntax

```rust
use ir_macros::rewrite_rules;
use ir::IntBinaryOp::*;

rewrite_rules! {
    // Identity / absorption — operators on NodeOutputId build graph shape.
    (x + IntConst(0))                   => x,
    (x - IntConst(0))                   => x,
    (x - x)                             => int_const(0, ty),
    (x ^ x)                             => int_const(0, ty),
    (x * IntConst(0))                   => int_const(0, ty),
    (x * IntConst(1))                   => x,
    (x & IntConst(0))                   => int_const(0, ty),

    // Deep nested matching.
    ((a & IntConst(c1)) & IntConst(c2))
        => a & int_const(c1 & c2, ty),
    ((a & IntConst(c1)) | (b & IntConst(c2))) & IntConst(c3)
        => (a & int_const(c1 & c3, ty)) | (b & int_const(c2 & c3, ty)),
    ((x + IntConst(c1)) + IntConst(c2))
        => x + int_const(c1.wrapping_add(c2), ty),

    // Full constant evaluation.
    (IntConst(l) + IntConst(r)) where ty.fits_u64()
        => int_const(l.wrapping_add(r), ty),
    (IntEq(IntConst(l), IntConst(r)))
        => bool_const(l == r),

    // Input-type introspection.
    Extend::<SignExtend>(IntConst(v) : in_ty)
        => int_const(in_ty.sign_extend(v), ty),

    // Bitcast identity.
    IntBitsToFloat(FloatBitsToInt(x))   => x,
    FloatBitsToInt(IntBitsToFloat(x))   => x,

    // Hand-written escape hatch for logic that doesn't fit the DSL.
    cast_to_float @ try_lower_cast_to_float,
}
```

### Grammar

| Feature | Syntax | Meaning |
|---|---|---|
| Output capture | bare identifier (`x`, `a`) | Binds a `NodeOutputId`. Same name twice = equality constraint. |
| Integer-const capture | `IntConst(c)` | Matches `NodeKind::IntConst` and binds its stored `u64`. |
| Bool-const capture | `BoolConst(b)` | Matches `NodeKind::BoolConst`, binds `bool`. |
| Float-const capture | `FloatConst(bits)` | Matches `NodeKind::FloatConst`, binds raw bits as `u64`. |
| Int binary ops | `a + b`, `a - b`, `a * b`, `a / b`, `a & b`, `a | b`, `a ^ b`, `a << b`, `a >> b` | `NodeKind::IntBinaryOp(op)` exclusively. Commutative ops (`+`, `*`, `&`, `|`, `^`) auto-try both orderings; non-commutative ops (`-`, `/`, `<<`, `>>`) match in stated order. |
| Bool binary ops | `BAnd(a, b)`, `BOr(a, b)`, `BXor(a, b)` | `NodeKind::BoolBinaryOp(op)`. Function-style to disambiguate from int operators. All commutative. |
| Float binary ops | `FAdd(a, b)`, `FSub(a, b)`, `FMul(a, b)`, `FDiv(a, b)` | `NodeKind::FloatBinaryOp(op)`. Function-style. `FAdd`/`FMul` commutative. |
| Int comparisons | `IntEq(l, r)`, `IntLt(l, r)`, `IntLe(l, r)`, `IntSlt(l, r)`, `IntSle(l, r)`, `IntCarry(l, r)`, `IntBorrow(l, r)`, `IntScarry(l, r)`, `IntSborrow(l, r)` | `NodeKind::IntCmpOp(op)`. Maps directly to `IntCmpOp` variants. `IntEq` commutative; others ordered. |
| Float comparisons | `FEq(l, r)`, `FNe(l, r)`, `FLt(l, r)`, `FLe(l, r)` | `NodeKind::FloatCmpOp(op)`. `FEq`/`FNe` commutative. |
| Other kinds | `Popcount(x)`, `Lzcount(x)`, `Truncate(x)`, `Extend::<ExtKind>(x)`, `Piece(hi, lo)`, `Extract::<lsb, len>(x)`, etc. | Direct NodeKind shape match. |
| Input-type binding | `IntConst(c) : in_ty` | Additionally binds the capture's output type as `in_ty: NodeOutputType`. |
| Where clause | `where <expr>` | Bool expression run after matching; scope includes all captures, `ty`, `fg`. |
| Escape hatch | `name @ fn_name` | Calls `fn_name(fg, node) -> Result<OptimizationResult>` as part of dispatch. |

### RHS expression context

In scope:
- `ty: NodeOutputType` — output type of the matched root node.
- `fg: &mut BuiltFunctionGraph`.
- All captures from the LHS with their native types (`NodeOutputId` or `u64`
  or `bool`).
- Builder functions: `int_const(val: u64, ty) -> NodeOutputId`,
  `bool_const(b: bool) -> NodeOutputId`, `float_const(bits, ty)`.

Operators on `NodeOutputId`-typed subexpressions build new graph nodes. The
macro decides per-subexpression by tracking whether each identifier was bound
as a value or as an output. A subexpression whose operands are all
value-bound identifiers emits plain Rust (`c1 & c3` → `u64` AND). A
subexpression containing any `NodeOutputId` emits a `make_value_node` call.

### Rule ordering semantics

Within a `rewrite_rules! { ... }` block, rules are tried in **declaration
order** against each node. The first rule whose LHS matches and whose `where`
clause passes fires and rewrites; remaining rules are not tried on that node
during this iteration. The pass runs inside `OptimizerPipeline`'s shared
fixed-point loop, so rewrites cause re-iteration until no rule fires.

Recommended ordering, from first to last:

1. **Full constant evaluation** (`(IntConst(l) + IntConst(r)) => int_const(...)`).
   These produce simpler nodes that enable downstream rewrites.
2. **Algebraic identities** (`x + 0 => x`, `x ^ x => 0`).
3. **Reassociation and mask merging** (`(x + c1) + c2 => x + (c1+c2)`).
4. **Escape-hatch helpers** (`name @ fn_name`).

### Expansion (conceptual)

For each rule, the macro emits a function
`fn rule_<N>(fg: &mut BuiltFunctionGraph, node: NodeId) -> Result<OptimizationResult>`
that:

1. Extracts the node's kind, inputs, and single output.
2. Runs the LHS match using nested `match_value!`-style logic (the macro
   shares expansion helpers with `match_value!` but does not extend its
   public surface).
3. Evaluates the `where` clause if present.
4. Evaluates the RHS, producing a `NodeOutputId`.
5. Calls `fg.replace_all_uses(matched_out, new_out)?` and returns `Changed`.

The top-level `ConstantFold` optimizer iterates nodes and calls each generated
rule function plus each `name @ fn_name` helper in order, `|=`-combining the
results.

### Out-of-scope in v1

- Cast-to-float lowering (`try_lower_cast_to_float`) — produces different
  node kinds based on input/output type pair. Stays as hand-written helper,
  wired in via `cast_to_float @ try_lower_cast_to_float`.

All other rewrites in `constant_fold.rs` become rules (add/sub
reassociation, bitcast identity, extend/truncate, AND-mask merging, the
`((a & c1) | (b & c2)) & c3` distribution example).

### Testing

- **Macro expansion tests** in `crates/ir-macros/tests/rewrite_rules.rs`:
  - `trybuild` ok cases for each grammar feature (bare capture, const
    capture, nested kinds, `: in_ty` suffix, `where` clause, commutativity,
    RHS node-builder, RHS value-builder, escape hatch).
  - `trybuild` err cases for common mistakes (undeclared identifier on RHS,
    value vs output misuse, bad node-kind name).
- **Rule tests** — every existing test in `opt/src/constant_fold.rs::tests`
  keeps passing unchanged. These are the black-box contract for behaviour.
- **New rule tests** for rules added during the refactor (e.g. the
  distribution example).

### Risks

- **Proc-macro complexity.** Estimated 600-900 LOC in `ir-macros`. This is
  the largest single piece of new code in the refactor.
- **Error messages.** Proc-macros produce notoriously bad errors. Invest in
  span tracking at each grammar position and hand-written "expected X, found
  Y" diagnostics.
- **Spike first.** Before committing to the full rewrite, a half-day spike
  implements end-to-end expansion for three representative rules:
  `(x + IntConst(0)) => x`, `((a & IntConst(c1)) & IntConst(c2)) => a & int_const(c1 & c2, ty)`,
  and `Extend::<SignExtend>(IntConst(v) : in_ty) => int_const(in_ty.sign_extend(v), ty)`.
  If any of these hit a wall, the macro scope is re-planned before writing
  the full grammar.

### Savings

`opt/src/constant_fold.rs` drops from ~1000 lines of non-test code to ~100
(the rule table + the one escape-hatch function). Net workspace LOC
approximately break-even once the macro is included, but the readability
improvement is the point.

## Phase 3 — `pattern` crate unification

Two internal refactors in `crates/pattern/`. No public API changes.

### 3.1 `IntoPat` trait

Every builder struct in `pat.rs` (`IntBinaryOpPat`, `BoolBinaryOpPat`,
`FloatBinaryOpPat`, `LoadPat`, `StorePat`, `StackStorePat`, `StackStorePhiPat`,
`PhiPat`, `CallPat`, `CallOtherPat`, `RetPat`, `IfPat` — twelve total)
currently defines identical `capture` and `when` methods that delegate to
`Pat::from(self).capture(v)` / `Pat::from(self).when(f)`.

Replace with one blanket-impl trait:

```rust
pub trait IntoPat: Into<Pat> + Sized {
    fn capture(self, v: Var) -> Pat { self.into().capture(v) }
    fn when<F>(self, f: F) -> Pat
    where
        F: Fn(&BuiltFunctionGraph, NodeOutputId) -> bool + Send + Sync + 'static,
    {
        self.into().when(f)
    }
}
impl<T: Into<Pat>> IntoPat for T {}
```

Every builder automatically gains `.capture()` and `.when()`. The per-struct
method bodies are deleted (~24 methods, ~100 LOC).

### 3.2 Matcher unary-arm consolidation

In `matcher.rs`, six unary-input patterns have byte-identical match bodies:
`CastToBool`, `CastToInt`, `Truncate`, `Popcount`, `Lzcount`, `Extend`.
Similarly, several two-input, no-commutativity patterns share a body:
`Piece`, `IntCmpOp`, `Insert`, `FloatCmpOp`.

Extract two helpers:

```rust
impl<'g> Matcher<'g> {
    fn match_unary_op<F>(
        &self,
        node: NodeId,
        operand: &Pat,
        bindings: &mut Bindings,
        kind_ok: F,
    ) -> bool
    where F: FnOnce(&NodeKind) -> bool;

    fn match_binary_op<F>(
        &self,
        node: NodeId,
        lhs: &Pat,
        rhs: &Pat,
        bindings: &mut Bindings,
        kind_ok: F,
    ) -> bool
    where F: FnOnce(&NodeKind) -> bool;
}
```

Each unary/binary arm becomes a one-liner:

```rust
PatKind::CastToBool { operand } =>
    self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::CastToBool)),
PatKind::Truncate { operand } =>
    self.match_unary_op(node, operand, bindings, |k| matches!(k, NodeKind::Truncate)),
PatKind::Piece { hi, lo } =>
    self.match_binary_op(node, hi, lo, bindings, |k| matches!(k, NodeKind::Piece)),
```

### Out-of-scope

- The `PatKind` enum itself — each variant has genuinely different fields.
- The free-function constructors (`add`, `load`, `call`, …) — already
  one-liners forming the user-facing API.
- Backtracking / `Bindings` logic.
- The `match_value!` macro — not touched.

### Testing

Existing tests in `crates/pattern/tests/matching.rs` (2655 lines) pass
unchanged — they are the behavioural oracle.

### Savings

`pat.rs` ~150 LOC, `matcher.rs` ~200 LOC.

## Phase 4 — `opt/known_bits.rs` and remaining passes clean-up

After Phase 0 and Phase 2, the remaining passes (`known_bits`, `dead_branch`,
`redundant_phis`, `load_readonly`, `stack_store`) benefit from the
centralised helpers but do not warrant DSL treatment — each is an
analysis/dataflow pass with a distinct shape.

Work in this phase:

- Replace all remaining `as_value().ok_or(ExpectedValueOutput(...))` chains
  across `known_bits.rs` / `dead_branch.rs` / `redundant_phis.rs` /
  `load_readonly.rs` / `stack_store.rs` with `as_value_or_err()`.
- Replace phi-kind matches with `NodeKind::is_phi()`.

(`opt::utils` is already deleted in Phase 0; `opt/constant_fold.rs` is already
rewritten in Phase 2.)

No new helpers, no new macros. This phase is mechanical clean-up that finishes
the consolidation started in Phase 0.

### Savings

~40 LOC across passes; `opt/utils.rs` deleted.

## Future improvements (not in this refactor)

Called out here so they are not lost, but explicitly out of scope:

- **`ir/src/dot.rs`** has three per-NodeKind dispatch functions (`node_shape`,
  `node_fillcolor`, `pretty_label`) totalling ~160 `NodeKind::` match arms.
  These could be unified into a single `NodeKindMetadata` table (shape,
  fillcolor, label-template per variant). Deferred: rendering data is
  inherently per-variant, current code is readable, and `NodeKind` changes
  infrequently.
- **`ir/src/builder.rs`** `build_*` methods could be unified behind generics,
  but the current per-shape methods are already short (~5-10 LOC each) and
  have clearer call-sites than a generic alternative.
- **`ir/src/validate.rs`** layer functions have outer-loop similarity but
  genuinely different inner work; not worth a visitor abstraction.

## Testing strategy (summary)

- Every existing test in the workspace continues to pass unchanged at every
  phase boundary. No test deletions during the refactor.
- Phase 2 adds `ir-macros/tests/rewrite_rules.rs` (expansion and err tests)
  plus end-to-end rule tests for rules added or newly expressible during the
  refactor.
- End-to-end validation on `cargo run --example analyzer` after each phase.
  The generated `cfg.html` / `graph.html` for the workspace's test binary
  (`binary_tests/binary_test`) must be *semantically equivalent* to the
  pre-refactor baseline — same number of nodes of each kind, same edge
  structure. Exact byte-identity is not required because node-id allocation
  can shift when helper call-order changes. A baseline is captured before
  Phase 0 begins; a small comparison script (created in a new top-level
  `scripts/` directory) lives there for the duration of the refactor and is
  removed afterwards.

## Rollout

- One commit per phase minimum; split larger phases by sub-section if the
  diff exceeds ~500 LOC.
- Phase 2 lands after a spike (see Phase 2 Risks). If the spike reveals
  unresolved grammar or expansion issues, Phase 2 is re-scoped (e.g. fewer
  rules expressed in the DSL, more hand-written helpers) before committing
  to the full rewrite. Phases 0, 1, 3, 4 are unaffected.
- Phase 3 is independent of Phase 2 and may land out of order if Phase 2 is
  blocked.
