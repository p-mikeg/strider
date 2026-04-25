# opt Crate Review (Round 3) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address correctness, simplification, readability, and clippy issues found in a fresh blind review of `crates/opt`.

**Architecture:** Each task is a localized fix or refactor; tests are added/updated where the issue is observable; baseline `cargo build -p opt`, `cargo test -p opt`, and `cargo clippy -p opt --all-targets --no-deps -- -D warnings` must remain green after every commit.

**Tech Stack:** Rust 2024, `cranelift-entity`, `rustc_hash` (FxHashMap/Set), `pattern` rewrite-rule DSL, `ir-macros::match_value!`.

**Worktree:** `.worktrees/opt-review-r3` on branch `review/opt-crate-r3` (already created, branched from `feature/ai`).

---

## Findings overview

The review surfaced one real correctness bug (silent miscompilation of `FloatUnaryOp::Round` constant folding), one IR-leak in `stack_load_forward`, and a small set of consistency / readability / clippy items.

| # | Area | Severity | Type |
|---|------|----------|------|
| 1 | `eval_float::eval_float_unary` `Round` uses wrong rounding mode | **Bug** | correctness |
| 2 | `stack_load_forward::resolve` leaks freshly-built nodes on partial MemPhi failure | Bug | correctness / hygiene |
| 3 | `constant_fold::ConstantFold` struct doc-comment is mis-attached to `try_lower_cast_to_float` | Bug | docs |
| 4 | `redundant_phis::remove_phis` carries dead defensive `ok_or` after length check | Cleanup | readability |
| 5 | `sp_expr` uses `std::collections::HashSet` instead of `FxHashSet` like the rest of the file | Cleanup | consistency |
| 6 | `function_args::detect_stack_args` mixes `std::collections::HashMap`/`HashSet` with FxHashMap | Cleanup | consistency |
| 7 | `known_bits::optimize` Phase 1 reimplements a worklist instead of using `crate::worklist::WorkSet` | Refactor | dedup |
| 8 | `worklist::detach_unreachable_nodes` collects `all_node_ids()` into a needless `Vec` | Cleanup | clippy `needless_collect` |
| 9 | `constant_fold::rules` BAnd/BOr absorbing-element rules use awkward `if !l { false } else { skip }` shape | Cleanup | readability |
| 10 | `let Some(_) = _ else { return ... }` pattern in 3 sites should be `let-else` | Cleanup | clippy `manual_let_else` |

Excluded explicitly: pedantic warnings (e.g. `missing_const_for_fn`, `redundant_pub_crate`), per-arch float comparison advice (`float_cmp` would force epsilon comparisons that are wrong for IEEE constant folding), and architectural redesigns of the SP-decomposition fallback.

---

## Task 1: Fix `FloatUnaryOp::Round` constant folding to use round-to-nearest-even

**Files:**
- Modify: `crates/opt/src/constant_fold/eval_float.rs:75-103`
- Test: `crates/opt/src/constant_fold/tests.rs`

**Why:** `FloatUnaryOp::Round` is documented in `crates/ir/src/ops/op_kinds.rs:117-118` as *"Round to nearest integer (ties to even): round(x)"*, matching IEEE 754 default rounding mode (FE_TONEAREST → ties-to-even). The current implementation calls Rust's `f32::round` / `f64::round`, which is **round-half-away-from-zero**. So the optimizer silently miscompiles half-ties:

| input | IEEE round-to-even (correct) | Rust `round` (current bug) |
|-------|------------------------------|----------------------------|
| `0.5` | `0.0` | `1.0` |
| `1.5` | `2.0` | `2.0` |
| `2.5` | `2.0` | `3.0` |
| `-0.5` | `-0.0` | `-1.0` |

Rust 1.77 stabilized `f32::round_ties_even` / `f64::round_ties_even`; this workspace is on 1.93 (Cargo.toml `edition = "2024"`).

- [ ] **Step 1: Write the failing test in `crates/opt/src/constant_fold/tests.rs`**

Append (find a free location near other float constant-fold tests):

```rust
#[test]
fn round_uses_ties_to_even_not_away_from_zero() -> Result<()> {
    use ir::node::NodeOutputType;
    use ir::FloatUnaryOp;

    // Each tuple: (input bits, expected ties-to-even output bits, ty).
    let cases_f64: &[(u64, u64)] = &[
        (0.5_f64.to_bits(), 0.0_f64.to_bits()),
        (1.5_f64.to_bits(), 2.0_f64.to_bits()),
        (2.5_f64.to_bits(), 2.0_f64.to_bits()),
        ((-0.5_f64).to_bits(), (-0.0_f64).to_bits()),
        ((-1.5_f64).to_bits(), (-2.0_f64).to_bits()),
    ];
    for &(input_bits, expected_bits) in cases_f64 {
        let mut b = ir::FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let v = b.build_float_const(input_bits, NodeOutputType::F64)?;
        let r = b.build_float_unary_operation(v, FloatUnaryOp::Round, NodeOutputType::F64)?;
        b.build_return(Some(r), &[])?;
        let mut fg = b.build()?;
        crate::ConstantFold.optimize(&mut fg)?;

        // Find the Return's input and check it folded to the expected constant.
        let folded_bits = collect_returned_float_const(&fg)
            .expect("expected a folded FloatConst feeding Return");
        assert_eq!(
            folded_bits, expected_bits,
            "Round({}) folded to bits {:#x}, expected {:#x}",
            f64::from_bits(input_bits), folded_bits, expected_bits,
        );
    }
    Ok(())
}

/// Locates the unique Return node and returns the bits of its first ret-val
/// input *if* that input is a `FloatConst`. Returns `None` otherwise.
fn collect_returned_float_const(fg: &ir::BuiltFunctionGraph) -> Option<u64> {
    for n in fg.preorder() {
        if !matches!(fg.graph.node_kind(n), ir::node::NodeKind::Return) {
            continue;
        }
        let inputs = fg.graph.node_inputs(n);
        // Return inputs: [ctrl, ret_val_0, ...]. We expect exactly one ret-val.
        let ret_val = *inputs.get(1)?;
        let producer = fg.graph.get_node_from_output(ret_val);
        if let ir::node::NodeKind::FloatConst(bits) = *fg.graph.node_kind(producer) {
            return Some(bits);
        }
        return None;
    }
    None
}
```

