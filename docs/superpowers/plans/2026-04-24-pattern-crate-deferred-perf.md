# Pattern Crate Deferred Performance Work — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the two hottest sources of wasted work in the pattern-matching engine: per-snapshot `Bindings` clone cost (13 HashMap allocations per backtrack) and per-candidate-node dispatch cost (every node in the preorder walk, even those of the wrong kind).

**Architecture:** Two independent refactors, landed as separate phases.
Phase 1 replaces 13 `HashMap<K, V>` fields with a single append-only `Vec<BindingEntry>` and a `mark()` / `restore()` API — backtracking becomes `Vec::truncate` with no allocations.
Phase 2 adds a structural `KindFilter` hint on every `NodePat` exposing the root NodeKind discriminant it accepts, wired through the `Pattern` trait so `Matcher::find_all` skips candidate nodes whose kind is incompatible and `try_match_common` short-circuits before any work.

**Tech Stack:** Rust 2024 edition. No new dependencies. Uses `std::mem::Discriminant<NodeKind>` (stable, `Copy + PartialEq`).

---

## File Structure

### Phase 1 (journal bindings)

**Modified:**
- [`crates/pattern/src/matcher/bindings.rs`](crates/pattern/src/matcher/bindings.rs) — replace 13 HashMaps with `Vec<BindingEntry>`; add `BindingsMark` + `mark()` / `restore()`; rewrite `bind_*` / `get_*` as linear scans.
- [`crates/pattern/src/pat/any.rs`](crates/pattern/src/pat/any.rs) — `CapturePat::try_match` uses `mark`/`restore`.
- [`crates/pattern/src/pat/guards.rs`](crates/pattern/src/pat/guards.rs) — `WhenPat` / `WhenMatchPat` use `mark`/`restore`.
- [`crates/pattern/src/pat/node_pat.rs`](crates/pattern/src/pat/node_pat.rs) — `try_match_common` and the `try_match_node` override use `mark`/`restore`.
- [`crates/pattern/src/pat/traits.rs`](crates/pattern/src/pat/traits.rs) — default `try_match_node` uses `mark`/`restore`.

### Phase 2 (KindFilter hint)

**Modified:**
- [`crates/pattern/src/pat/node_pat.rs`](crates/pattern/src/pat/node_pat.rs) — add `KindFilter` enum, add `root_kind` field to `NodePat`, update constructor signature, add `accepts()` fast-path in `try_match_common`.
- [`crates/pattern/src/pat/traits.rs`](crates/pattern/src/pat/traits.rs) — add `root_kind_filter()` method to `Pattern` trait (default `Any`).
- [`crates/pattern/src/pat/any.rs`](crates/pattern/src/pat/any.rs), [`crates/pattern/src/pat/guards.rs`](crates/pattern/src/pat/guards.rs) — `CapturePat`/`WhenPat`/`WhenMatchPat` forward `root_kind_filter` to inner.
- [`crates/pattern/src/matcher/mod.rs`](crates/pattern/src/matcher/mod.rs) — `find_all` applies the filter in the preorder loop.
- Every ctor file ([`crates/pattern/src/pat/builders.rs`](crates/pattern/src/pat/builders.rs), [`crates/pattern/src/pat/ctor/casts.rs`](crates/pattern/src/pat/ctor/casts.rs), [`crates/pattern/src/pat/ctor/int.rs`](crates/pattern/src/pat/ctor/int.rs), [`crates/pattern/src/pat/ctor/bool_.rs`](crates/pattern/src/pat/ctor/bool_.rs), [`crates/pattern/src/pat/ctor/float.rs`](crates/pattern/src/pat/ctor/float.rs), [`crates/pattern/src/pat/ctor/control.rs`](crates/pattern/src/pat/ctor/control.rs), [`crates/pattern/src/pat/ctor/wildcards.rs`](crates/pattern/src/pat/ctor/wildcards.rs), [`crates/pattern/src/pat/ctor/consts.rs`](crates/pattern/src/pat/ctor/consts.rs), [`crates/pattern/src/pat/ctor/variant_agnostic.rs`](crates/pattern/src/pat/ctor/variant_agnostic.rs), [`crates/pattern/src/pat/mod.rs`](crates/pattern/src/pat/mod.rs)) — pass the expected NodeKind discriminant to `NodePat::matcher`.

---

## Phase 1: Journal-based `Bindings`

### Task 1: Restructure `Bindings` storage

**Files:**
- Modify: [`crates/pattern/src/matcher/bindings.rs`](crates/pattern/src/matcher/bindings.rs) (full rewrite)

- [ ] **Step 1: Replace the struct body with `Vec<BindingEntry>` + enum**

Replace the current 13-HashMap definition with:

```rust
#[derive(Clone, Default)]
pub struct Bindings {
    entries: Vec<BindingEntry>,
}

#[derive(Clone, Copy)]
enum BindingEntry {
    Var(Var, NodeOutputId),
    NodeVar(NodeVar, NodeId),
    Int(IntVar, u64),
    Bool(BoolVar, bool),
    Float(FloatVar, u64),
    IntBinaryOp(IntBinaryOpVar, IntBinaryOp),
    IntUnaryOp(IntUnaryOpVar, IntUnaryOp),
    IntCmpOp(IntCmpOpVar, IntCmpOp),
    BoolBinaryOp(BoolBinaryOpVar, BoolBinaryOp),
    BoolUnaryOp(BoolUnaryOpVar, BoolUnaryOp),
    FloatBinaryOp(FloatBinaryOpVar, FloatBinaryOp),
    FloatUnaryOp(FloatUnaryOpVar, FloatUnaryOp),
    FloatCmpOp(FloatCmpOpVar, FloatCmpOp),
}

/// Opaque marker returned by [`Bindings::mark`] and consumed by
/// [`Bindings::restore`].  Represents "the binding state at the moment
/// of marking"; rolling back discards entries appended after the mark.
#[derive(Clone, Copy)]
pub struct BindingsMark(usize);
```

