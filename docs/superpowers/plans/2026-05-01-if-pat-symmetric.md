# `IfPat` Symmetric Matching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `if_node().cond(C).true_branch(T)` also match graphs where the If has condition `Not(C)` and `T` matches the *false* branch (and the symmetric case for `false_branch`). The semantics handle compilers that invert the if-then-else for codegen reasons; the source-level logic is the same.

**Architecture:** Replace the simple `IfPat → Pat` lowering (which produces a single `NodePat` with indexed inputs and indexed consumers) with a custom `Pattern` impl that internally tries two layouts:
1. **Direct**: cond matches the If's input slot 1; true_branch matches consumer of output 0; false_branch matches consumer of output 1.
2. **Swapped**: the If's input slot 1 is `BoolUnaryOp::Neg(<inner>)`; the inner matches the pattern's cond; true_branch matches consumer of output 1 (false output); false_branch matches consumer of output 0 (true output).

The swap is attempted **only when the pattern has a `cond` constraint** — without a cond, "swap" has no semantic basis (there's nothing to negate), so we keep the conservative direct match. This matches the user's stated motivation: compilers invert *because of* a condition negation.

**Tech Stack:** Rust 1.93 + edition 2024; existing `pattern` crate primitives (`Pattern` trait, `NodePat`, `MatchCtx`, `Bindings`); `ir::BoolUnaryOp::Neg` for the negation check.

**Worktree:** `/home/mike/Desktop/strider/.worktrees/if-pat-symmetric/` on branch `feature/if-pat-symmetric` (cut from `feature/ai`).

---

## Pre-flight

- [ ] **Step 0a: Verify clean baseline**

```bash
cd /home/mike/Desktop/strider/.worktrees/if-pat-symmetric
cargo build --workspace --tests --benches 2>&1 | tail -3
cargo test -p pattern 2>&1 | grep -c "test result: FAILED"
```

Expected: clean build; FAILED count = 0.

---

## Task 1: Write failing tests for the new symmetric semantics

**Why TDD:** the matching engine is subtle (multiple match paths, walk-through, captures); failing tests up front pin down exactly what behaviour we want before touching the matcher.

**Files:**
- New: `crates/pattern/tests/matching/if_pat_symmetric.rs`
- Modify: `crates/pattern/tests/matching.rs` — add the new module.

**Test fixture shapes needed:**
- (a) `if(int_lt(x, y)) { ret 1 } else { ret 2 }` — direct layout
- (b) `if(bool_not(int_lt(x, y))) { ret 2 } else { ret 1 }` — swapped layout (compiler-inverted equivalent of (a))

These should be added to `crates/pattern/tests/matching/support/shapes.rs` as helpers `if_inverted_cond_then_return(...)` analogous to the existing `if_cmp_then_return`.

- [ ] **Step 1: Add the swapped fixture shape**

Read [`crates/pattern/tests/matching/support/shapes.rs`](crates/pattern/tests/matching/support/shapes.rs) to find `if_cmp_then_return`.  Add a parallel function `if_cmp_then_return_inverted` that builds:

```text
entry:
    cond = bool_not(int_lt(int_const(N), int_const(1)))   // i.e. Not(N < 1)
    if cond goto false_region else goto true_region        // note: branches swapped
true_region:
    return int_const(2)        // was the "false" body in if_cmp_then_return
false_region:
    return int_const(1)        // was the "true" body
```

Both fixtures encode the same logic — `if (N < 1) { ret 1 } else { ret 2 }` — but the second has the cond inverted and branches swapped, which is what a compiler might emit.

The function signature mirrors `if_cmp_then_return`:
```rust
pub fn if_cmp_then_return_inverted(n: u64) -> ir::BuiltFunctionGraph {
    // ... build the inverted form
}
```

(Read the existing `if_cmp_then_return` for the exact builder API; do not invent.)

- [ ] **Step 2: Create the new test module**

Create [`crates/pattern/tests/matching/if_pat_symmetric.rs`](crates/pattern/tests/matching/if_pat_symmetric.rs) with the following tests.  Each test uses both fixtures (direct + inverted) and asserts that the same pattern matches in both:

```rust
//! Symmetric If-pattern matching: `if_node().cond(C).true_branch(T)` should
//! also match graphs where the cond is `Not(C)` and `T` is in the false
//! branch.  Models compiler-inverted if-then-else.

use ir::IntCmpOp;
use pattern::*;

use super::support::{Tb, assertions as a, reg_vn, shapes};

// ── True-branch swap ─────────────────────────────────────────────────────────

#[test]
fn cond_with_true_branch_matches_direct() {
    let g = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_lt(int_const(4u64), int_const(1u64)))
        .true_branch(any());
    a::matches(&g, pat, 1);
}

#[test]
fn cond_with_true_branch_matches_swapped() {
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_lt(int_const(4u64), int_const(1u64)))
        .true_branch(any());
    a::matches(&g, pat, 1);
}

// ── False-branch swap ────────────────────────────────────────────────────────

#[test]
fn cond_with_false_branch_matches_direct() {
    let g = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_lt(int_const(4u64), int_const(1u64)))
        .false_branch(any());
    a::matches(&g, pat, 1);
}

#[test]
fn cond_with_false_branch_matches_swapped() {
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_lt(int_const(4u64), int_const(1u64)))
        .false_branch(any());
    a::matches(&g, pat, 1);
}

// ── Both branches: full layout swap ──────────────────────────────────────────

#[test]
fn cond_with_both_branches_matches_direct() {
    let g = shapes::if_cmp_then_return(4);
    // Pattern says: cond=(4<1), true contains const 1, false contains const 2.
    let pat = if_node()
        .cond(int_lt(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .false_branch(any());
    a::matches(&g, pat, 1);
}

#[test]
fn cond_with_both_branches_matches_swapped() {
    // Inverted graph encodes the same source-level program; the pattern
    // (still written from the source POV) must still match.
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_lt(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .false_branch(any());
    a::matches(&g, pat, 1);
}

// ── Cond mismatch still doesn't match ────────────────────────────────────────

#[test]
fn cond_mismatch_no_match_in_direct() {
    let g = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_lt(int_const(99u64), int_const(1u64)))   // wrong constant
        .true_branch(any());
    a::none(&g, pat);
}

#[test]
fn cond_mismatch_no_match_in_swapped() {
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_lt(int_const(99u64), int_const(1u64)))   // wrong constant
        .true_branch(any());
    a::none(&g, pat);
}

// ── No cond: no swap (conservative semantics) ────────────────────────────────

#[test]
fn no_cond_only_true_branch_matches_direct_only() {
    // With no cond constraint, the swap is not attempted — `true_branch(p)`
    // means literally the true branch.  This is the conservative semantics
    // documented in IfPat::true_branch.
    let g_direct   = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node().true_branch(any());
    a::matches(&g_direct, pat.clone(), 1);
    // In the inverted graph the branch labelled "true" is the OTHER one;
    // an unconstrained pattern still matches because true-branch's
    // consumer exists either way.  But this is the direct match, not a
    // swap — the matcher didn't fold any logical equivalence.  Document.
    a::matches(&g_inverted, pat, 1);
}

// ── Capture sees the same If node either way ────────────────────────────────

#[test]
fn captured_if_node_id_works_in_both_layouts() {
    let g_direct   = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let n = Capture::new();
    let pat = if_node()
        .cond(int_lt(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .capture(n);
    let m_d = a::unique(&g_direct,   pat.clone());
    let m_i = a::unique(&g_inverted, pat);
    assert!(matches!(g_direct  .graph.node_kind(m_d.node(n).unwrap()), ir::node::NodeKind::If));
    assert!(matches!(g_inverted.graph.node_kind(m_i.node(n).unwrap()), ir::node::NodeKind::If));
}
```

Add the module declaration to [`crates/pattern/tests/matching.rs`](crates/pattern/tests/matching.rs):
```rust
mod if_pat_symmetric;
```

- [ ] **Step 3: Run the tests — they must FAIL**

```bash
cd /home/mike/Desktop/strider/.worktrees/if-pat-symmetric
cargo test -p pattern --test matching if_pat_symmetric 2>&1 | tail -20
```

Expected: the new fixture compiles fine; the four `*_swapped` tests FAIL because the matcher doesn't yet do the swap.  The `*_direct` tests pass.  The `cond_mismatch_no_match_in_swapped` test currently passes vacuously (the pattern can't match) — it'll need to pass NON-vacuously after Task 2.