- [ ] **Step 2: Run the test and confirm it fails on at least the half-tie cases**

Run: `cargo test -p opt --lib round_uses_ties_to_even_not_away_from_zero -- --nocapture`
Expected: FAIL — current `f64::round` returns `1.0` for `0.5`, `3.0` for `2.5`, `-1.0` for `-0.5`.

- [ ] **Step 3: Fix `eval_float_unary` to use `round_ties_even`**

In `crates/opt/src/constant_fold/eval_float.rs`, replace **both** Round arms (the F32 branch around line 85 and the F64 branch around line 97):

```rust
// F32 branch
FloatUnaryOp::Round => v.round_ties_even(),
```

```rust
// F64 branch
FloatUnaryOp::Round => v.round_ties_even(),
```

(Leave Neg / Abs / Sqrt / Ceil / Floor unchanged — they all already match IEEE 754 / hardware semantics.)

- [ ] **Step 4: Run the test and confirm it passes**

Run: `cargo test -p opt --lib round_uses_ties_to_even_not_away_from_zero`
Expected: PASS.

- [ ] **Step 5: Run the full opt test suite**

Run: `cargo test -p opt`
Expected: all previously-passing tests still pass; new test passes; no other regressions.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -p opt --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/opt/src/constant_fold/eval_float.rs crates/opt/src/constant_fold/tests.rs
git commit -m "$(cat <<'EOF'
fix(opt): use IEEE round-ties-to-even for FloatUnaryOp::Round constfold

The IR documents Round as "ties to even" (matches IEEE 754 default
rounding mode and x86/ARM hardware), but eval_float_unary called Rust's
f64::round which is ties-away-from-zero. Half-ties at 0.5, 2.5, -0.5 etc.
folded to the wrong constant. Switch to round_ties_even (stable since
Rust 1.77).
EOF
)"
```

---

## Task 2: Stop leaking IR nodes when `stack_load_forward::resolve` aborts a MemPhi

**Files:**
- Modify: `crates/opt/src/stack_load_forward/mod.rs:115-220`
- Test: `crates/opt/src/stack_load_forward/tests.rs`

**Why:** Inside `resolve`, the `NodeKind::MemPhi` arm (currently lines 183-217) walks every predecessor with `let v = resolve(...)?;`. When a non-first predecessor returns `None`, the `?` short-circuits — but earlier predecessors may already have created fresh `Truncate` / `ShiftRight` / `ValuePhi` nodes via the recursive `resolve`. Those nodes leak into `Graph` as orphans (no users, no path back from entry except indirectly through producers' use-lists). The validator scopes Layer A to the reachable set so it doesn't flag them, but they still bloat the graph and confuse the `dot` renderer (it draws use-lists and would emit edges to detached nodes).

The fix is to resolve all predecessors *first* (collecting a `Vec<Option<NodeOutputId>>`), bail without creating *any* new node if any one is `None`, and only then synthesize the `ValuePhi` / narrowing chain. The narrow-load-from-wider-store helper (lines 153-172) already runs *before* the MemPhi arm so it isn't affected, but it has the same shape — partially built nodes can leak if the immediately following `replace_all_uses` fails. We'll handle just the MemPhi case here; the narrow case is single-step so it can only leak on the `replace_all_uses` failure path, which is genuinely exceptional.

- [ ] **Step 1: Write the failing test**

In `crates/opt/src/stack_load_forward/tests.rs`, append a test that:
1. Builds a 2-region IR where one region has a `StackStore[sp+0]` and the other has no matching store (so its `resolve` arm returns `None`).
2. Counts `ValuePhi` nodes before and after the pass.
3. Asserts the count did not increase (since the load wasn't forwardable, no `ValuePhi` should leak).

```rust
#[test]
fn aborted_memphi_resolution_does_not_leak_value_phi() -> Result<()> {
    // Two predecessors meeting at a join: one stores at sp+0, the other
    // does not. The load at sp+0 in the joined region cannot be forwarded,
    // so the pass must abort and leave the graph free of any new ValuePhi.
    let sp = sp_vn();
    let mut b = ir::FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let left = b.create_region()?;
    let right = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: branch true -> left, false -> right
    b.set_region(entry);
    let cond = b.build_int_const(1, ir::node::NodeOutputType::Bool)?;
    let cond_b = b.build_cast_to_bool(cond)?;
    b.build_branch_if(cond_b, left, right)?;

    // left: store sp at offset 0
    b.set_region(left);
    let sp_left = b.read_variable(&sp)?;
    let val = b.build_int_const(0xCAFE, ir::node::NodeOutputType::U32);
    b.build_store(rsleigh::VnSpace::RAM, sp_left, val)?;
    b.build_jump(join)?;

    // right: no store
    b.set_region(right);
    b.build_jump(join)?;

    // join: load from sp+0 — only one predecessor stored to it.
    b.set_region(join);
    let sp_join = b.read_variable(&sp)?;
    let loaded = b.build_load(rsleigh::VnSpace::RAM, sp_join, ir::node::NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let before = count_kind(&fg, |k| matches!(k, ir::node::NodeKind::ValuePhi));
    crate::StackLoadForward::new(sp, target::Endianness::Little).optimize(&mut fg)?;
    let after = count_kind(&fg, |k| matches!(k, ir::node::NodeKind::ValuePhi));
    assert_eq!(
        before, after,
        "abort path leaked a ValuePhi: before={before}, after={after}",
    );
    Ok(())
}

fn count_kind(
    fg: &ir::BuiltFunctionGraph,
    pred: impl Fn(&ir::node::NodeKind) -> bool,
) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}
```

> **Note for implementer:** the exact builder method names (e.g. `build_branch_if`, `build_jump`, `build_load`, `build_store`, `build_cast_to_bool`) and `sp_vn()` helper used here mirror conventions already used in this test module — copy the existing test imports and helpers verbatim from `crates/opt/src/stack_load_forward/tests.rs` rather than inventing new ones; if a method has a slightly different name in this codebase, adapt the test to the existing API. The point is the structural property: build a 2-pred MemPhi where exactly one predecessor stores; load from the join; assert no `ValuePhi` was added.

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo test -p opt --lib aborted_memphi_resolution_does_not_leak_value_phi -- --nocapture`
Expected: FAIL — `count_kind` after > before because a `ValuePhi` was synthesized for the left branch's resolved value before the right branch returned `None`.