- [ ] **Step 2: Add `mark()` / `restore()`**

```rust
impl Bindings {
    /// Snapshot the current state in O(1) with no allocations.
    /// Use with [`Self::restore`] to roll back failed match attempts.
    pub(crate) fn mark(&self) -> BindingsMark {
        BindingsMark(self.entries.len())
    }

    /// Discard every entry appended after `mark` was taken.
    pub(crate) fn restore(&mut self, mark: BindingsMark) {
        self.entries.truncate(mark.0);
    }
}
```

- [ ] **Step 3: Rewrite `bind_var` / `bind_node_var` as linear scans**

```rust
impl Bindings {
    pub(crate) fn bind_var(&mut self, v: Var, out: NodeOutputId) -> bool {
        for entry in &self.entries {
            if let BindingEntry::Var(k, existing) = entry
                && *k == v
            {
                return *existing == out;
            }
        }
        self.entries.push(BindingEntry::Var(v, out));
        true
    }

    pub(crate) fn bind_node_var(&mut self, nv: NodeVar, node: NodeId) -> bool {
        for entry in &self.entries {
            if let BindingEntry::NodeVar(k, existing) = entry
                && *k == nv
            {
                return *existing == node;
            }
        }
        self.entries.push(BindingEntry::NodeVar(nv, node));
        true
    }

    pub fn get(&self, v: Var) -> Option<NodeOutputId> {
        for entry in &self.entries {
            if let BindingEntry::Var(k, val) = entry
                && *k == v
            {
                return Some(*val);
            }
        }
        None
    }

    pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> {
        for entry in &self.entries {
            if let BindingEntry::NodeVar(k, val) = entry
                && *k == nv
            {
                return Some(*val);
            }
        }
        None
    }
}
```

- [ ] **Step 4: Replace the existing `bind()` helper + `decl_bind_get!` macro with a journal-based equivalent**

Keep the `decl_bind_get!` macro shape, but expand to linear-scan bodies that filter by a user-supplied variant constructor/matcher. New macro:

```rust
macro_rules! decl_bind_get {
    ($variant:ident, $bind_name:ident, $get_name:ident, $var:ty, $val:ty, $doc_stem:literal) => {
        impl Bindings {
            #[doc = concat!("Bind `v` to ", $doc_stem, ".\n\nReturns `true` on new or idempotent binding, `false` on conflict (no mutation).")]
            pub fn $bind_name(&mut self, v: $var, val: $val) -> bool {
                for entry in &self.entries {
                    if let BindingEntry::$variant(k, existing) = entry
                        && *k == v
                    {
                        return *existing == val;
                    }
                }
                self.entries.push(BindingEntry::$variant(v, val));
                true
            }

            #[doc = concat!("Returns the ", $doc_stem, " bound to `v`, or `None` if unbound.")]
            pub fn $get_name(&self, v: $var) -> Option<$val> {
                for entry in &self.entries {
                    if let BindingEntry::$variant(k, val) = entry
                        && *k == v
                    {
                        return Some(*val);
                    }
                }
                None
            }
        }
    };
}

decl_bind_get!(Int,            bind_int,             get_int,             IntVar,          u64,          "the integer constant value");
decl_bind_get!(Bool,           bind_bool,            get_bool,            BoolVar,         bool,         "the boolean constant value");
decl_bind_get!(Float,          bind_float,           get_float_bits,      FloatVar,        u64,          "the float constant IEEE 754 bit pattern");
decl_bind_get!(IntBinaryOp,    bind_int_binary_op,   get_int_binary_op,   IntBinaryOpVar,  IntBinaryOp,  "the [`IntBinaryOp`] variant");
decl_bind_get!(IntUnaryOp,     bind_int_unary_op,    get_int_unary_op,    IntUnaryOpVar,   IntUnaryOp,   "the [`IntUnaryOp`] variant");
decl_bind_get!(IntCmpOp,       bind_int_cmp_op,      get_int_cmp_op,      IntCmpOpVar,     IntCmpOp,     "the [`IntCmpOp`] variant");
decl_bind_get!(BoolBinaryOp,   bind_bool_binary_op,  get_bool_binary_op,  BoolBinaryOpVar, BoolBinaryOp, "the [`BoolBinaryOp`] variant");
decl_bind_get!(BoolUnaryOp,    bind_bool_unary_op,   get_bool_unary_op,   BoolUnaryOpVar,  BoolUnaryOp,  "the [`BoolUnaryOp`] variant");
decl_bind_get!(FloatBinaryOp,  bind_float_binary_op, get_float_binary_op, FloatBinaryOpVar,FloatBinaryOp,"the [`FloatBinaryOp`] variant");
decl_bind_get!(FloatUnaryOp,   bind_float_unary_op,  get_float_unary_op,  FloatUnaryOpVar, FloatUnaryOp, "the [`FloatUnaryOp`] variant");
decl_bind_get!(FloatCmpOp,     bind_float_cmp_op,    get_float_cmp_op,    FloatCmpOpVar,   FloatCmpOp,   "the [`FloatCmpOp`] variant");
```