If the swap-required tests pass already, something is wrong with the fixture; investigate before continuing.

---

## Task 2: Implement the symmetric match in `IfPat`

**Files:**
- Modify: `crates/pattern/src/pat/builders/branch.rs` — replace the `From<IfPat> for Pat` lowering with a custom `Pattern` impl.

The current lowering (`branch.rs:40-64`) produces a single `NodePat` with `InputsSpec::Indexed(...)` and `ConsumersSpec::Indexed(...)`.  We replace it with a `Pat::from_dyn(Arc::new(IfPattern { ... }))` whose `try_match` runs both layouts.

- [ ] **Step 1: Sketch the new `IfPattern` struct**

In `crates/pattern/src/pat/builders/branch.rs`, replace the existing impls with:

```rust
//! `IfPat` — matches `If` nodes with optional constraints on the condition
//! input and the single consumers of the true/false control outputs.
//!
//! When the pattern has a `cond` constraint, the matcher tries TWO layouts:
//! 1. **Direct**: cond matches input 1; true_branch matches consumer of output 0;
//!    false_branch matches consumer of output 1.
//! 2. **Swapped**: input 1 is `BoolUnaryOp::Neg(inner)`, inner matches cond;
//!    true_branch matches consumer of output 1; false_branch matches consumer
//!    of output 0.
//!
//! This handles compiler-inverted if-then-else: `if (c) A else B` and
//! `if (!c) B else A` are logically equivalent and must both match the
//! source-level pattern `if_node().cond(c).true_branch(A).false_branch(B)`.
//!
//! Without a `cond` constraint, no swap is attempted — there is no
//! condition to negate.

use std::sync::Arc;

use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use ir::{BoolUnaryOp, ops};

use crate::matcher::Bindings;
use crate::pat::Pat;
use crate::pat::node_pat::KindSpec;
use crate::pat::traits::{MatchCtx, Pattern};

/// Builder for `If` node patterns.  Created by [`crate::pat::if_node`].
pub struct IfPat {
    cond: Option<Pat>,
    true_branch: Option<Pat>,
    false_branch: Option<Pat>,
}

impl IfPat {
    pub(crate) fn new() -> Self {
        Self { cond: None, true_branch: None, false_branch: None }
    }
    /// Constrain the branch condition.  When set, the matcher also tries
    /// the compiler-inverted layout — see module-level docs.
    pub fn cond(mut self, p: impl Into<Pat>) -> Self {
        self.cond = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's true-branch
    /// output.  When `cond` is also set, also matches the consumer of
    /// the false-branch output if cond is found wrapped in `Not(...)`.
    pub fn true_branch(mut self, p: impl Into<Pat>) -> Self {
        self.true_branch = Some(p.into());
        self
    }
    /// Match `p` against the single consumer of the If's false-branch
    /// output.  Symmetric to `true_branch`.
    pub fn false_branch(mut self, p: impl Into<Pat>) -> Self {
        self.false_branch = Some(p.into());
        self
    }
}

/// Custom `Pattern` impl for `IfPat`: tries direct and (if cond is set)
/// swapped layouts.
struct IfPattern {
    cond: Option<Pat>,
    true_branch: Option<Pat>,
    false_branch: Option<Pat>,
}

impl Pattern for IfPattern {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node = ctx.graph.graph.get_node_from_output(target);
        if !matches!(ctx.graph.graph.node_kind(node), NodeKind::If) {
            return false;
        }
        // Direct layout first.
        let mark = b.mark();
        if self.try_layout(ctx, node, b, /*swapped=*/ false) {
            return true;
        }
        b.restore(mark);
        // Swap only when cond is constrained.
        if self.cond.is_none() {
            return false;
        }
        if self.try_layout(ctx, node, b, /*swapped=*/ true) {
            return true;
        }
        b.restore(mark);
        false
    }

    fn kind_spec(&self) -> KindSpec {
        KindSpec::Exact(NodeKind::If)
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        // If has zero value outputs; the default "iterate outputs" path
        // doesn't apply.  Match against the node directly.
        if !matches!(ctx.graph.graph.node_kind(node), NodeKind::If) {
            return false;
        }
        let mark = b.mark();
        if self.try_layout(ctx, node, b, /*swapped=*/ false) {
            return true;
        }
        b.restore(mark);
        if self.cond.is_none() {
            return false;
        }
        if self.try_layout(ctx, node, b, /*swapped=*/ true) {
            return true;
        }
        b.restore(mark);
        false
    }
}

impl IfPattern {
    fn try_layout(
        &self,
        ctx: &MatchCtx,
        if_node: NodeId,
        b: &mut Bindings,
        swapped: bool,
    ) -> bool {
        // 1. Cond.  Input 1 of the If.
        if let Some(cond_pat) = &self.cond {
            let inputs = ctx.graph.graph.node_inputs(if_node);
            let Some(cond_in) = inputs.into_iter().nth(1) else {
                return false;
            };
            if swapped {
                // Require cond_in to be Neg(<x>); match cond_pat against <x>.
                let cond_node = ctx.graph.graph.get_node_from_output(cond_in);
                if !matches!(
                    ctx.graph.graph.node_kind(cond_node),
                    NodeKind::BoolUnaryOp(BoolUnaryOp::Neg)
                ) {
                    return false;
                }
                let inner_inputs = ctx.graph.graph.node_inputs(cond_node);
                let Some(inner) = inner_inputs.into_iter().next() else {
                    return false;
                };
                if !ctx.matcher.match_output_with_walk_through(inner, cond_pat, b) {
                    return false;
                }
            } else if !ctx.matcher.match_output_with_walk_through(cond_in, cond_pat, b) {
                return false;
            }
        }

        // 2. True-branch consumer (or false-branch under swap).
        let true_pat = self.true_branch.as_ref();
        let false_pat = self.false_branch.as_ref();
        let (true_out_idx, false_out_idx) = if swapped { (1, 0) } else { (0, 1) };

        if let Some(tp) = true_pat {
            if !match_branch_consumer(ctx, if_node, true_out_idx, tp, b) {
                return false;
            }
        }
        if let Some(fp) = false_pat {
            if !match_branch_consumer(ctx, if_node, false_out_idx, fp, b) {
                return false;
            }
        }
        true
    }
}

/// Match `pat` against the single forward-step consumer of the If's
/// output at `output_index`.  Honors `ignore_control_states`: walks
/// through the immediate ControlState header if necessary, mirroring
/// the existing `match_consumer_node` shape used elsewhere.
fn match_branch_consumer(
    ctx: &MatchCtx,
    if_node: NodeId,
    output_index: usize,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let outputs = ctx.graph.graph.node_outputs(if_node);
    let Some(out) = outputs.into_iter().nth(output_index) else {
        return false;
    };
    // The output is a Control edge — find its single consumer node.
    let Some(consumer) = crate::matcher::walk::next_control_node(ctx.matcher, out) else {
        return false;
    };
    crate::pat::node_pat::match_consumer_node(ctx, consumer, pat, b)
}

impl From<IfPat> for Pat {
    fn from(b: IfPat) -> Pat {
        let IfPat { cond, true_branch, false_branch } = b;
        Pat::from_dyn(Arc::new(IfPattern { cond, true_branch, false_branch }))
    }
}
```