- [ ] **Step 3: Fix the MemPhi arm**

In `crates/opt/src/stack_load_forward/mod.rs:183-217`, the structure must be:
1. Iterate predecessors with `resolve` and collect into `Vec<NodeOutputId>` only if every call returns `Some`.
2. If any returns `None`, return `None` without having mutated `fg.graph`.

Note that the recursive `resolve` itself synthesizes nodes for narrow-load-from-wider-store. To avoid those leaking, walk in two passes:

**Pass 1** — quick read-only check that every predecessor *can* resolve. We don't have a side-effect-free version of `resolve`, so the cleanest fix is to keep one pass but defer node creation. Since the narrow synthesis happens inside the per-predecessor `resolve` call, we cannot easily defer it.

**Practical fix:** record the size of `fg.graph.all_node_ids()` before the loop, abort by detaching any nodes created since the snapshot. But `detach_node_inputs` doesn't remove the node — they remain as zombies (which is what we're trying to avoid).

The cleanest fix is to refactor `resolve` so the recursive case for `MemPhi` *first* probes each predecessor with a side-effect-free check, then commits to building IR only when all predecessors will succeed. Implement a helper that returns the "shape" of what would be built without touching the graph:

```rust
/// Return value of `probe`: identifies the resolution shape without
/// mutating the graph. Used so that a MemPhi whose last predecessor
/// fails doesn't leak partially-built nodes from earlier predecessors.
enum ResolveShape {
    /// Existing output — no new nodes will be created.
    Existing(NodeOutputId),
    /// Narrow-load-from-wider-store. Creating the actual nodes is deferred.
    Narrow { data: NodeOutputId, store_ty: ir::node::NodeOutputType },
    /// Recursive MemPhi composition.
    Phi { phi_token: NodeOutputId, preds: Vec<ResolveShape> },
}

fn probe(
    fg: &BuiltFunctionGraph,
    mem: NodeOutputId,
    offset: i64,
    load_size: i64,
    load_ty: ir::node::NodeOutputType,
    visited: &mut rustc_hash::FxHashSet<NodeOutputId>,
) -> Option<ResolveShape> { /* mirror `resolve`, no graph mutations */ }

fn realize(
    fg: &mut BuiltFunctionGraph,
    shape: ResolveShape,
    load_ty: ir::node::NodeOutputType,
    endianness: Endianness,
) -> Option<NodeOutputId> { /* turn shape into IR; pure node-creation */ }
```

Replace `resolve` with `probe`-then-`realize`. Then the MemPhi arm in `probe` does:

```rust
NodeKind::MemPhi => {
    if !visited.insert(mem) { return None; }
    let inputs: Vec<NodeOutputId> = fg.graph.node_inputs(node).into_iter().collect();
    if inputs.len() < 2 { return None; }
    let phi_token = inputs[0];
    let mut preds = Vec::with_capacity(inputs.len() - 1);
    for &p in &inputs[1..] {
        preds.push(probe(fg, p, offset, load_size, load_ty, visited)?);
    }
    Some(ResolveShape::Phi { phi_token, preds })
}
```

Here the `?` is safe to short-circuit because `probe` does not mutate the graph.

`realize` collapses identical predecessor shapes (the existing dedup at lines 206-208) by doing the recursive `realize` first, comparing the resulting `NodeOutputId`s, and synthesizing a `ValuePhi` only when at least one differs. Finally update the call site in `try_forward_load`:

```rust
let Some(shape) = probe(fg, mem, offset, load_size, load_ty, &mut visited) else {
    return Ok(OptimizationResult::NoChange);
};
let Some(forwarded) = realize(fg, shape, load_ty, endianness) else {
    return Ok(OptimizationResult::NoChange);
};
```

- [ ] **Step 4: Run the leak test**