Remove the old `fn bind<K, V>(...)` helper (it's no longer used).

- [ ] **Step 5: Update the `Bindings` doc comment**

Replace the "Follow-up: journal-based backtracking" block with actual description of how the journal works. Suggested:

```rust
/// A set of capture-variable bindings accumulated during a single match attempt.
///
/// Bindings are append-only: once a variable is bound it cannot be rebound to a
/// different value.  A mismatch (trying to bind an already-bound variable to a
/// different value) makes the containing match fail.
///
/// Backtracking uses a journal-based scheme: every match site that wants to
/// speculatively attempt sub-matches calls [`Self::mark`] before the attempt
/// and [`Self::restore`] on failure — the marker is a `usize` cursor into the
/// append-only entry Vec, and restoring is an O(1) `Vec::truncate`.  No
/// allocations, no per-kind HashMap clones, no deep copy of the full state.
///
/// Lookups (`get_*`) are linear scans filtered by entry variant.  Typical
/// matches produce ≤10 bindings so scan cost is in-cache and beats HashMap
/// on cold-path inserts.  If a future pattern produces dozens of bindings,
/// a lazy overlay index can be added without changing the API.
```

- [ ] **Step 6: Run matcher tests to verify bindings tests still pass**

Run: `cargo test -p pattern --lib matcher::tests 2>&1 | tail -20`
Expected:
```
test matcher::tests::int_var_bind_and_get ... ok
test matcher::tests::int_var_idempotent_rebind ... ok
test matcher::tests::int_var_conflict_fails ... ok
test matcher::tests::bool_var_bind_and_get ... ok
test matcher::tests::bool_var_idempotent_rebind ... ok
test matcher::tests::bool_var_conflict_fails ... ok
test matcher::tests::float_var_bind_and_get ... ok
test matcher::tests::float_var_idempotent_rebind ... ok
test matcher::tests::float_var_conflict_fails ... ok
test matcher::tests::dyn_pat_dispatch_routes_via_data_pattern ... ok
test matcher::tests::capture_ids_are_globally_unique ... ok

test result: ok. 13 passed; 0 failed; …
```

- [ ] **Step 7: Run the full pattern suite**

Run: `cargo test -p pattern 2>&1 | grep -E "test result"`
Expected: `test result: ok. 188 passed; 0 failed; …` (plus the 13-pass lib result from Step 6).

- [ ] **Step 8: Commit**

```bash
git add crates/pattern/src/matcher/bindings.rs
git commit -m "Journal-based Bindings: replace 13 HashMaps with Vec<BindingEntry>

Bindings was a struct of 13 separately-allocated HashMaps. Every
backtrack snapshot (NodePat::try_match_common, CapturePat, WhenPat,
WhenMatchPat, default try_match_node) did Bindings::clone(), cloning
13 HashMap headers. Even empty hashbrown maps aren't free to clone.

Replace with a single append-only Vec<BindingEntry> where BindingEntry
is a 13-variant discriminated union. Storage is linear-scanned by
variant — typical matches have <10 captures so in-cache scan beats
HashMap. All public bind_* / get_* signatures are unchanged. Sites
that take snapshots still use Bindings::clone() in this commit — the
next commit migrates them to the new mark/restore API."
```

---

### Task 2: Migrate snap/restore call sites to `mark`/`restore`

**Files:**
- Modify: [`crates/pattern/src/pat/any.rs`](crates/pattern/src/pat/any.rs)
- Modify: [`crates/pattern/src/pat/guards.rs`](crates/pattern/src/pat/guards.rs)
- Modify: [`crates/pattern/src/pat/node_pat.rs`](crates/pattern/src/pat/node_pat.rs)
- Modify: [`crates/pattern/src/pat/traits.rs`](crates/pattern/src/pat/traits.rs)

- [ ] **Step 1: Migrate `CapturePat::try_match` in [`pat/any.rs`](crates/pattern/src/pat/any.rs)**

Find the block:

```rust
impl Pattern for CapturePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let snap = b.clone();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            return false;
        }
        if b.bind_var(self.var, target) {
            true
        } else {
            *b = snap;
            false
        }
    }
```

Replace with:

```rust
impl Pattern for CapturePat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let mark = b.mark();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            // `match_output` may have appended partial state before failing;
            // restore to pre-attempt state so failure is observer-clean.
            b.restore(mark);
            return false;
        }
        if b.bind_var(self.var, target) {
            true
        } else {
            b.restore(mark);
            false
        }
    }
```

Note: the original code did NOT restore on the inner-fail path — it returned `false` without `*b = snap`. That was correct only if the inner match itself rolled back on failure (which, pre-journal, was the contract). With the journal scheme every pattern must roll back its own speculation; adding the `b.restore(mark)` on the inner-fail path is the journal equivalent of the prior inner-pattern's self-restoration. Double restoration is safe — `truncate` is idempotent for the same marker.

- [ ] **Step 2: Migrate `WhenPat::try_match` and `WhenMatchPat::try_match` in [`pat/guards.rs`](crates/pattern/src/pat/guards.rs)**

Replace the `WhenPat` body:

```rust
impl Pattern for WhenPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let mark = b.mark();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            b.restore(mark);
            return false;
        }
        let Some(out_ty) = ctx.graph.graph.output_kind(target).as_value() else {
            b.restore(mark);
            return false;
        };
        if (self.func)(ctx.graph, out_ty, target) {
            true
        } else {
            b.restore(mark);
            false
        }
    }
}
```

Replace the `WhenMatchPat` body the same way, preserving its distinct signature (`&self.bindings` argument to the predicate).

- [ ] **Step 3: Migrate `NodePat::try_match_common` and `try_match_node` override in [`pat/node_pat.rs`](crates/pattern/src/pat/node_pat.rs)**

The `try_match_common` body currently reads:

```rust
if !(self.kind_match)(ctx, node, b) {
    return false;
}
let after_kind = b.clone();
if try_once(self, ctx, node, target, b, false) {
    return true;
}
if let InputsSpec::Fixed { pats, commutative } = &self.inputs
    && pats.len() == 2
    && commutative(ctx, node)
{
    *b = after_kind.clone();
    if try_once(self, ctx, node, target, b, true) {
        return true;
    }
}
*b = after_kind;
false
```

Rewrite as:

```rust
if !(self.kind_match)(ctx, node, b) {
    return false;
}
let after_kind = b.mark();
if try_once(self, ctx, node, target, b, false) {
    return true;
}
if let InputsSpec::Fixed { pats, commutative } = &self.inputs
    && pats.len() == 2
    && commutative(ctx, node)
{
    b.restore(after_kind);
    if try_once(self, ctx, node, target, b, true) {
        return true;
    }
}
b.restore(after_kind);
false
```

`BindingsMark` is `Copy`, so reusing `after_kind` for both the retry restore and the final rollback needs no clone.

Find `try_match_node` override (below `try_match_common`, iterates outputs):

```rust
for out in outputs.into_iter() {
    let snap = b.clone();
    if self.try_match(ctx, out, b) {
        return true;
    }
    *b = snap;
}
```

Replace:

```rust
for out in outputs.into_iter() {
    let mark = b.mark();
    if self.try_match(ctx, out, b) {
        return true;
    }
    b.restore(mark);
}
```

- [ ] **Step 4: Migrate default `Pattern::try_match_node` in [`pat/traits.rs`](crates/pattern/src/pat/traits.rs)**

Same change in the default impl (lines ~50-59):

```rust
fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
    for out in ctx.graph.graph.node_outputs(node).into_iter() {
        let mark = b.mark();
        if self.try_match(ctx, out, b) {
            return true;
        }
        b.restore(mark);
    }
    false
}
```

- [ ] **Step 5: Verify no stale `.clone()` / `*b =` patterns remain**

Run: `grep -rn "let snap = b.clone\|\*b = snap\|after_kind = b.clone\|\*b = after_kind" crates/pattern/src/`
Expected output: empty (no matches).

- [ ] **Step 6: Run the full pattern suite**

Run: `cargo test -p pattern 2>&1 | grep -E "test result"`
Expected: `test result: ok. 188 passed; 0 failed` (three suite results, same counts as before).

- [ ] **Step 7: Run the full workspace test**

Run: `cargo test --workspace 2>&1 | grep -E "FAILED|test result: FAILED" | head -5`
Expected: no output (no failures).

- [ ] **Step 8: Run clippy**

Run: `cargo clippy --workspace -p pattern 2>&1 | tail -5`
Expected: `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in …s` with no warnings from the pattern crate.

- [ ] **Step 9: Commit**

```bash
git add crates/pattern/src/pat/any.rs crates/pattern/src/pat/guards.rs crates/pattern/src/pat/node_pat.rs crates/pattern/src/pat/traits.rs
git commit -m "Migrate snap/restore call sites to Bindings::mark/restore

Seven backtrack sites previously did \`let snap = b.clone(); … *b = snap;\`,
each cloning 13 HashMaps (pre-journal) or one Vec (post-journal). Replace
with \`let mark = b.mark(); … b.restore(mark);\` — mark is a Copy usize,
restore is an O(1) Vec::truncate. No more per-snapshot allocations on
the hot path.

Sites migrated: CapturePat, WhenPat, WhenMatchPat, NodePat::try_match_common
(with both commutative-retry restore and total-failure restore sharing one
mark), NodePat::try_match_node override, default Pattern::try_match_node."
```

---

## Phase 2: `KindFilter` hint + find_all prefilter

### Task 3: Define `KindFilter` + extend `NodePat`

**Files:**
- Modify: [`crates/pattern/src/pat/node_pat.rs`](crates/pattern/src/pat/node_pat.rs)

- [ ] **Step 1: Add `KindFilter` enum and `accepts` method**

Insert near the top of `node_pat.rs`, after the existing `use` block:

```rust
/// Fast-path root-kind hint on [`NodePat`].  `find_all` uses it to skip
/// candidate nodes whose `NodeKind` is incompatible with this pattern,
/// avoiding any per-candidate closure dispatch or clone work.
/// `try_match_common` also short-circuits on it, so `match_at` callers
/// (e.g. rewrite rules) still get the fast-fail path.
#[derive(Clone, Copy)]
pub enum KindFilter {
    /// Pattern can accept any NodeKind — the `kind_match` closure is
    /// the sole authority.  Used by wildcards (`AnyPat`, `VarPat`) and
    /// any future pattern that matches across unrelated kinds.
    Any,
    /// Pattern only accepts nodes whose discriminant matches the stored
    /// one.  Nearly every leaf NodePat falls here.
    Single(std::mem::Discriminant<NodeKind>),
}

impl KindFilter {
    #[inline]
    pub(crate) fn accepts(&self, kind: &NodeKind) -> bool {
        match self {
            Self::Any => true,
            Self::Single(d) => *d == std::mem::discriminant(kind),
        }
    }

    /// Convenience constructor: a filter that accepts exactly the
    /// discriminant of `exemplar`.
    #[inline]
    pub(crate) fn exact(exemplar: &NodeKind) -> Self {
        Self::Single(std::mem::discriminant(exemplar))
    }
}
```

- [ ] **Step 2: Add `root_kind` field to `NodePat` + update `matcher()` constructor**

In the `NodePat` struct definition, add the field (near the top, next to `kind_match`):

```rust
pub struct NodePat {
    /// Fast-path filter consulted before any clone or closure call.
    /// Every ctor sets this to the expected NodeKind discriminant; `Any`
    /// is reserved for patterns whose root kind genuinely varies (none
    /// exist today — see [`KindFilter`]).
    pub(crate) root_kind: KindFilter,
    pub(crate) kind_match: NodeKindCheck,
    // … existing fields unchanged …
}
```

Update the `matcher()` constructor signature to require the filter:

```rust
impl NodePat {
    pub(crate) fn matcher(
        root_kind: KindFilter,
        kind_match: NodeKindCheck,
        inputs: InputsSpec,
    ) -> Self {
        Self {
            root_kind,
            kind_match,
            kind_build: None,
            build_result_ty: BuildTy::InheritRoot,
            inputs,
            outputs: OutputsSpec::None,
            consumers: ConsumersSpec::None,
            post_match: None,
            output_var: None,
            node_var: None,
        }
    }
    // … rest unchanged …
}
```

- [ ] **Step 3: Short-circuit in `try_match_common`**

Add the filter check as the first line of `try_match_common`, before `kind_match`:

```rust
fn try_match_common(
    &self,
    ctx: &MatchCtx,
    node: NodeId,
    target: Option<NodeOutputId>,
    b: &mut Bindings,
) -> bool {
    if !self.root_kind.accepts(ctx.graph.graph.node_kind(node)) {
        return false;
    }
    if !(self.kind_match)(ctx, node, b) {
        return false;
    }
    // … rest unchanged …
}
```

- [ ] **Step 4: Run cargo check (expect compile errors at every ctor site)**

Run: `cargo check -p pattern 2>&1 | head -30`
Expected: errors of the form
```
error[E0061]: this function takes 3 arguments but 2 arguments were supplied
   --> crates/pattern/src/pat/ctor/...
```
across every `NodePat::matcher(...)` call site. This is expected — next tasks fix them.

---

### Task 4: Extend `Pattern` trait with `root_kind_filter`

**Files:**
- Modify: [`crates/pattern/src/pat/traits.rs`](crates/pattern/src/pat/traits.rs)
- Modify: [`crates/pattern/src/pat/node_pat.rs`](crates/pattern/src/pat/node_pat.rs)
- Modify: [`crates/pattern/src/pat/any.rs`](crates/pattern/src/pat/any.rs)
- Modify: [`crates/pattern/src/pat/guards.rs`](crates/pattern/src/pat/guards.rs)

- [ ] **Step 1: Add trait method with default**

In [`crates/pattern/src/pat/traits.rs`](crates/pattern/src/pat/traits.rs), add to the `Pattern` trait:

```rust
pub trait Pattern: Send + Sync {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool;

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        // … existing default …
    }

    /// Advertises the `NodeKind` discriminants this pattern can match at
    /// its root.  `Matcher::find_all` uses this to skip candidate nodes
    /// whose kind is incompatible, turning a graph-wide preorder scan
    /// into an effectively kind-indexed scan for patterns with a
    /// concrete root.
    ///
    /// Default: [`KindFilter::Any`] (no filtering).  Override on
    /// combinators by forwarding to the inner pattern, and on leaves
    /// whose root kind is statically known.
    fn root_kind_filter(&self) -> crate::pat::node_pat::KindFilter {
        crate::pat::node_pat::KindFilter::Any
    }

    fn try_build(&self, _ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        // … existing default …
    }
}
```

- [ ] **Step 2: Override on `NodePat`**

In [`crates/pattern/src/pat/node_pat.rs`](crates/pattern/src/pat/node_pat.rs), add to the `impl Pattern for NodePat` block:

```rust
impl Pattern for NodePat {
    // … existing methods …

    fn root_kind_filter(&self) -> KindFilter {
        self.root_kind
    }
}
```

- [ ] **Step 3: Forward on combinators**

In [`crates/pattern/src/pat/any.rs`](crates/pattern/src/pat/any.rs), add to `CapturePat`:

```rust
impl Pattern for CapturePat {
    // … existing try_match, try_build …

    fn root_kind_filter(&self) -> crate::pat::node_pat::KindFilter {
        self.inner.as_dyn().root_kind_filter()
    }
}
```

In [`crates/pattern/src/pat/guards.rs`](crates/pattern/src/pat/guards.rs), add the same forwarder to both `WhenPat` and `WhenMatchPat`:

```rust
impl Pattern for WhenPat {
    // … existing try_match …
    fn root_kind_filter(&self) -> crate::pat::node_pat::KindFilter {
        self.inner.as_dyn().root_kind_filter()
    }
}

impl Pattern for WhenMatchPat {
    // … existing try_match …
    fn root_kind_filter(&self) -> crate::pat::node_pat::KindFilter {
        self.inner.as_dyn().root_kind_filter()
    }
}
```

Note: `AnyPat` and `VarPat` use the default `Any` — no override needed.

- [ ] **Step 4: Verify compile errors are still limited to missing-arg at ctor sites**

Run: `cargo check -p pattern 2>&1 | grep -E "error\[" | head -10`
Expected: only `E0061` missing-argument errors at `NodePat::matcher` call sites — no other errors.

---

### Task 5: Migrate all ctor sites to supply `root_kind`

**Files:**
- Modify: [`crates/pattern/src/pat/builders.rs`](crates/pattern/src/pat/builders.rs)
- Modify: [`crates/pattern/src/pat/ctor/casts.rs`](crates/pattern/src/pat/ctor/casts.rs)
- Modify: [`crates/pattern/src/pat/ctor/int.rs`](crates/pattern/src/pat/ctor/int.rs)
- Modify: [`crates/pattern/src/pat/ctor/bool_.rs`](crates/pattern/src/pat/ctor/bool_.rs)
- Modify: [`crates/pattern/src/pat/ctor/float.rs`](crates/pattern/src/pat/ctor/float.rs)
- Modify: [`crates/pattern/src/pat/ctor/control.rs`](crates/pattern/src/pat/ctor/control.rs)
- Modify: [`crates/pattern/src/pat/ctor/wildcards.rs`](crates/pattern/src/pat/ctor/wildcards.rs)
- Modify: [`crates/pattern/src/pat/ctor/consts.rs`](crates/pattern/src/pat/ctor/consts.rs)
- Modify: [`crates/pattern/src/pat/ctor/variant_agnostic.rs`](crates/pattern/src/pat/ctor/variant_agnostic.rs)
- Modify: [`crates/pattern/src/pat/mod.rs`](crates/pattern/src/pat/mod.rs)

- [ ] **Step 1: Define family-convenience helpers at the top of [`node_pat.rs`](crates/pattern/src/pat/node_pat.rs)**

To keep ctor call sites terse, add module-level constants for the most-used discriminants. Since `Discriminant::<NodeKind>` isn't const-constructible, use a private module with `once_cell`-free thread-local-free helpers (the `std::mem::discriminant` function is const-fn-stable for `Copy` enums in recent Rust; if not available on the project's MSRV, use once-per-call inline functions):