Some of the functions referenced (`match_consumer_node`, `match_output_with_walk_through`) already exist privately in the pattern crate.  The CRITICAL question is whether they're accessible from `pat::builders::branch`.  In step 2 below we identify what to make `pub(crate)` and what to inline.

- [ ] **Step 2: Make required helpers reachable from `branch.rs`**

The helpers `match_consumer_node` (at [`crates/pattern/src/pat/node_pat.rs:531`](crates/pattern/src/pat/node_pat.rs#L531)) and `Matcher::match_output_with_walk_through` are currently private to their respective modules.  Audit:

```bash
grep -n "pub(crate)\|fn match_consumer_node\|fn match_output_with_walk_through" \
    crates/pattern/src/pat/node_pat.rs \
    crates/pattern/src/matcher/mod.rs
```

If they're already `pub(crate)`, no action.  Otherwise, change their visibility to `pub(crate)` so `branch.rs` can call them.  Also check `crate::matcher::walk::next_control_node` is accessible.

- [ ] **Step 3: Run the failing tests — they must now PASS**

```bash
cargo test -p pattern --test matching if_pat_symmetric 2>&1 | tail -20
```

Expected: all 9 tests pass.

If any swapped test fails, debug:
1. Check the fixture's IR via `dot::dump_html(&fg, "/tmp/dbg.html")` to confirm the cond IS wrapped in `Not(...)` and branches ARE swapped.
2. Check `ctx.matcher.match_output_with_walk_through` semantics — does it handle `BoolUnaryOp::Neg` walk-through itself?  If yes, our explicit Neg check could be redundant; if no, the explicit Neg check is the correctness guard.

- [ ] **Step 4: Run the full pattern crate test suite**

```bash
cargo test -p pattern 2>&1 | tail -5
cargo test -p pattern 2>&1 | grep -c "test result: FAILED"
```

Expected: 0 failures.  Existing `if_node_*` tests (in `tests/matching/control_flow.rs`) must continue passing.

- [ ] **Step 5: Run the workspace tests**

```bash
cargo test --workspace 2>&1 | grep -c "test result: FAILED"
```

Expected: 0 failures.  No callers in `opt`, `strider`, or elsewhere should break — the swap is a STRICT EXTENSION of matching (every previously-matched case still matches; some additional cases match too).

If anything fails, the most likely cause is: a caller relied on the pattern NOT matching a swapped case (e.g. it was building a pattern, expecting only direct matches, and now gets unexpected matches).  Investigate and either fix the caller or document the new semantics.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
pattern/if_pat: match compiler-inverted if-then-else layouts

if_node().cond(C).true_branch(T) now also matches graphs where
the If's input is Not(C) and T is in the false branch (and the
symmetric case for false_branch).  Compilers commonly emit the
inverted form; the source-level logic is the same.

The swap is attempted only when the pattern has a cond constraint —
without a cond, there's no negation and no swap.

New IfPattern impl replaces the simple NodePat lowering; tests in
tests/matching/if_pat_symmetric.rs cover both layouts plus a
no-cond control case.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Update `lib.rs` rustdoc and `CLAUDE.md` if needed

- [ ] **Step 1: Audit pattern crate rustdoc for `if_node` / `IfPat`**

```bash
grep -rn "if_node\|IfPat\|true_branch\|false_branch" crates/pattern/src/lib.rs crates/pattern/src/pat/mod.rs crates/pattern/src/pat/ctor/control.rs
```

Where the docs say "matches the consumer of the true output", add a note about the cond-controlled swap.  Specifically:
- [`crates/pattern/src/pat/ctor/control.rs:120`](crates/pattern/src/pat/ctor/control.rs#L120) — the `if_node()` function's rustdoc.
- The `IfPat::true_branch` / `false_branch` rustdoc was updated in Task 2 already.

Add a one-paragraph note to `if_node()`'s rustdoc:
```rust
/// Starts building an `If` pattern.  Chain `.cond()`, `.true_branch()`,
/// `.false_branch()` to add constraints.
///
/// **Symmetric matching with cond:** when both `.cond(C)` and a branch
/// constraint are set, the matcher also tries the compiler-inverted
/// layout — input `Not(C)` with branches swapped.  Without a `.cond()`
/// constraint, only the direct layout is tried.
```

- [ ] **Step 2: Audit `CLAUDE.md`**

```bash
grep -n "if_node\|IfPat\|true_branch\|false_branch" CLAUDE.md
```

The pattern crate bullet at line ~96 mentions `IfPat (.cond(p), .true_branch, .false_branch, .capture(c), .when(f))`.  Add a one-clause hint:
> `IfPat` (`.cond(p)`, `.true_branch`, `.false_branch`, …) — when `.cond(C)` is set, also matches the compiler-inverted layout (`Not(C)` + swapped branches).

- [ ] **Step 3: Build & test (sanity)**

```bash
cargo build --workspace --tests --benches 2>&1 | tail -3
cargo test --workspace 2>&1 | grep -c "test result: FAILED"
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
pattern/docs: document IfPat's symmetric matching

if_node()'s rustdoc and the CLAUDE.md pattern crate bullet now
mention that `.cond(C)` enables the compiler-inverted-layout match.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Final verification

- [ ] **Step F1: Build, test, clippy**

```bash
cargo build --workspace --tests --benches 2>&1 | tail -3
cargo test --workspace 2>&1 | tail -5
cargo clippy --workspace --tests --benches 2>&1 | tail -10
```

Expected: clean across the board.  No FAILED counts; no clippy errors.

- [ ] **Step F2: Code review**

Dispatch the `feature-dev:code-reviewer` agent on the diff against `feature/ai`:
```bash
git log --oneline feature/ai..HEAD
git diff --stat feature/ai..HEAD
```

Ask the reviewer for: simplification opportunities, missed walk-through cases (e.g. should the swap consider `BoolBinaryOp::Xor(c, true)` as a Neg synonym?), correctness gaps in the new tests, doc drift.

Apply any high-confidence findings; document and skip low-confidence ones.

- [ ] **Step F3: Merge to `feature/ai`**

```bash
cd /home/mike/Desktop/strider
git checkout feature/ai
git merge --ff-only feature/if-pat-symmetric
```

If non-FF (because someone else merged), rebase first.

---

## Out-of-scope (intentionally not done)

- **`Xor(c, true)` as a Neg synonym.**  Some compilers emit `c ^ true` instead of `Not(c)`.  Adding this widens the match surface; defer until a concrete consumer needs it.
- **Generic "branch swap without cond".**  The user explicitly framed the feature around cond inversion.  Without a cond constraint, swapping branches changes pattern semantics in confusing ways (e.g. `if_node().true_branch(p)` would match either branch — ambiguous).  Conservative semantics: no cond → no swap.
- **Symmetric matching for `if-else if-else` chains.**  An if with multiple comparisons in the true branch could in principle be inverted at multiple levels.  The current implementation handles a single If node's swap; chained inversions would compose naturally as the matcher recurses.

---

## Self-review notes

- **Spec coverage:** Each requirement (true-branch swap, false-branch swap, both-branches swap, no-swap-without-cond) has a test in Task 1 Step 2.  Each test pairs a direct fixture with an inverted fixture and checks both match.
- **Type consistency:** `IfPat`, `IfPattern`, `Pat`, `Pattern`, `MatchCtx`, `Bindings`, `BoolUnaryOp::Neg`, `NodeKind::BoolUnaryOp(...)` — names used consistently.
- **API surface:** `IfPat`'s public API (`new`, `cond`, `true_branch`, `false_branch`) is unchanged; the swap is internal.  No new builder methods.  Backward-compatible.
- **Performance:** the swap is a second match attempt with a `BoolUnaryOp::Neg` kind check up front — short-circuits if the input is not `Neg`, so the cost on graphs without inversion is one extra kind comparison per `IfPat` match.  Negligible.