Run: `cargo test -p opt --lib aborted_memphi_resolution_does_not_leak_value_phi`
Expected: PASS.

- [ ] **Step 5: Run all stack-load-forward tests**

Run: `cargo test -p opt --lib stack_load_forward`
Expected: all preexisting tests still pass (the refactor is semantics-preserving for the success path).

- [ ] **Step 6: Run full test + clippy**

```bash
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/opt/src/stack_load_forward/mod.rs crates/opt/src/stack_load_forward/tests.rs
git commit -m "$(cat <<'EOF'
fix(opt): split stack_load_forward::resolve into probe+realize

The MemPhi arm walked predecessors with `resolve(...)?`; when a later
predecessor returned None, earlier predecessors that synthesized new
Truncate/ShiftRight/ValuePhi nodes leaked them into the graph as
orphans. Split into a side-effect-free `probe` (returns a ResolveShape)
and a `realize` (creates IR), so a partial walk cannot mutate the graph.
EOF
)"
```

---

## Task 3: Re-attach the `ConstantFold` doc-comment to the struct

**Files:**
- Modify: `crates/opt/src/constant_fold/mod.rs:18-31, 71`

**Why:** Lines 18-23 hold the doc paragraph *"Folds constant expressions and applies algebraic identities…"* that is meant for the `ConstantFold` struct on line 71. Because lines 24-30 are also a `///` block (documenting `try_lower_cast_to_float` on line 31), Rust attaches the *combined* 18-30 block to `try_lower_cast_to_float`, leaving the struct undocumented. The struct is one of the crate's public optimizers — losing its doc is a real public-API regression (no rustdoc, no IDE hover summary).

- [ ] **Step 1: Verify the misattribution with rustdoc**

Run:
```bash
cargo rustdoc -p opt --lib -- -D rustdoc::missing_docs 2>&1 | head -20
```
Or simpler — just `grep -n "pub struct ConstantFold" crates/opt/src/constant_fold/mod.rs` and confirm line 71 has no `///` immediately preceding it.

- [ ] **Step 2: Move the struct doc to the struct**

Edit `crates/opt/src/constant_fold/mod.rs`. The current shape (lines 16-71):

```rust
// ── Public optimizer ──────────────────────────────────────────────────────────

/// Folds constant expressions and applies algebraic identities.
///
/// Handles full constant evaluation for all arithmetic, comparison, boolean,
/// truncation, and extension operations.  Also applies identities such as
/// `x + 0 → x`, `x ^ x → 0`, and nested AND-mask merging `(a & C1) & C2 →
/// a & (C1 & C2)`.
/// Lowers a `CastToFloat` node to the appropriate specific form based on the
/// actual input type:
///
/// - Input is the same float type as output → eliminated (identity).
/// - Input is a different float type → lowered to `FloatToFloat`.
/// - Input is an integer `IntConst(v)` → immediately constant-folded to `FloatConst(v)`.
/// - Input is any other integer type → lowered to `IntBitsToFloat`.
fn try_lower_cast_to_float(
    ...
}

pub struct ConstantFold;
```

becomes:

```rust
/// Lowers a `CastToFloat` node to the appropriate specific form based on the
/// actual input type:
///
/// - Input is the same float type as output → eliminated (identity).
/// - Input is a different float type → lowered to `FloatToFloat`.
/// - Input is an integer `IntConst(v)` → immediately constant-folded to `FloatConst(v)`.
/// - Input is any other integer type → lowered to `IntBitsToFloat`.
fn try_lower_cast_to_float(
    ...
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Folds constant expressions and applies algebraic identities.
///
/// Handles full constant evaluation for all arithmetic, comparison, boolean,
/// truncation, and extension operations.  Also applies identities such as
/// `x + 0 → x`, `x ^ x → 0`, and nested AND-mask merging `(a & C1) & C2 →
/// a & (C1 & C2)`.
pub struct ConstantFold;
```

i.e. delete the misplaced `// ── Public optimizer ──` separator above the orphan doc, delete the orphan `Folds constant expressions…` paragraph from above `try_lower_cast_to_float`, and re-add the same paragraph (and the separator comment) immediately above `pub struct ConstantFold;`.

- [ ] **Step 3: Confirm the struct now has a doc**

Run: `cargo doc -p opt --no-deps --lib 2>&1 | tail -5` then open `target/doc/opt/struct.ConstantFold.html` (or just `grep -A2 "pub struct ConstantFold" crates/opt/src/constant_fold/mod.rs` and inspect the surrounding lines).

- [ ] **Step 4: Run tests + clippy**

```bash
cargo test -p opt --lib
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/opt/src/constant_fold/mod.rs
git commit -m "$(cat <<'EOF'
docs(opt): re-attach ConstantFold struct doc to the struct

The "Folds constant expressions and applies algebraic identities"
paragraph was sandwiched between a `// ──` separator and the doc for
try_lower_cast_to_float, so rustdoc bound the whole block to the
function and the struct ended up undocumented. Moved the paragraph
immediately above `pub struct ConstantFold;`.
EOF
)"
```

---

## Task 4: Drop dead defensive `ok_or` in `redundant_phis::remove_phis`

**Files:**
- Modify: `crates/opt/src/redundant_phis/mod.rs:85-110, 129-138`

**Why:** Three sites compute `let _ = X.iter().next().ok_or(ErrorKind::UniqueCtrlNotFound)?;` immediately after a `len() == 1` check. The `next()` cannot return `None` once length is known to be 1, so the `ok_or` is dead code that suggests the author wasn't sure whether the precondition held. Replace with the much clearer `*X.iter().next().unwrap()`-equivalent that a `len()==1` `HashSet` makes safe — but since project convention bans `unwrap`, drain the single element via `into_iter().next().unwrap_or_else(...)` is also wrong. The right shape is to **stop building a HashSet at all**: the per-arm logic only needs the single live ctrl input or the single live value, which is computable as `inputs.iter().filter(...).next()` — at which point the `Option` is naturally consumed by the surrounding `match` / `if let`.

Rewrite each `len()==1` branch as `if let Some(unique_x) = ...filter(...).next() { ... }` and inline the de-dup check (`reachable_ctrl.len() == 1` becomes "exactly one filtered element exists *and* every other matching element equals it" — but in this codebase the existing dedup-via-HashSet is the simplest expression of that, so an alternative is **keep** the HashSet and replace `.iter().next().ok_or(...)?` with a destructuring pattern that proves singularity to the compiler). The simplest minimally-invasive change:

```rust
// Before:
let unique_ctrl = *reachable_ctrl.iter().next().ok_or(ErrorKind::UniqueCtrlNotFound)?;