```rust
/// Pre-built `KindFilter` values for every `NodeKind` variant a ctor
/// needs.  Grouped near the top of the file so ctor sites read as
/// `NodePat::matcher(KF::INT_BINARY_OP, …)`.
pub(crate) mod kf {
    use super::{KindFilter, NodeKind};

    // For data-carrying variants we build the filter from an
    // arbitrary exemplar value — discriminant ignores the payload.
    pub(crate) fn single(exemplar: &NodeKind) -> KindFilter {
        KindFilter::exact(exemplar)
    }
}
```

Use sites do: `KindFilter::exact(&NodeKind::IntBinaryOp(ir::IntBinaryOp::Add))` or the shorthand `kf::single(&NodeKind::IntBinaryOp(ir::IntBinaryOp::Add))`. Discriminant ignores the `Add` payload.

Decision: skip the `kf` module entirely, use `KindFilter::exact(&NodeKind::Foo(..))` inline. Simpler.

- [ ] **Step 2: Migrate [`builders.rs`](crates/pattern/src/pat/builders.rs)**

Every `NodePat::matcher(...)` call gains a first argument. Mappings:

- `IntBinaryOpPat` → `KindFilter::exact(&NodeKind::IntBinaryOp(op))`
- `BoolBinaryOpPat` → `KindFilter::exact(&NodeKind::BoolBinaryOp(op))`
- `FloatBinaryOpPat` → `KindFilter::exact(&NodeKind::FloatBinaryOp(op))`
- `LoadPat` → `KindFilter::exact(&NodeKind::Load(rsleigh::VnSpace::default()))` — need to confirm `VnSpace::default()` exists or use `space.unwrap_or(...)` / an arbitrary fixed `VnSpace`.

Wait — `rsleigh::VnSpace` may not implement `Default`. Verify this with `grep "impl Default for VnSpace\|pub struct VnSpace\|pub enum VnSpace" ../rsleigh/src/*.rs` during execution. If not, construct the filter at runtime using any stable VnSpace constant or use `std::mem::discriminant`-of-any-variant approach.

Fallback approach that sidesteps the `Default`/constructor issue: extract the discriminant from an arbitrary instance. Since all `NodeKind::Load(_)` share a discriminant regardless of the inner `VnSpace`, we can synthesize one by transmute — but that's unsafe. Instead, stash a `KindFilter::exact(&NodeKind::Load(dummy_vnspace))` where `dummy_vnspace` is whatever `VnSpace` the builder saw. Almost every builder already has a `space: Option<VnSpace>` field; if unset, the ctor requires the caller to pass a default somewhere, OR we use a helper:

```rust
/// Pre-built filter returning `Single(discriminant(&NodeKind::Load(...)))`
/// regardless of the VnSpace payload.  Discriminant depends only on the
/// variant, not the inner value.
fn load_filter() -> KindFilter {
    // SAFETY: discriminant ignores the payload, so we can construct a
    // temporary NodeKind with any valid VnSpace to extract it.
    KindFilter::exact(&NodeKind::Load(rsleigh::VnSpace::RAM))
    // (replace RAM with whatever constant is available on the project's
    // rsleigh version; verify via `grep 'pub.*VnSpace' ../rsleigh/src`)
}
```