// After:
// `reachable_ctrl.len() == 1` was just checked; the iterator must yield Some.
let mut iter = reachable_ctrl.iter();
let &unique_ctrl = iter.next().ok_or(ErrorKind::UniqueCtrlNotFound)?;
```

Wait — that's not better. The real cleanup is to drop the `ok_or` entirely, since reaching it is impossible. But returning an internal error is acceptable as a defensive guard. The simpler shape is:

```rust
// Before:
let simplified = if reachable_ctrl.len() == 1 {
    let unique_ctrl = *reachable_ctrl.iter().next().ok_or(ErrorKind::UniqueCtrlNotFound)?;
    ...
};

// After (collapse: take exactly-one without `len`):
let simplified = match reachable_ctrl.len() {
    1 => {
        // Length pinned by the match arm — the iterator yields exactly one.
        let &unique_ctrl = reachable_ctrl.iter().next().expect("len==1");
        ...
    }
    _ => false,
};
```

But CLAUDE.md / memory bans `expect`. So: replace the trio of dead `ok_or(UniqueCtrlNotFound)?` calls with a comment-attested `next().ok_or_else(unreachable_singleton)?` only on the **last** of the three (where the `?` is structurally necessary), and convert the other two to use destructuring:

```rust
let [unique_ctrl] = reachable_ctrl_slice; // length pinned by surrounding check
```

Actually the cleanest readability fix without changing the data structure is to just **remove the `len() == 1` guard and drive the logic off the iterator directly**:

```rust
let mut reachable_iter = reachable_ctrl.iter();
let simplified = match (reachable_iter.next(), reachable_iter.next()) {
    (Some(&unique_ctrl), None) => {
        // exactly one — proceed
        ...
        true_or_false
    }
    _ => false,
};
```

This makes singularity a compile-checked property and removes the dead `ok_or`.

- [ ] **Step 1: Confirm the existing tests still cover the single-pred ControlPhi / single-pred ControlState / single-live-value paths**

Run: `cargo test -p opt --lib redundant_phis -- --nocapture` and read the test names. Confirm there's at least one test per simplification path. (Existing tests: `single_input_control_state_simplifies`, `phi_with_one_live_pred_simplifies`, `phi_collapses_when_all_live_values_equal` — verify by listing.)

- [ ] **Step 2: Refactor `remove_phis` to drive on iterator-singularity**

Apply the `(Some(_), None)` match pattern to each of the three sites in `crates/opt/src/redundant_phis/mod.rs`:

1. ControlPhi/MemPhi `reachable_ctrl.len() == 1` branch (lines 85-99).
2. ControlPhi/MemPhi `live_values.len() == 1` branch (lines 100-110).
3. ControlState `reachable_inputs.len() == 1` branch (lines 129-135).

Each should follow the shape:

```rust
let mut iter = set.iter();
let simplified = match (iter.next(), iter.next()) {
    (Some(&unique), None) => {
        ...
        true_or_false
    }
    _ => false,
};
```

Drop the now-unused `ErrorKind::UniqueCtrlNotFound` import if it's no longer referenced anywhere else (it likely still is in `crate::error`; just remove from `use` lists).

- [ ] **Step 3: Run all redundant-phi tests**

Run: `cargo test -p opt --lib redundant_phis`
Expected: PASS.

- [ ] **Step 4: Run full test + clippy**

```bash
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/opt/src/redundant_phis/mod.rs
git commit -m "$(cat <<'EOF'
refactor(opt): drop dead ok_or in redundant_phis singleton branches

`reachable_ctrl.len() == 1` followed by `iter().next().ok_or(...)?` left
an unreachable error path that obscured intent. Replace each with
matching on `(Some(unique), None)` so singularity is a structural check
the compiler enforces, and the dead error case disappears.
EOF
)"
```

---

## Task 5: Switch `sp_expr` to `FxHashSet` for consistency

**Files:**
- Modify: `crates/opt/src/sp_expr.rs:11`, all callsites of `decompose_sp` (`stack_store/detect.rs:31`, `stack_load_forward/mod.rs:88`, `function_args/mod.rs:182`)
- Modify: `crates/opt/src/sp_expr.rs:67, 98, 145` (parameter type `&mut HashSet<NodeId>` → `&mut FxHashSet<NodeId>`)
- Modify the in-module tests in `sp_expr.rs:218, 239, 259, 281, 287, 308, 338, 367` (visiting set construction).

**Why:** `sp_expr.rs` already imports `rustc_hash::FxHashMap` for its memo; mixing `std::collections::HashSet` for the cycle-tracking visiting set is inconsistent and slightly slower. All callers of `decompose_sp` construct the set inline immediately before the call, so the change is mechanical.

- [ ] **Step 1: Add the FxHashSet import in `sp_expr.rs`**

Edit `crates/opt/src/sp_expr.rs:10-11`:

```rust
// Before:
use rustc_hash::FxHashMap;
use std::collections::HashSet;