Verify during Step 2 execution: run `grep -rn "VnSpace::" /home/mike/Desktop/strider/crates/ir/src/ | head -5` to find a known-valid VnSpace constructor. Use that to seed the filter.

For each binary-op `impl From<*Pat> for Pat` in [`builders.rs`](crates/pattern/src/pat/builders.rs), change:

```rust
NodePat::matcher(
    Arc::new(move |ctx, node, _b| {
        matches!(ctx.graph.graph.node_kind(node), NodeKind::IntBinaryOp(x) if *x == op)
    }),
    inputs,
)
```

to:

```rust
NodePat::matcher(
    KindFilter::exact(&NodeKind::IntBinaryOp(op)),
    Arc::new(move |ctx, node, _b| {
        matches!(ctx.graph.graph.node_kind(node), NodeKind::IntBinaryOp(x) if *x == op)
    }),
    inputs,
)
```

The `op` in the filter is a real value (captured by the builder) — discriminant still only distinguishes variants, so the specific `op` value in the filter doesn't matter. For `LoadPat`/`StorePat` etc., use any valid `VnSpace`. For `FunctionArgPat`: `NodeKind::FunctionArg { source: <any source>, index: 0 }` — discriminant only.

Full migration table (one row per `NodePat::matcher` call in `builders.rs`):

| Builder | First arg |
|---|---|
| `IntBinaryOpPat` | `KindFilter::exact(&NodeKind::IntBinaryOp(op))` |
| `BoolBinaryOpPat` | `KindFilter::exact(&NodeKind::BoolBinaryOp(op))` |
| `FloatBinaryOpPat` | `KindFilter::exact(&NodeKind::FloatBinaryOp(op))` |
| `LoadPat` | uses the `space` local if known; otherwise pick any valid `VnSpace` |
| `StorePat` | same — any valid `VnSpace` for the discriminant |
| `StackStorePat` | `KindFilter::exact(&NodeKind::StackStore { space: <any>, offset: 0 })` |
| `StackStorePhiPat` | `KindFilter::exact(&NodeKind::StackStorePhi { space: <any> })` |
| `PhiPat` | `KindFilter::exact(&NodeKind::ControlPhi(<any Vn>))` |
| `CallPat` | `KindFilter::exact(&NodeKind::Call)` |
| `CallOtherPat` | `KindFilter::exact(&NodeKind::CallOther { user_op_id: 0 })` |
| `RetPat` | `KindFilter::exact(&NodeKind::Return)` |
| `FunctionArgPat` | `KindFilter::exact(&NodeKind::FunctionArg { source: <any>, index: 0 })` |
| `IfPat` | `KindFilter::exact(&NodeKind::If)` |

- [ ] **Step 3: Migrate [`ctor/casts.rs`](crates/pattern/src/pat/ctor/casts.rs)**

The `simple_unary_cast!` macro's NodePat::matcher call:

```rust
unary_node(
    Arc::new(|ctx, node, _b| matches!(ctx.graph.graph.node_kind(node), NodeKind::$variant)),
    NodeKind::$variant,
    $build_ty,
    operand,
)
```

Update `unary_node` to take the filter too, and pass `KindFilter::exact(&build_kind)` (which for unit variants is the same as `KindFilter::exact(&NodeKind::$variant)`):

```rust
fn unary_node(
    kind_match: NodeKindCheck,
    build_kind: NodeKind,
    build_ty: BuildTy,
    operand: impl Into<Pat>,
) -> Pat {
    NodePat::matcher(
        KindFilter::exact(&build_kind),
        kind_match,
        InputsSpec::fixed_ordered(vec![operand.into()]),
    )
    .with_build(Arc::new(move |_b| Ok(build_kind)))
    .with_build_ty(build_ty)
    .into_pat()
}
```

For `extend(op, operand)`:

```rust
pub fn extend(op: ExtendOp, operand: impl Into<Pat>) -> Pat {
    unary_node(
        Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::Extend(actual) if *actual == op)
        }),
        NodeKind::Extend(op),
        BuildTy::InheritRoot,
        operand,
    )
}
```

No change — `unary_node` already derives the filter from `build_kind`.

- [ ] **Step 4: Migrate [`ctor/int.rs`](crates/pattern/src/pat/ctor/int.rs), [`ctor/bool_.rs`](crates/pattern/src/pat/ctor/bool_.rs), [`ctor/float.rs`](crates/pattern/src/pat/ctor/float.rs)**

Each has:
- One `int_unary`/`bool_unary`/`float_unary` function with a direct `NodePat::matcher` call — prepend `KindFilter::exact(&NodeKind::IntUnaryOp(op))` / etc.
- One `int_cmp`/`float_cmp` function — prepend `KindFilter::exact(&NodeKind::IntCmpOp(op))` / etc.
- In `float.rs`, the `unit_conv` helper — take the filter as an arg, like `unary_node` in `casts.rs`.

For `float.rs`'s `unit_conv`, refactor parameters to include the build-kind which is already an arg:

```rust
fn unit_conv(variant_match: impl Fn(&NodeKind) -> bool + Send + Sync + 'static,
             build_kind: NodeKind,
             operand: impl Into<Pat>) -> Pat {
    NodePat::matcher(
        KindFilter::exact(&build_kind),
        Arc::new(move |ctx, node, _b| variant_match(ctx.graph.graph.node_kind(node))),
        InputsSpec::fixed_ordered(vec![operand.into()]),
    )
    .with_build(Arc::new(move |_b| Ok(build_kind)))
    .into_pat()
}
```

- [ ] **Step 5: Migrate [`ctor/control.rs`](crates/pattern/src/pat/ctor/control.rs)**

`initial_var_impl` — add `KindFilter::exact(&NodeKind::InitialVar(<any Vn>))`. Requires a valid `rsleigh::Vn` constructor; if the IR exposes a synthetic one, use it, otherwise verify via `grep "pub const\|impl .*Vn" ../rsleigh/src/*.rs | head -10`.

If no synthetic constructor exists, add a private helper:

```rust
fn initial_var_filter() -> KindFilter {
    // Discriminant is payload-independent; the Vn passed here is never
    // stored or compared. If rsleigh exposes no constructor, fall back
    // to a closure-based KindFilter::Any and pay the filter cost at
    // match time — this path matches only InitialVar nodes via the
    // kind_match closure, but find_all skips are disabled for it.
    // TODO: once rsleigh exposes a Vn::dummy() constructor, switch to
    // KindFilter::Single(discriminant(&NodeKind::InitialVar(...))).
    KindFilter::Any
}
```

Prefer the real discriminant if possible — check `../rsleigh/src` for any public `Vn::ZERO` / `Vn::new` / `Default` impl.

- [ ] **Step 6: Migrate [`ctor/wildcards.rs`](crates/pattern/src/pat/ctor/wildcards.rs)**

- `int_const(v)` — `KindFilter::exact(&NodeKind::IntConst(v))` (any IntConst passes the filter; closure checks value).
- `bool_const(v)` — `KindFilter::exact(&NodeKind::BoolConst(v))`.
- `float_const(bits)` — `KindFilter::exact(&NodeKind::FloatConst(bits))`.

- [ ] **Step 7: Migrate [`ctor/consts.rs`](crates/pattern/src/pat/ctor/consts.rs)**

The `never_match_kind` const-with constructors are match-only-false (they never fire on LHS). `KindFilter::Any` keeps them match-only — no candidate node gets filtered out, but the closure returns false for every kind anyway. Equally correct to use `KindFilter::Single(…never-matching-discriminant…)`, but `Any` is simpler.

Update:

```rust
pub fn int_const_with_fn<F>(f: F) -> Pat { … }
// ↑ Change the NodePat::matcher first arg to KindFilter::Any.
```

Same for `bool_const_with_fn` and `float_const_with_fn`.

- [ ] **Step 8: Migrate [`ctor/variant_agnostic.rs`](crates/pattern/src/pat/ctor/variant_agnostic.rs)**

The `impl_variant_any!` macro's NodePat::matcher calls need the filter. The filter for `int_binary_any` is any IntBinaryOp — same discriminant as a concrete one, so `KindFilter::exact(&NodeKind::$op_enum(Default::default()))` works IF the op enums impl Default. Check each: `IntBinaryOp`, `IntUnaryOp`, etc.

If not, pick any concrete variant. Each op enum has at least one variant and the `$variant` token tree isn't available inside the macro — but we can sidestep by accepting a sample value through the macro invocation:

Revise macro:

```rust
macro_rules! impl_variant_any {
    (binary, $fn_name:ident, $op_enum:ident, $sample:expr, $op_var:ident, …) => {
        // Use KindFilter::exact(&NodeKind::$op_enum($sample)) in the NodePat::matcher call.
    };
}
```

And every invocation passes a sample:

```rust
impl_variant_any!(
    binary, int_binary_any, IntBinaryOp, ir::IntBinaryOp::Add, IntBinaryOpVar, …
);
```

- [ ] **Step 9: Migrate `IntoAny*Const` impls in [`pat/mod.rs`](crates/pattern/src/pat/mod.rs)**