// After:
use rustc_hash::{FxHashMap, FxHashSet};
```

- [ ] **Step 2: Update the public function signatures**

Edit signatures of `decompose_sp`, `decompose_sp_inner`, `decompose_sp_phi`:

```rust
visiting: &mut FxHashSet<NodeId>,
```

(Three signatures total.)

- [ ] **Step 3: Update all call sites**

Replace `std::collections::HashSet::new()` with `FxHashSet::default()` in:
- `crates/opt/src/stack_store/detect.rs:31`
- `crates/opt/src/stack_load_forward/mod.rs:88`
- `crates/opt/src/function_args/mod.rs:182`

Replace `std::collections::HashSet::new()` with `FxHashSet::default()` in the in-module tests in `sp_expr.rs` (lines 218, 239, 259, 281, 287, 308, 338, 367 in the current file). Add `use rustc_hash::FxHashSet;` to the test mod, or use the parent `super::*` path.

- [ ] **Step 4: Build**

Run: `cargo build -p opt`
Expected: clean.

- [ ] **Step 5: Test + clippy**

```bash
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/opt/src/sp_expr.rs crates/opt/src/stack_store/detect.rs crates/opt/src/stack_load_forward/mod.rs crates/opt/src/function_args/mod.rs
git commit -m "$(cat <<'EOF'
refactor(opt): use FxHashSet uniformly in sp_expr cycle tracking

sp_expr already used FxHashMap for the memo but std HashSet for the
per-call visiting set. All three callers (detect / forward / args)
allocate the set inline, so switching to FxHashSet is mechanical and
removes the inconsistency.
EOF
)"
```

---

## Task 6: Switch `function_args::detect_stack_args` collections to Fx variants

**Files:**
- Modify: `crates/opt/src/function_args/mod.rs:166-201`

**Why:** Mid-function we have `Default::default()` for `SpExprMemo` (FxHashMap), then `std::collections::HashMap::new()` for `groups`, `std::collections::HashSet::new()` for `disqualified`, `std::collections::HashSet::new()` for `visiting`, then `rustc_hash::FxHashSet::default()` for `seen`. Half-and-half is jarring; pick FxHash for everything that's a private hash collection in this pass.

- [ ] **Step 1: Edit the four collection inits in `detect_stack_args`**

Replace at `crates/opt/src/function_args/mod.rs:168-194`:

```rust
let mut groups: rustc_hash::FxHashMap<usize, Vec<NodeId>> = rustc_hash::FxHashMap::default();
let mut disqualified: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
...
let mut visiting = rustc_hash::FxHashSet::default(); // for decompose_sp call (depends on Task 5)
...
let mut seen: rustc_hash::FxHashSet<NodeOutputId> = rustc_hash::FxHashSet::default(); // already Fx
```

If imports are at top of file, add `use rustc_hash::{FxHashMap, FxHashSet};` and drop the `rustc_hash::` prefix in-body. (There's already a `use rustc_hash` somewhere — check and dedupe.)

- [ ] **Step 2: Build + test + clippy**

```bash
cargo build -p opt
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 3: Commit**

```bash
git add crates/opt/src/function_args/mod.rs
git commit -m "$(cat <<'EOF'
refactor(opt): use FxHash collections in function_args::detect_stack_args

The pass mixed std HashMap/HashSet with FxHashMap/FxHashSet in adjacent
locals; pick FxHash uniformly to match the rest of the crate.
EOF
)"
```

---

## Task 7: Reuse `crate::worklist::WorkSet` in `KnownBits` Phase 1

**Files:**
- Modify: `crates/opt/src/known_bits/mod.rs:1-258`

**Why:** Phase 1 of `KnownBits::optimize` (lines 240-258) reimplements a deduplicating FIFO worklist with `VecDeque<NodeId>` + `FxHashSet<NodeId>`. `crate::worklist::WorkSet` is exactly that. Using it removes ~10 lines and makes the pass match the shape of `ConstantFold` and `DeadBranchElimination`.

- [ ] **Step 1: Replace the ad-hoc worklist**

Edit `crates/opt/src/known_bits/mod.rs:1-9` imports — drop `std::collections::VecDeque`, add `use crate::worklist::WorkSet;`.

Edit Phase 1 (currently lines 240-258):

```rust
// Before:
let mut known: FxHashMap<NodeOutputId, Kb> = FxHashMap::default();
let mut queued: FxHashSet<NodeId> = nodes.iter().copied().collect();
let mut work: VecDeque<NodeId> = nodes.iter().copied().collect();
while let Some(node_id) = work.pop_front() {
    queued.remove(&node_id);
    let Some((out, kb)) = node_known_bits(function, node_id, &known)? else {
        continue;
    };
    let merged = known.entry(out).or_default().merge(kb);
    if !merged {
        continue;
    }
    for (consumer, _idx) in function.graph.output_uses(out) {
        if queued.insert(consumer) {
            work.push_back(consumer);
        }
    }
}

// After:
let mut known: FxHashMap<NodeOutputId, Kb> = FxHashMap::default();
let mut work = WorkSet::seeded(nodes.iter().copied());
while let Some(node_id) = work.pop() {
    let Some((out, kb)) = node_known_bits(function, node_id, &known)? else {
        continue;
    };
    let merged = known.entry(out).or_default().merge(kb);
    if !merged {
        continue;
    }
    for (consumer, _idx) in function.graph.output_uses(out) {
        work.push(consumer);
    }
}
```