The `decl_any_const!` macro's `NodePat::matcher(..)` call in both the `Var` and typed-var impls needs the filter. Use `KindFilter::exact(&NodeKind::$variant(Default::default()))` — requires IntConst/BoolConst/FloatConst to be `NodeKind::Foo(u64)` / `NodeKind::Foo(bool)` / `NodeKind::Foo(u64)`. A sample constant `0` / `false` / `0` works; discriminant ignores the payload. Pass the sample into the macro:

```rust
macro_rules! decl_any_const {
    (
        trait $trait:ident, sealed $sealed:ident, method $method:ident,
        variant $variant:ident, $sample:expr, $build_ty:expr,
        typed $typed:ty, $bind:ident, $get:ident, $missing:literal
    ) => { … };
}

decl_any_const!(
    trait IntoAnyIntConst, sealed SealedAnyIntConst, method into_any_int_const_pat,
    variant IntConst, 0u64, crate::pat::node_pat::BuildTy::InheritRoot,
    typed IntVar, bind_int, get_int, "IntVar"
);
// …
```

- [ ] **Step 10: Final compile + test**

Run: `cargo check -p pattern 2>&1 | grep -E "error" | head -5`
Expected: empty.

Run: `cargo test -p pattern 2>&1 | grep -E "test result"`
Expected: `188 passed; 0 failed` (across three suite results).

Run: `cargo test --workspace 2>&1 | grep -E "FAILED" | head -5`
Expected: empty.

Run: `cargo clippy --workspace -p pattern 2>&1 | tail -5`
Expected: no warnings from the pattern crate.

---

### Task 6: Wire `find_all` prefilter

**Files:**
- Modify: [`crates/pattern/src/matcher/mod.rs`](crates/pattern/src/matcher/mod.rs)

- [ ] **Step 1: Apply the filter in `find_all`**

Replace the existing `find_all` body:

```rust
pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
    let filter = pat.as_dyn().root_kind_filter();
    self.fn_graph
        .preorder()
        .filter(|&node| filter.accepts(self.fn_graph.graph.node_kind(node)))
        .filter_map(|node| {
            let mut bindings = Bindings::default();
            if self.match_node_id(node, pat, &mut bindings) {
                Some(Match { root: node, bindings })
            } else {
                None
            }
        })
        .collect()
}
```

Note: `KindFilter::accepts(kind: &NodeKind)` — confirm the iterator yields `NodeId` and we call `node_kind(node)` inside the filter closure. `KindFilter` is `Copy`, so moving into the filter closure is free.

`match_at` is unchanged — it checks a single known root and the filter would be redundant (it's still enforced inside `try_match_common`).

- [ ] **Step 2: Run pattern tests**

Run: `cargo test -p pattern 2>&1 | grep -E "test result"`
Expected: `188 passed; 0 failed`. If any test fails, investigate — it indicates a ctor that under-advertised its filter (set too-narrow `Single` where it should be `Any`, or a forwarding combinator that didn't delegate).

- [ ] **Step 3: Run full workspace**

Run: `cargo test --workspace 2>&1 | grep -E "FAILED" | head -5`
Expected: empty.

- [ ] **Step 4: Commit the whole Phase 2**

```bash
git add crates/pattern/src/pat/node_pat.rs crates/pattern/src/pat/traits.rs crates/pattern/src/pat/any.rs crates/pattern/src/pat/guards.rs crates/pattern/src/pat/builders.rs crates/pattern/src/pat/ctor/ crates/pattern/src/pat/mod.rs crates/pattern/src/matcher/mod.rs
git commit -m "KindFilter hint + find_all prefilter

Matcher::find_all previously walked every node in preorder and tried
the full pattern at each candidate. For a rule rooted at a concrete
NodeKind (add, load, call, ret, if) this wastes 95%+ of iterations on
irrelevant kinds.

Add a structural KindFilter hint on NodePat advertising the root
discriminant the pattern accepts. Pattern trait exposes it via
root_kind_filter(); combinators (CapturePat/WhenPat/WhenMatchPat)
forward to inner; NodePat returns its stored filter. find_all uses
the filter to skip incompatible kinds in the preorder loop.
try_match_common also short-circuits on it so match_at callers
(rewrite rules) get the fast-fail path too.

Every ctor site sets the filter to its expected NodeKind discriminant.
Match-only-false patterns (int_const_with_fn &c) stay KindFilter::Any
since they never match on LHS anyway."
```

---

## Verification checklist (post-plan)

- [ ] `cargo test -p pattern` — 188 passed, 0 failed (plus 13 lib tests)
- [ ] `cargo test --workspace` — no FAILED
- [ ] `cargo clippy --workspace -p pattern` — no warnings
- [ ] `grep -rn "let snap = b.clone\|\*b = snap\|after_kind = b.clone\|\*b = after_kind" crates/pattern/src/` — empty
- [ ] `grep -rn "Bindings { vars:\|bool_vals:\|int_binary_ops:" crates/pattern/src/` — empty (the old HashMap structure is gone)
- [ ] Visual check: `find_all` body includes `filter.accepts(...)` in the preorder chain
- [ ] Visual check: every `NodePat::matcher(...)` call passes a `KindFilter` as first arg

## Deliberately out of scope

- Full `enum KindMatch { Exact, IntBinaryAny, …, Custom(Arc<dyn Fn>) }` replacing the closure. The hint captures the preorder-prefilter win (biggest); replacing the closure with an enum match only helps on the post-filter path which is <5% of nodes for typical patterns. Revisit only if benchmarks show vtable dispatch dominates after this plan.
- Overlay-HashMap hybrid for large binding sets. Add only if a pattern is observed producing >32 captures with measurable scan cost.
- Benchmarks. Add when the user requests quantified numbers.

## Commit structure

Phase 1 = 2 commits (storage rewrite, then call-site migration).
Phase 2 = 1 commit (atomic across node_pat.rs + trait + combinators + every ctor + find_all — the APIs are co-dependent and splitting risks broken-midway state).

Total: 3 commits.