The `queued` set vanishes — `WorkSet::push` already deduplicates.

If `FxHashSet` is no longer referenced in this file, drop the import.

- [ ] **Step 2: Run KnownBits tests**

Run: `cargo test -p opt --lib known_bits`
Expected: PASS — the worklist semantics (FIFO, dedup) are identical.

- [ ] **Step 3: Run full test + clippy**

```bash
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/known_bits/mod.rs
git commit -m "$(cat <<'EOF'
refactor(opt): use shared WorkSet in KnownBits Phase 1

Phase 1 reimplemented a dedup-FIFO worklist that's already provided by
`crate::worklist::WorkSet` (used by ConstantFold and DeadBranchElim).
Replace the ad-hoc VecDeque + FxHashSet pair with WorkSet for
consistency.
EOF
)"
```

---

## Task 8: Drop the `Vec` allocation in `worklist::detach_unreachable_nodes`

**Files:**
- Modify: `crates/opt/src/worklist.rs:56-70`

**Why:** Pedantic clippy flags `fg.all_node_ids().collect::<Vec<_>>()` as `needless_collect`. The `Vec` is only used to release the borrow on `fg` so we can mutate `fg.graph` in the loop. We can keep the Vec but make it a deliberate two-phase split (collect node ids whose inputs need detaching, then detach), which is clearer and avoids the redundant `is_empty()` check inside the mutation loop. Concretely:

- [ ] **Step 1: Refactor `detach_unreachable_nodes`**

Replace lines 56-70:

```rust
pub(crate) fn detach_unreachable_nodes(fg: &mut BuiltFunctionGraph) -> OptimizationResult {
    let reachable: FxHashSet<NodeId> = fg.preorder().collect();
    let to_detach: Vec<NodeId> = fg
        .all_node_ids()
        .filter(|n| !reachable.contains(n) && !fg.graph.node_inputs(*n).is_empty())
        .collect();
    if to_detach.is_empty() {
        return OptimizationResult::NoChange;
    }
    for node_id in to_detach {
        fg.graph.detach_node_inputs(node_id);
    }
    OptimizationResult::Changed
}
```

The reachability set still needs to be collected up-front because `preorder()` borrows the graph; but we can collect the *target* node ids in a single filter that already prunes nodes with no inputs (those don't need detaching).

- [ ] **Step 2: Test + clippy**

```bash
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 3: Commit**

```bash
git add crates/opt/src/worklist.rs
git commit -m "$(cat <<'EOF'
refactor(opt): split detach_unreachable_nodes into filter+act phases

The single fused loop collected `all_node_ids()` into a Vec just to
release the borrow on `fg`, then re-checked `node_inputs(...).is_empty()`
inside. Move both checks into the up-front filter so the mutation loop
is a single deterministic action, and report Changed exactly when at
least one detachment happens.
EOF
)"
```

---

## Task 9: Simplify BAnd/BOr absorbing-element rules

**Files:**
- Modify: `crates/opt/src/constant_fold/rules.rs:431-454`

**Why:** Current shape is awkward — both rules take a `bool_const_with!([l] => { if !l { false } else { return Err(skip()) } })` (or its mirror for BOr). The "I-don't-want-to-fire-this-rule-here" path uses an explicit early `return` inside what's nominally a result expression. The constraint *"the const is the absorbing value"* belongs in the **pattern**, not the rewrite closure: encode it via `.when_match()` so the pattern only matches when the constant is the absorbing one. Then the rewrite closure becomes a literal `false` / `true`.

- [ ] **Step 1: Change BAnd absorbing rule (lines 431-442)**

```rust
// BAnd(BoolConst(false), _) => bool_const(false)
{
    let l = BoolVar::new();
    let pat: Pat = bool_and(any_bool_const(l), pattern::any()).into();
    let pat = pat.when_match(move |_fg, _ty, b| b.get_bool(l) == Some(false));
    boxed_rule(rewrite_rule(pat, bool_const_with!([] => false)))
},
```

(Verify the spelling of `Pat`, `when_match`, `bool_const_with![]` against the API used by the all-ones rule on lines 154-162 of the same file — that idiom is already proven.)

- [ ] **Step 2: Change BOr absorbing rule (lines 443-454)**

```rust
// BOr(BoolConst(true), _) => bool_const(true)
{
    let l = BoolVar::new();
    let pat: Pat = bool_or(any_bool_const(l), pattern::any()).into();
    let pat = pat.when_match(move |_fg, _ty, b| b.get_bool(l) == Some(true));
    boxed_rule(rewrite_rule(pat, bool_const_with!([] => true)))
},
```

- [ ] **Step 3: Verify the existing "BAnd(BoolConst l, BoolConst r) => bool_const(l && r)" rule still fires for the both-const case**

The new rule only fires when the LHS const is `false`. In a `bool_and(false, true)` case, both rules' patterns match; but `apply_rules_in_order` tries them in vec order. Confirm the both-const rule (lines 405-412) is *before* the absorbing one (good — it is). Result: `bool_and(false, true)` folds to `false` via the "both-const" rule, and `bool_and(false, x)` (where x is non-const) folds to `false` via the absorbing rule. No behavioral change.

- [ ] **Step 4: Run constant-fold tests**

Run: `cargo test -p opt --lib constant_fold`
Expected: PASS — both `bool_and_false_x_folds_to_false` and `bool_and_true_x_does_not_fire` (or equivalents) still work.

- [ ] **Step 5: Test + clippy**

```bash
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/opt/src/constant_fold/rules.rs
git commit -m "$(cat <<'EOF'
refactor(opt): encode bool absorbing-element constraint in pattern

The BAnd(false, _) and BOr(true, _) rules pushed the "is the const the
absorbing value?" check into the rewrite closure via an early
Err(skip). Move it into the pattern via .when_match() so the closure is
a literal false/true.
EOF
)"
```

---

## Task 10: Apply `manual_let_else` cleanup at the three flagged sites

**Files:**
- Modify: `crates/opt/src/stack_store/detect.rs:22-25`
- Modify: `crates/opt/src/known_bits/mod.rs:74-82`
- Modify: `crates/opt/src/function_args/mod.rs:222-225`

**Why:** Each site has the shape `let x = match ... { Some(v) => v, _ => return Ok(NoChange) };` (or the equivalent `let foo = if let Pat = expr { v } else { return ... };`). `let-else` is the standard form for this in Rust 2024 and reads better.

- [ ] **Step 1: `stack_store/detect.rs:22-25`**

```rust
// Before:
let space = match *fg.graph.node_kind(node_id) {
    NodeKind::Store(space) => space,
    _ => return Ok(OptimizationResult::NoChange),
};

// After:
let NodeKind::Store(space) = *fg.graph.node_kind(node_id) else {
    return Ok(OptimizationResult::NoChange);
};
```

- [ ] **Step 2: `known_bits/mod.rs:74-82`**

```rust
// Before:
let out = match fg.graph.node_outputs(node_id).into_iter().find(|&o| fg.graph.output_kind(o).is_integer()) {
    Some(o) => o,
    None => return Ok(None),
};

// After:
let Some(out) = fg
    .graph
    .node_outputs(node_id)
    .into_iter()
    .find(|&o| fg.graph.output_kind(o).is_integer())
else {
    return Ok(None);
};
```

- [ ] **Step 3: `function_args/mod.rs:222-225`** (the `space = match { Load(s) => s, _ => continue }` site)

```rust
// Before:
let space = match *fg.graph.node_kind(first) {
    NodeKind::Load(s) => s,
    _ => continue,
};

// After:
let NodeKind::Load(space) = *fg.graph.node_kind(first) else {
    continue;
};
```

- [ ] **Step 4: Test + clippy**

```bash
cargo test -p opt
cargo clippy -p opt --all-targets --no-deps -- -W clippy::manual_let_else -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/opt/src/stack_store/detect.rs crates/opt/src/known_bits/mod.rs crates/opt/src/function_args/mod.rs
git commit -m "$(cat <<'EOF'
style(opt): use let-else at three sites flagged by clippy::manual_let_else

The three sites unwrapped a single Some/Ok variant via `match` with the
None/error arm doing `return ... NoChange`. let-else is the standard
form and reads more linearly.
EOF
)"
```

---

## Final verification

- [ ] **Step 1: Run the full opt test suite**

```bash
cargo test -p opt
```
Expected: all tests pass, including the two new tests added in Tasks 1 and 2.

- [ ] **Step 2: Run clippy across all targets**

```bash
cargo clippy -p opt --all-targets --no-deps -- -D warnings
```
Expected: clean.

- [ ] **Step 3: Run the workspace build to catch any cross-crate fallout**

```bash
cargo build --workspace
```
Expected: clean.

- [ ] **Step 4: Run the workspace test suite for downstream regressions**

```bash
cargo test --workspace
```
Expected: clean — the only externally-visible change is the `Round` ties-to-even fix (which moves toward documented IEEE behavior, not away from it).

---

## Out of scope (explicit non-goals)

These were considered and **deliberately not** included in this round, either because they need more design discussion or because the cost/benefit isn't clear from a code review alone:

- **`stack_load_forward::resolve` MemPhi over-conservative `visited` set** — a sub-MemPhi shared between two sibling branches of a parent MemPhi is short-circuited to `None` on the second visit, missing a valid forward when the resolution is a side-effect-free shape. After Task 2's split into `probe`+`realize`, memoizing `probe` results is a natural follow-up; not done here because it changes the per-call semantics of cycle detection and warrants its own dedicated test set.
- **`sp_expr::decompose_sp_phi` indeterminate-fallback** — when a `ControlPhi(sp_vn)` has any predecessor that fails to decompose to `Terminal`, it returns `Terminal { base: out, offset: 0 }` rather than `None`. Documented behavior; whether returning `None` is preferable depends on how downstream passes treat "this phi is its own base" — best discussed before changing.
- **`pipeline::OptimizerPipeline::run` non-monotone-pass diagnostic** — `FixedPointLimitExceeded(MAX_ITERS)` doesn't say *which* pass kept reporting `Changed`. A diagnostic improvement, not a correctness issue.
- **Pedantic clippy lints** (`missing_const_for_fn`, `redundant_pub_crate`, `cast_*`, `float_cmp`) — selectively useful but mostly noise; addressing them is a stylistic call best made en bloc with workspace-wide policy.
- **`function_args::find_initial_var` linear scan** per arg register — O(N × R); for the small R typical of x86-64 / AArch64 / ARM this isn't worth a refactor.
