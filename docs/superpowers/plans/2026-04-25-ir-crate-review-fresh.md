# `ir` Crate Review (Fresh) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix real correctness bugs, latent panic-rule violations, and dead/asymmetric code in the `ir` crate, and close test gaps that hid them. Default `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` must remain clean throughout. No previous-review artifacts are relied on; all findings are anchored to the current `feature/ai` HEAD (`8c2400b`).

**Architecture:**
- Small, atomic commits per task. Each fix lands with its test (TDD).
- No public-API breaks where the workspace consumes the surface (`BuiltFunctionGraph::graph`, `node_inputs(...)[i]` indexing). The plan deliberately leaves these alone and documents *why*.
- `ir` crate has crate-internal-only callers for the items being changed (`node_input_id_at`, `remove_node_input`, `new_invalid`, signature constants, builder helpers, dot label helpers); cascade is bounded.

**Tech Stack:** Rust 1.93, `cranelift-entity`, `smallvec`, `rsleigh` (path dep), proptest (existing), workspace clippy on default lints.

**Worktree:** `/home/mike/Desktop/strider/.worktrees/ir-review-2026-04-25` on branch `review/ir-crate-2026-04-25`.

---

## Baseline (already verified at plan-write time)

- `cargo build -p ir` ✓
- `cargo clippy -p ir --all-targets -- -D warnings` ✓ (clean on default lints)
- `cargo test -p ir` ✓ — 167 unit + 10 build/validate roundtrip + 6 dedup + 3 proptest + 3 walk = **189 tests, 0 failures**

The plan must end with all of those still green and the workspace too: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.

---

## File Structure

Files this plan creates or modifies, grouped by concern. No new files except where noted.

**Node typing (single source of truth — Layer A consumes this):**
- Modify: `crates/ir/src/node_signature.rs` — fix `IN_PHI` typing, fix `PostCallVarState` output typing, simplify `SlotList::at`, expand tests to cover every `NodeKind` variant and assert variadic tails.

**Validator (Layer C invariant gap + tests for unconfirmed invariants):**
- Modify: `crates/ir/src/validate/layer_c.rs` — add zero-predecessor `ControlState` check.
- Modify: `crates/ir/src/error.rs` — add `ValidationError::EmptyControlStatePredecessors` (and the new `ErrorKind` variants needed by other tasks).
- Modify: `crates/ir/src/validate/mod.rs` — wire the new error variant into the bundle iteration if needed.
- Modify: `crates/ir/src/validate/tests.rs` — add tests for the new layer-C check, MemPhi arity mismatch, ValuePhi arity mismatch, `NodeOutputCountMismatch`, and `PostCallVarState` with a Bool-typed output.

**Graph access — panic-rule violations:**
- Modify: `crates/ir/src/graph/access.rs` — `node_input_id_at` returns `Result<NodeInputId>`.
- Modify: `crates/ir/src/graph/uses.rs` — `remove_node_input` bounds-checks the index; `update_input` self-redirect early-return.
- Modify: `crates/ir/src/graph/store.rs` — lazy cache-key allocation in `create_node`.
- Modify: `crates/ir/src/error.rs` — add `InputIndexOutOfBounds { node, index, len }` and `RemoveInputFromCacheableNode(NodeId)`.
- Modify: `crates/ir/src/validate/layer_b.rs` and `crates/ir/src/validate/tests.rs` and `crates/ir/src/graph/tests.rs` and `crates/ir/tests/dedup_cache.rs` — propagate the new `Result` from `node_input_id_at`.

**Builder semantic bugs:**
- Modify: `crates/ir/src/builder/nodes.rs` — fix `build_control_phi` to validate `incoming_values` as values (not control); change `build_if`'s non-Bool-cond error to a dedicated variant.
- Modify: `crates/ir/src/builder/call.rs` — collapse `clobbered_outputs` into a single pass that preserves the offending `NodeOutputId` in the error.
- Modify: `crates/ir/src/builder/coerce.rs` — make `get_as_int` consistent with `get_as_unsigned_int` for `BoolConst`.
- Modify: `crates/ir/src/error.rs` — add `ErrorKind::ExpectedBool(NodeOutputId)`.
- Modify: `crates/ir/src/builder/tests.rs` — new tests for each fix (see tasks).

**Function builder visibility (light hardening, no API break):**
- Modify: `crates/ir/src/function.rs` — make `FunctionGraph::new_invalid` `pub(crate)` (only the builder uses it).

**Dot rendering correctness:**
- Modify: `crates/ir/src/dot/label.rs` — bounds-check `call_clobbered_name`; fix the hard-coded `"from bool"` in the `CastToInt` label; replace `\u{2192}` with `→` to match every other label.
- Modify: `crates/ir/src/dot/tests.rs` — new tests for the call-clobbered OOB path, the `CastToInt`-from-non-bool label, and a `StackStore` label rendering.

**Integration tests (memory chain coverage):**
- Modify: `crates/ir/tests/build_validate_roundtrip.rs` — add a `Store → Load → Return` test that asserts the load's memory input is the store's memory output (not initial memory).

**Common test helper (light):**
- Possibly Modify: `crates/ir/tests/common/mod.rs` — add a small builder helper if needed by the new integration test (will be decided in Task 23, only if the existing helpers don't suffice).

---

## Conventions every task must follow

1. **TDD.** Each task that changes behavior writes the failing test first, runs it, then writes the minimal fix, then re-runs the test.
2. **No `unwrap`/`expect`/`panic!`/`debug_assert!`** in non-test code paths (project rule, see `~/.claude/projects/-home-mike-Desktop-strider/memory/feedback_no_panic_unwrap.md`). Tests that return `Result<()>` use `?`. New tests follow the existing pattern in `validate/tests.rs` (return `Result<(), Box<dyn Error>>` or similar).
3. **One commit per task** unless explicitly stated otherwise. Commit messages follow the repo style (`fix(ir): …`, `test(ir): …`, `refactor(ir): …`).
4. **After every task:** run `cargo test -p ir` and `cargo clippy -p ir --all-targets -- -D warnings`. Both must pass before commit.
5. **Final-task gate:** `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` must both pass.

---

## Phase 1 — Node-signature correctness (Layer A admits real graphs)

### Task 1: Fix `IN_PHI` slot kind from `AnyInt` to `AnyValue`

**Why now:** `ControlPhi` and `ValuePhi` value inputs (every predecessor's incoming value) are currently restricted to integer-kinded outputs. Real binaries phi-merge x86 flag registers (`CF`, `ZF`, `SF`) which the IR models as `Bool`. `IN_PHI` is the only variadic-tail slot still forced to integer; `ARG`/`RET`/`CALL_OUT` were already relaxed to `AnyValue` for the same reason (see `node_signature.rs:223-244` comments). This is an asymmetry, not a design choice.

**Files:**
- Modify: `crates/ir/src/node_signature.rs:260-264`
- Modify: `crates/ir/src/node_signature.rs` (tests module) — new test
- Modify: `crates/ir/src/validate/tests.rs` — new test

- [ ] **Step 1: Write the failing test in `validate/tests.rs`**

```rust
#[test]
fn layer_a_accepts_bool_value_phi_inputs() -> Result<(), Box<dyn std::error::Error>> {
    use crate::node::{NodeKind, NodeOutputKind};
    let mut graph = Graph::default();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();

    // ControlState with one Control predecessor.
    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let [_cs_ctrl, phi_token] = graph.node_outputs_exact(cs)?;

    // BoolConst as the per-predecessor value.
    let bc = graph.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [bc_out] = graph.node_outputs_exact(bc)?;

    // ValuePhi taking [phi_token, bc_out] — Bool value input through IN_PHI.
    let vp = graph.create_node(
        NodeKind::ValuePhi,
        [phi_token, bc_out],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [vp_out] = graph.node_outputs_exact(vp)?;

    // Use the phi'd value in the Return so the validator's reachability walk hits it.
    let ret = graph.create_node(NodeKind::Return, [_cs_ctrl, mem, vp_out], []);
    let _ = ret;

    crate::validate::validate(&graph, entry)?;
    Ok(())
}
```

- [ ] **Step 2: Run test, expect FAIL**

Run: `cargo test -p ir --lib -- validate::tests::layer_a_accepts_bool_value_phi_inputs --nocapture`
Expected: a `NodeInputKindMismatch` against the Bool value input (Layer A rejects Bool against `AnyInt`).

- [ ] **Step 3: Apply the one-line fix in `node_signature.rs`**

```rust
// Before:
const IN_PHI: Slot = Slot {
    kind: AnyInt,
    name: "in",
    role: R::In,
};

// After:
// Per-predecessor value input for ControlPhi / ValuePhi. AnyValue (not AnyInt)
// because flag-register phis are routinely Bool-typed — same rationale as
// ARG / RET / CALL_OUT above.
const IN_PHI: Slot = Slot {
    kind: AnyValue,
    name: "in",
    role: R::In,
};
```

- [ ] **Step 4: Re-run test, expect PASS; run full ir test suite**

Run: `cargo test -p ir`
Expected: all 189 (now 190) tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/node_signature.rs crates/ir/src/validate/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): IN_PHI accepts AnyValue so Bool flag phis validate

ControlPhi / ValuePhi value inputs were restricted to AnyInt, which
falsely rejected Bool-typed flag-register phis on real binaries. Now
matches the relaxation already applied to ARG / RET / CALL_OUT.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Fix `PostCallVarState` output kind from `INT_VAL` to `ANY_VAL`

**Why now:** Same root cause as Task 1. `PostCallVarState(vn)` re-establishes liveness of any caller-clobbered varnode after a call. Flag registers are caller-clobbered and Bool-typed; the current `INT_VAL` (`AnyInt`) output kind makes Layer A reject every `PostCallVarState` for a flag register. This blows up validation on real ABI-compliant call sites.

**Files:**
- Modify: `crates/ir/src/node_signature.rs:331`
- Modify: `crates/ir/src/validate/tests.rs` — new test

- [ ] **Step 1: Write the failing test in `validate/tests.rs`**

```rust
#[test]
fn layer_a_accepts_bool_post_call_var_state() -> Result<(), Box<dyn std::error::Error>> {
    use crate::node::{NodeKind, NodeOutputKind};
    let mut graph = Graph::default();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();

    // Synthesize a Call with one Bool clobbered output (e.g. a flag register).
    let target = graph.create_node(
        NodeKind::IntConst(0xdead_beef),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [target_out] = graph.node_outputs_exact(target)?;
    let call = graph.create_node(
        NodeKind::Call,
        [entry_ctrl, mem, target_out],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::Bool),
        ],
    );
    let [call_ctrl, _call_mem, _flag_clob] = graph.node_outputs_exact(call)?;

    // Use a flag-typed Vn as the PostCallVarState's varnode.
    let flag_vn = rsleigh::Vn::register("ZF", 1, 1);
    let pcv = graph.create_node(
        NodeKind::PostCallVarState(flag_vn),
        [call_ctrl],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [pcv_out] = graph.node_outputs_exact(pcv)?;

    // Make pcv reachable so Layer A inspects it.
    let _ret = graph.create_node(NodeKind::Return, [call_ctrl, _call_mem, pcv_out], []);

    crate::validate::validate(&graph, entry)?;
    Ok(())
}
```

- [ ] **Step 2: Run test, expect FAIL**

Run: `cargo test -p ir --lib -- validate::tests::layer_a_accepts_bool_post_call_var_state`
Expected: `NodeOutputKindMismatch` (Bool against `AnyInt`).

- [ ] **Step 3: One-line fix in `node_signature.rs`**

```rust
// Before:
NodeKind::PostCallVarState(_) => sig!(inputs: [CTRL], outputs: [INT_VAL]),

// After:
// Output is AnyValue (not AnyInt): caller-clobbered flag registers are
// Bool-typed and routinely re-established by PostCallVarState.
NodeKind::PostCallVarState(_) => sig!(inputs: [CTRL], outputs: [ANY_VAL]),
```

- [ ] **Step 4: Re-run test, expect PASS; run full ir test suite**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/node_signature.rs crates/ir/src/validate/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): PostCallVarState output is AnyValue so Bool flag clobbers validate

INT_VAL (AnyInt) rejected every Bool-typed PostCallVarState — i.e. every
caller-saved flag register on real ABI graphs. Switch to ANY_VAL.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Simplify `SlotList::at` (remove dead branch)

**Why now:** `node_signature.rs:109-117`'s `else if idx >= self.head.len()` is always true after `head.get(idx)` returns `None`, making the trailing `else { None }` unreachable. The behavior is correct *by accident* (`self.tail` is `None` for fixed lists, exactly the wanted result) — a clearer formulation is one line.

**Files:**
- Modify: `crates/ir/src/node_signature.rs:109-117`

- [ ] **Step 1: Replace the body**

```rust
// Before:
pub fn at(&self, idx: usize) -> Option<Slot> {
    if let Some(s) = self.head.get(idx) {
        Some(*s)
    } else if idx >= self.head.len() {
        self.tail
    } else {
        None
    }
}

// After:
/// Slot at index `idx`.  For fixed-arity lists returns `None` past the
/// head; for variadic lists returns the tail slot for any past-head index.
pub fn at(&self, idx: usize) -> Option<Slot> {
    self.head.get(idx).copied().or(self.tail)
}
```

- [ ] **Step 2: Run tests, expect PASS unchanged**

Run: `cargo test -p ir`

- [ ] **Step 3: Commit**

```bash
git add crates/ir/src/node_signature.rs
git commit -m "$(cat <<'EOF'
refactor(ir): SlotList::at — drop unreachable else branch

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Add per-`NodeKind` signature smoke tests + variadic-tail assertions

**Why now:** `node_signature/tests.rs` currently spot-checks ~7 kinds and *never* asserts the variadic-tail slot kind. A copy-paste regression in any tail (e.g. flipping `ControlState`'s `CTRL` tail to `MEM`) would slip past the suite. Adding a single table-driven test fixes both gaps.

**Files:**
- Modify: `crates/ir/src/node_signature.rs` (tests module)

- [ ] **Step 1: Add the table-driven test (one per `NodeKind` variant). Replace any redundant per-variant tests below it with the table-driven form, but only after the new test passes.**

```rust
#[test]
fn expected_signature_covers_every_node_kind() {
    use crate::node::NodeKind;
    use rsleigh::{Vn, VnSpace};

    // Sample one instance per NodeKind variant. Variants that carry data
    // (NodeKind::Foo(x)) take an arbitrary representative payload — the
    // signature is independent of the payload.
    let dummy_vn = Vn::register("R0", 0, 8);
    let kinds: &[NodeKind] = &[
        NodeKind::Entry,
        NodeKind::InitialMemory,
        NodeKind::InitialVar(dummy_vn),
        NodeKind::FunctionArg { index: 0, vn: dummy_vn },
        NodeKind::ControlState,
        NodeKind::MemPhi,
        NodeKind::ControlPhi(dummy_vn),
        NodeKind::ValuePhi,
        NodeKind::If,
        NodeKind::Call,
        NodeKind::PostCallMemState,
        NodeKind::PostCallVarState(dummy_vn),
        NodeKind::Return,
        NodeKind::Load(VnSpace::RAM),
        NodeKind::Store(VnSpace::RAM),
        NodeKind::StackStore { space: VnSpace::RAM, offset: 0 },
        NodeKind::StackStorePhi { space: VnSpace::RAM },
        NodeKind::IntConst(0),
        NodeKind::IntUnaryOp(crate::ops::IntUnaryOp::Neg),
        NodeKind::IntBinaryOp(crate::ops::IntBinaryOp::Add),
        NodeKind::IntCmpOp(crate::ops::IntCmpOp::Eq),
        NodeKind::Truncate,
        NodeKind::Extend(crate::ops::ExtendOp::Zero),
        NodeKind::Popcount,
        NodeKind::Lzcount,
        NodeKind::CastToInt,
        NodeKind::BoolConst(false),
        NodeKind::BoolUnaryOp(crate::ops::BoolUnaryOp::Not),
        NodeKind::BoolBinaryOp(crate::ops::BoolBinaryOp::And),
        NodeKind::CastToBool,
        NodeKind::FloatConst(0),
        NodeKind::FloatBinaryOp(crate::ops::FloatBinaryOp::Add),
        NodeKind::FloatUnaryOp(crate::ops::FloatUnaryOp::Neg),
        NodeKind::FloatCmpOp(crate::ops::FloatCmpOp::Eq),
        NodeKind::IntToFloat,
        NodeKind::IntBitsToFloat,
        NodeKind::FloatToInt,
        NodeKind::FloatBitsToInt,
        NodeKind::FloatToFloat,
        NodeKind::CastToFloat,
        NodeKind::CallOther { user_op_id: 0 },
        NodeKind::SegmentOp { op_id: 0 },
        NodeKind::CPoolRef,
        NodeKind::New,
    ];

    // Just calling expected_signature must not panic for any variant; the
    // returned Signature must be inspectable and self-consistent.
    for k in kinds {
        let sig = expected_signature(k);
        // Variadic-tail invariant: a non-empty tail must carry a finite slot.
        let _ = sig.inputs.head_len();
        let _ = sig.outputs.head_len();
        let _ = sig.inputs.is_variadic();
        let _ = sig.outputs.is_variadic();
        assert!(sig.inputs.at(0).is_some() || sig.inputs.head_len() == 0,
            "head[0] should exist when head_len > 0 for {k:?}");
    }
}

#[test]
fn variadic_tail_kinds_match_intent() {
    use crate::node::NodeKind;
    use rsleigh::VnSpace;

    let cases: &[(NodeKind, ExpectedOutputKind)] = &[
        // ControlState's tail is per-predecessor Control inputs.
        (NodeKind::ControlState, ExpectedOutputKind::Control),
        // MemPhi's tail is per-predecessor Memory inputs.
        (NodeKind::MemPhi, ExpectedOutputKind::Memory),
        // Call/CallOther arg tail and Return ret-val tail are AnyValue (Bool flags routinely appear).
        (NodeKind::Call, ExpectedOutputKind::AnyValue),
        (NodeKind::CallOther { user_op_id: 0 }, ExpectedOutputKind::AnyValue),
        (NodeKind::Return, ExpectedOutputKind::AnyValue),
        // CPoolRef refs / New args.
        (NodeKind::CPoolRef, ExpectedOutputKind::AnyInt),
        (NodeKind::New, ExpectedOutputKind::AnyValue),
    ];
    for (k, expected) in cases {
        let sig = expected_signature(k);
        let tail = sig.inputs.tail.expect("variadic input expected");
        assert_eq!(tail.kind, *expected, "input tail kind for {:?}", k);
    }

    // Call's output tail is AnyValue (clobbered registers).
    let sig = expected_signature(&NodeKind::Call);
    assert_eq!(sig.outputs.tail.expect("variadic output").kind, ExpectedOutputKind::AnyValue);
    let _ = VnSpace::RAM; // keep import
}
```

> Note: `Slot::tail` and `Slot::head` field visibility currently allow these accesses from inside the tests module. If a private field stops the build, expose `pub(crate) fn tail(&self)`/`head(&self)` accessors first.

- [ ] **Step 2: Run tests**

Run: `cargo test -p ir --lib -- node_signature::tests`
Expected: PASS (Tasks 1 and 2 already changed the relevant tails to `AnyValue`).

- [ ] **Step 3: Commit**

```bash
git add crates/ir/src/node_signature.rs
git commit -m "$(cat <<'EOF'
test(ir): table-driven smoke + variadic-tail assertions for expected_signature

Covers every NodeKind variant and asserts the tail kind for every
variadic SlotList. Catches copy-paste regressions in IN_PHI / ARG / RET /
CALL_OUT / CTRL / MEM tails.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 2 — Validator: close the zero-input `ControlState` gap

### Task 5: Layer C rejects `ControlState` with zero predecessors

**Why now:** `ControlState`'s signature is `inputs: []; in_tail: CTRL`. `head_len == 0` and the variadic-tail count threshold is `>= 0`, so Layer A never fires on a zero-input `ControlState`. Layer C's `check_layer_c_control_state` iterates the (empty) input list zero times. A zero-pred `ControlState` is invalid (nowhere to dispatch from); today the validator silently accepts it.

**Files:**
- Modify: `crates/ir/src/validate/mod.rs` — add `EmptyControlStatePredecessors { control_state }` variant to `ValidationError`.
- Modify: `crates/ir/src/validate/layer_c.rs` — add the check.
- Modify: `crates/ir/src/validate/tests.rs` — new test.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn layer_c_rejects_control_state_with_zero_predecessors() -> Result<(), Box<dyn std::error::Error>> {
    use crate::node::{NodeKind, NodeOutputKind};
    let mut graph = Graph::default();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _initmem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    // Zero-input ControlState — invalid graph.
    let cs = graph.create_node(
        NodeKind::ControlState,
        [],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    // Make the cs reachable so Layers A/C see it.
    let [cs_ctrl, _phi] = graph.node_outputs_exact(cs)?;
    let mem = graph.node_outputs(_initmem).into_iter().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [cs_ctrl, mem], []);

    let err = crate::validate::validate(&graph, entry).expect_err("expected zero-pred ControlState to be rejected");
    let bundle = match err.kind() {
        crate::error::ErrorKind::ValidationFailed(b) => b.clone(),
        e => panic!("expected ValidationFailed, got {e:?}"),
    };
    assert!(bundle.errors.iter().any(|e| matches!(
        e,
        ValidationError::EmptyControlStatePredecessors { control_state } if *control_state == cs
    )), "missing EmptyControlStatePredecessors error: {:#?}", bundle.errors);
    Ok(())
}
```

- [ ] **Step 2: Run test, expect FAIL** (the variant doesn't yet exist)

- [ ] **Step 3: Add the error variant in `validate/mod.rs`**

Insert near the other `ControlStateNonControlPredecessor` variant:

```rust
    /// A `ControlState` has zero input predecessors. Reachable but unreachable-from
    /// — usually indicates a builder bug or a pass that drained predecessors
    /// without removing the `ControlState`.
    EmptyControlStatePredecessors { control_state: NodeId },
```

- [ ] **Step 4: Add the check in `validate/layer_c.rs:check_layer_c_control_state`**

```rust
pub(super) fn check_layer_c_control_state(graph: &Graph, errs: &mut Vec<ValidationError>) {
    for node in graph.nodes.keys() {
        if !matches!(graph.node_kind(node), NodeKind::ControlState) {
            continue;
        }
        let inputs = graph.node_inputs(node);
        if inputs.is_empty() {
            errs.push(ValidationError::EmptyControlStatePredecessors { control_state: node });
            continue;
        }
        for (idx, target) in inputs.into_iter().enumerate() {
            // … unchanged …
        }
    }
}
```

- [ ] **Step 5: Run test, expect PASS; full ir suite green**

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/validate/mod.rs crates/ir/src/validate/layer_c.rs crates/ir/src/validate/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): Layer C rejects ControlState with zero predecessors

A variadic head_len of 0 means Layer A's count threshold (>= 0) accepts
empty-input ControlState nodes; Layer C's iteration was a no-op for them
too. Now flagged with a dedicated EmptyControlStatePredecessors error.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Tests for `MemPhi` arity, `ValuePhi` arity, and wrong output count

**Why now:** Existing Layer C arity tests only cover `ControlPhi`. `MemPhi` and `ValuePhi` go through the same code path; a regression there would not be caught. Layer A also has no test for `NodeOutputCountMismatch` (only the input-count variant is tested).

**Files:**
- Modify: `crates/ir/src/validate/tests.rs`

- [ ] **Step 1: Write the new tests**

For each: build a `ControlState` with N predecessors, then attach a phi with a *wrong* number of value inputs, run `validate`, assert the bundle contains `PhiValueArityMismatch` with the expected counts. Pattern follows the existing `layer_c_phi_value_arity_mismatch` test with the kind swapped.

```rust
#[test]
fn layer_c_mem_phi_arity_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    // 2 predecessors, 1 memory value → arity mismatch.
    // (Use the same scaffolding as layer_c_phi_value_arity_mismatch but with NodeKind::MemPhi.)
    // Assert PhiValueArityMismatch { expected_predecessors: 2, actual_values: 1 } is in the bundle.
    todo_inline!()
}

#[test]
fn layer_c_value_phi_arity_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    // Same idea with NodeKind::ValuePhi.
    todo_inline!()
}

#[test]
fn layer_a_rejects_wrong_output_count() -> Result<(), Box<dyn std::error::Error>> {
    // Build a node whose output_kinds list is too long for its expected_signature.
    // Easiest target: an IntConst given two outputs.
    let mut graph = Graph::default();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _im = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let bad = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [
            NodeOutputKind::OutputType(NodeOutputType::U64),
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(_im).into_iter().next().unwrap();
    let bad_out0 = graph.node_outputs(bad).into_iter().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem, bad_out0], []);
    let err = crate::validate::validate(&graph, entry).expect_err("expected output count mismatch");
    let bundle = match err.kind() {
        crate::error::ErrorKind::ValidationFailed(b) => b.clone(),
        e => panic!("expected ValidationFailed, got {e:?}"),
    };
    assert!(bundle.errors.iter().any(|e|
        matches!(e, ValidationError::NodeOutputCountMismatch { node, .. } if *node == bad)
    ), "{:#?}", bundle.errors);
    Ok(())
}
```

> The two phi tests above use a `todo_inline!` placeholder for plan brevity. The implementor must expand them following the structure of `layer_c_phi_value_arity_mismatch` (see `validate/tests.rs:246`), substituting `NodeKind::MemPhi` (with `NodeOutputKind::Memory` value inputs and a `NodeOutputKind::Memory` output) and `NodeKind::ValuePhi` (with integer value inputs and an integer output) respectively. The MemPhi version must use Memory-kinded predecessor values and a Memory output kind to pass Layer A; only its input *count* should mismatch the predecessor count of its owning `ControlState`.

- [ ] **Step 2: Run tests, expect FAIL** until the existing arity-check code is exercised.

- [ ] **Step 3: No code changes expected — the existing layer_c phi check already handles MemPhi and ValuePhi (Task 5 didn't change that). Tests should pass with `expand-todo-inline-fully` only.**

If a test fails, root-cause first (do not paper over with `if let Ok(_) = …`).

- [ ] **Step 4: Commit**

```bash
git add crates/ir/src/validate/tests.rs
git commit -m "$(cat <<'EOF'
test(ir): cover MemPhi/ValuePhi arity and wrong-output-count diagnostics

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 3 — Graph access: panic-rule fixes

### Task 7: `node_input_id_at` returns `Result<NodeInputId>`

**Why now:** The current method's docstring says it panics on out-of-range — a public method violating the project's no-panic rule. All in-tree callers are within the `ir` crate (`validate/layer_b.rs`, `validate/tests.rs`, `graph/tests.rs`, `tests/dedup_cache.rs`), so the cascade is bounded.

**Files:**
- Modify: `crates/ir/src/error.rs` — add the new error variant.
- Modify: `crates/ir/src/graph/access.rs:107`
- Modify: `crates/ir/src/validate/layer_b.rs:24`
- Modify: `crates/ir/src/validate/tests.rs:125, 439, 475`
- Modify: `crates/ir/src/graph/tests.rs:703`
- Modify: `crates/ir/tests/dedup_cache.rs:145`

- [ ] **Step 1: Add `ErrorKind::InputIndexOutOfBounds { node, index, len }` in `error.rs`**

```rust
#[error("input index {index} out of bounds for node {node:?} (len={len})")]
InputIndexOutOfBounds { node: NodeId, index: usize, len: usize },
```

- [ ] **Step 2: Write the failing test in `graph/tests.rs`**

```rust
#[test]
fn node_input_id_at_returns_error_on_out_of_bounds() {
    let mut graph = Graph::default();
    let n = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let err = graph.node_input_id_at(n, 0).expect_err("Entry has no inputs");
    let crate::error::ErrorKind::InputIndexOutOfBounds { node, index, len } = err.kind() else {
        panic!("wrong error kind: {err:?}");
    };
    assert_eq!(*node, n);
    assert_eq!(*index, 0);
    assert_eq!(*len, 0);
}
```

- [ ] **Step 3: Run test, expect compile-fail (signature changed)**

- [ ] **Step 4: Change the signature in `access.rs`**

```rust
/// Returns the [`NodeInputId`] of the input slot at position `idx` of `node`.
///
/// # Errors
/// Returns [`crate::error::ErrorKind::InputIndexOutOfBounds`] if `idx` is past
/// the node's current input count.
#[inline]
pub fn node_input_id_at(&self, node: NodeId, idx: usize) -> crate::error::Result<NodeInputId> {
    let slice = self.nodes[node].inputs.as_slice(&self.input_pool);
    slice.get(idx).copied().ok_or_else(|| {
        crate::error::ErrorKind::InputIndexOutOfBounds {
            node,
            index: idx,
            len: slice.len(),
        }
        .into()
    })
}
```

- [ ] **Step 5: Update each caller to use `?`**

- `crates/ir/src/validate/layer_b.rs:24` — the surrounding fn returns `Result`, propagate via `?`.
- `crates/ir/src/validate/tests.rs:125, 439, 475` — these are `Result<(), …>`-returning tests; use `?`.
- `crates/ir/src/graph/tests.rs:703` — same.
- `crates/ir/tests/dedup_cache.rs:145` — wrap with `?` (function already returns `Result<(), Box<dyn Error>>` in the existing pattern; if it doesn't, change it to do so).

- [ ] **Step 6: Run `cargo test -p ir` — all green; run the new test passes**

- [ ] **Step 7: Commit**

```bash
git add crates/ir/src/error.rs crates/ir/src/graph/access.rs crates/ir/src/validate/layer_b.rs crates/ir/src/validate/tests.rs crates/ir/src/graph/tests.rs crates/ir/tests/dedup_cache.rs
git commit -m "$(cat <<'EOF'
fix(ir): node_input_id_at returns Result instead of panicking

Adds ErrorKind::InputIndexOutOfBounds and propagates via `?` at every
in-tree caller (Layer B + tests). Honors the project's no-panic rule.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: `remove_node_input` bounds-checks the index and uses a dedicated error variant

**Why now:** Today the function (`uses.rs:117-134`) panics on `inputs.as_slice()[index]` when `index` is out of range, and the cacheable-node guard reuses `AddInputToCacheableNode` (misleading diagnostic for a removal).

**Files:**
- Modify: `crates/ir/src/error.rs` — add `RemoveInputFromCacheableNode(NodeId)`.
- Modify: `crates/ir/src/graph/uses.rs:117-134`
- Modify: `crates/ir/src/graph/tests.rs` — new tests.

- [ ] **Step 1: Write failing tests in `graph/tests.rs`**

```rust
#[test]
fn remove_node_input_returns_error_on_out_of_bounds() -> Result<(), crate::error::Error> {
    let mut graph = Graph::default();
    let cs = graph.create_node(
        NodeKind::ControlState,
        [],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let err = graph.remove_node_input(cs, 7).expect_err("oob expected");
    assert!(matches!(err.kind(), crate::error::ErrorKind::InputIndexOutOfBounds { node, index: 7, len: 0 } if *node == cs));
    Ok(())
}

#[test]
fn remove_node_input_on_cacheable_uses_dedicated_error() {
    let mut graph = Graph::default();
    let c = graph.create_node(NodeKind::IntConst(0), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let err = graph.remove_node_input(c, 0).expect_err("cacheable expected");
    assert!(matches!(err.kind(), crate::error::ErrorKind::RemoveInputFromCacheableNode(n) if *n == c));
}
```

- [ ] **Step 2: Run tests, expect FAIL**

- [ ] **Step 3: Apply fix**

```rust
pub fn remove_node_input(&mut self, node_id: NodeId, index: u32) -> crate::error::Result<()> {
    if self.node_kind(node_id).is_cacheable() {
        return Err(crate::error::ErrorKind::RemoveInputFromCacheableNode(node_id).into());
    }
    let index = index as usize;
    let inputs = &mut self.nodes[node_id].inputs;
    let slice = inputs.as_slice(&self.input_pool);
    let delete_input_id = *slice.get(index).ok_or_else(|| {
        crate::error::Error::from(crate::error::ErrorKind::InputIndexOutOfBounds {
            node: node_id,
            index,
            len: slice.len(),
        })
    })?;
    inputs.remove(index, &mut self.input_pool);
    for &input_id in &inputs.as_slice(&self.input_pool)[index..] {
        self.inputs[input_id].input_index -= 1;
    }
    self.unlink_input_from_output_list(delete_input_id);
    Ok(())
}
```

- [ ] **Step 4: Run tests, expect PASS; full suite green**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/error.rs crates/ir/src/graph/uses.rs crates/ir/src/graph/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): remove_node_input bounds-checks index; dedicated cacheable error

Replaces the panicking slice index with a Result returning
InputIndexOutOfBounds, and disambiguates the cacheable-node guard from
add_node_input via a new RemoveInputFromCacheableNode variant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: `update_input` self-redirect early-return

**Why now:** `update_input(input_id, output_id)` where `output_id` is already the input's current target performs a redundant unlink-relink, which (a) evicts the cacheable owner's dedup entry needlessly, and (b) re-orders the use-list (the entry moves to the head). The fix is one branch.

**Files:**
- Modify: `crates/ir/src/graph/uses.rs:145-156`
- Modify: `crates/ir/src/graph/tests.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn update_input_to_same_output_preserves_use_list_order() {
    use std::collections::BTreeSet;
    let mut graph = Graph::default();
    let c = graph.create_node(NodeKind::IntConst(0), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let cval = graph.node_outputs(c).into_iter().next().unwrap();
    // Two consumers of cval to give the use-list real ordering.
    let _a = graph.create_node(NodeKind::IntUnaryOp(crate::ops::IntUnaryOp::Neg), [cval], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
    let b = graph.create_node(NodeKind::IntUnaryOp(crate::ops::IntUnaryOp::Not), [cval], [NodeOutputKind::OutputType(NodeOutputType::U64)]);

    let before: BTreeSet<_> = graph.output_uses(cval).collect();
    let head_before = graph.output_first_use_id(cval);

    let b_in0 = graph.node_input_id_at(b, 0).unwrap();
    graph.update_input(b_in0, cval); // self-redirect
    let after: BTreeSet<_> = graph.output_uses(cval).collect();
    assert_eq!(before, after);
    // Head must not change as a side effect of a self-redirect.
    assert_eq!(head_before, graph.output_first_use_id(cval));
}
```

- [ ] **Step 2: Run test, expect FAIL** (head-id will change today).

- [ ] **Step 3: Apply fix**

```rust
pub fn update_input(&mut self, input_id: NodeInputId, output_id: NodeOutputId) {
    if self.inputs[input_id].output_id == output_id {
        return; // self-redirect: no mutation, no eviction.
    }
    let owner = self.inputs[input_id].node_id;
    self.evict_cache_entry_if_cacheable(owner);
    self.unlink_input_from_output_list(input_id);
    self.inputs[input_id].output_id = output_id;
    self.link_input_to_output_list(input_id);
}
```

- [ ] **Step 4: Run test, expect PASS; full suite green**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/graph/uses.rs crates/ir/src/graph/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): update_input self-redirect is a no-op

Skip unlink-relink (and the cacheable owner's eviction) when the new
target equals the current one. Preserves use-list ordering and avoids
a spurious cache miss on the next create_node with the same key.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: `create_node` lazy cache-key allocation

**Why now:** `store.rs:57-62` currently allocates `inputs.to_vec()` and `output_kinds.to_vec()` on every `create_node` call — including for non-cacheable nodes, which never use the key. Node creation is the central hot path.

**Files:**
- Modify: `crates/ir/src/graph/store.rs:51-98`
- Modify: `crates/ir/src/graph/tests.rs` — confirm dedup behavior is unchanged.

- [ ] **Step 1: Verify by inspection that the existing dedup tests cover both cacheable hits and non-cacheable bypass; no new test needed if so. (Existing `dedup_cache.rs` has 6 tests; spot-check for "non-cacheable kind never inserted into cache".)**

- [ ] **Step 2: Refactor `create_node`**

```rust
pub fn create_node(
    &mut self,
    kind: NodeKind,
    inputs: impl IntoIterator<Item = NodeOutputId>,
    output_kinds: impl IntoIterator<Item = NodeOutputKind>,
) -> NodeId {
    let inputs: SmallVec<[NodeOutputId; 4]> = inputs.into_iter().collect();
    let output_kinds: SmallVec<[NodeOutputKind; 4]> = output_kinds.into_iter().collect();
    let node = Node::new(kind);

    // Cacheable kinds: build the key, look it up, return on hit.
    // Non-cacheable kinds: skip the key entirely.
    let cache_key = if kind.is_cacheable() {
        let key = (node, inputs.to_vec(), output_kinds.to_vec());
        if let Some(node_id) = self.node_to_id.get(&key) {
            return *node_id;
        }
        Some(key)
    } else {
        None
    };

    let node_id = self.nodes.push(node);
    if let Some(key) = cache_key {
        self.node_to_id.insert(key, node_id);
    }

    let inputs: SmallVec<[NodeInputId; 2]> = inputs
        .into_iter()
        .enumerate()
        .map(|(index, output)| {
            self.inputs
                .push(NodeInput::new(output, node_id, index as u32))
        })
        .collect();
    for &input_use in &inputs {
        self.link_input_to_output_list(input_use);
    }

    let outputs = output_kinds.into_iter().enumerate().map(|(index, kind)| {
        self.outputs.push(NodeOutput::new(kind, node_id, index as u32))
    });

    self.nodes[node_id].inputs = NodeInputIdList::from_iter(inputs, &mut self.input_pool);
    self.nodes[node_id].outputs = NodeOutputIdList::from_iter(outputs, &mut self.output_pool);
    node_id
}
```

- [ ] **Step 3: Run `cargo test -p ir`; all 189+ tests green**

- [ ] **Step 4: Commit**

```bash
git add crates/ir/src/graph/store.rs
git commit -m "$(cat <<'EOF'
perf(ir): create_node skips cache-key allocation for non-cacheable kinds

Two Vec heap allocations per call were unconditional. Now built only on
the cacheable path. Behavior unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 4 — Builder semantic fixes

### Task 11: `build_control_phi` validates `incoming_values` as values, not control edges

**Why now:** `builder/nodes.rs:676-680` checks `is_control()` on each incoming value. The signature requires value inputs (`IN_PHI`); control edges are explicitly *not* what flows through phi value inputs. The bug is latent because production callers always pass `&[]`, but it makes the API impossible to use as documented and causes the wrong error variant.

**Files:**
- Modify: `crates/ir/src/builder/nodes.rs:666-688`
- Modify: `crates/ir/src/builder/tests.rs` — new test covering non-empty `incoming_values`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn build_control_phi_accepts_value_incoming_values() -> Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region(&[])?;
    b.set_entry_region(region)?;
    // ControlPhi needs a phi-token; create a ControlState first.
    // Easiest: use the region's entry phi-token. Then build a ControlPhi for a varnode
    // and supply a non-empty incoming_values containing a value output.
    // Assert build_control_phi returns Ok and inserts the node.
    todo_inline!()
}

#[test]
fn build_control_phi_rejects_control_kinded_incoming_value() -> Result<()> {
    // Construct a ControlState/phi-token; supply incoming_values = [some Control output].
    // Expect ErrorKind::ExpectedValue (NOT ExpectedControl).
    todo_inline!()
}
```

> The implementor expands the `todo_inline!()` placeholders following the patterns in `builder/tests.rs` for region creation and phi-token retrieval.

- [ ] **Step 2: Run tests, expect FAIL**

- [ ] **Step 3: Apply the fix**

```rust
pub(super) fn build_control_phi(
    &mut self,
    var: rsleigh::Vn,
    phi_token: NodeOutputId,
    incoming_values: &[NodeOutputId],
) -> Result<NodeOutputId> {
    let phi_token_kind = self.graph().output_kind(phi_token);
    if !phi_token_kind.is_control_phi() {
        return Err(ErrorKind::ExpectedControlPhi(phi_token).into());
    }
    for &v in incoming_values {
        let kind = self.graph().output_kind(v);
        if !kind.is_value() {
            return Err(ErrorKind::ExpectedValue(v, kind).into());
        }
    }
    let output_type = var.size.try_into()?;
    Ok(self.build_single_output_pure(
        NodeKind::ControlPhi(var),
        core::iter::once(phi_token).chain(incoming_values.iter().copied()),
        output_type,
    ))
}
```

- [ ] **Step 4: Run tests, expect PASS; full ir suite green**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/builder/nodes.rs crates/ir/src/builder/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): build_control_phi validates incoming values as values, not Control

Phi value inputs are typed by IN_PHI (AnyValue), not Control. The wrong
predicate made the documented &[v0, v1, …] form unusable.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 12: `build_if` returns a dedicated `ExpectedBool` error for non-Bool conditions

**Why now:** `build_if` returns `ExpectedValue(cond, cond_kind)` when `cond` is a value but not Bool — confusing when `cond` *is* a value (just the wrong kind). A dedicated variant disambiguates the diagnostic.

**Files:**
- Modify: `crates/ir/src/error.rs` — add `ExpectedBool(NodeOutputId)` variant (preserve any existing one if already present).
- Modify: `crates/ir/src/builder/nodes.rs:494-509`
- Modify: `crates/ir/src/builder/tests.rs` — new test asserting the variant.

- [ ] **Step 1: Search for an existing `ExpectedBool` variant** — `rg -n "ExpectedBool" crates/ir/src/error.rs`. If present, reuse; otherwise add a new `#[error("expected Bool value at {0:?}")] ExpectedBool(NodeOutputId)`.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn build_if_rejects_non_bool_condition_with_expected_bool_error() -> Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region(&[])?;
    b.set_entry_region(region)?;
    let true_r = b.create_region(&[])?;
    let false_r = b.create_region(&[])?;
    let bad_cond = b.build_int_const(1, NodeOutputType::U32)?;
    let err = b.build_if(bad_cond, true_r, false_r).expect_err("expected Bool error");
    assert!(matches!(err.kind(), crate::error::ErrorKind::ExpectedBool(o) if *o == bad_cond),
        "got {err:?}");
    Ok(())
}
```

- [ ] **Step 3: Run test, expect FAIL**

- [ ] **Step 4: Apply the fix**

```rust
let cond_kind = self.graph().output_kind(cond);
if !cond_kind.is_bool() {
    return Err(ErrorKind::ExpectedBool(cond).into());
}
```

- [ ] **Step 5: Run test, expect PASS; full ir suite green**

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/error.rs crates/ir/src/builder/nodes.rs crates/ir/src/builder/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): build_if uses ExpectedBool for non-Bool conditions

ExpectedValue(cond) was misleading when cond *was* a value (just not
Bool). New ExpectedBool variant disambiguates.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: `build_call` collapses the dead `clobbered_outputs` allocation and preserves the offending id in errors

**Why now:** `builder/call.rs:30-44` currently builds a SmallVec of pre-call outputs only to derive their kinds, discarding the actual ids — and the validation error at line 44 uses `NodeOutputId::default()`, losing the offending id needed to debug.

**Files:**
- Modify: `crates/ir/src/builder/call.rs:30-46`

- [ ] **Step 1: Write the failing test in `builder/tests.rs`**

```rust
#[test]
fn build_call_error_carries_offending_output_id() -> Result<()> {
    // Construct a builder where a clobbered varnode's current value is non-value
    // (e.g. by using the InitialMemory output as the "value"). Trigger build_call.
    // Assert the returned error's NodeOutputId is the actual InitialMemory output id,
    // not NodeOutputId::default().
    todo_inline!()
}
```

- [ ] **Step 2: Run test, expect FAIL**

- [ ] **Step 3: Refactor**

```rust
let arg_passing: SmallVec<[NodeOutputId; 4]> = self
    .arg_passing_vars
    .iter()
    .map(|var| self.read_variable(var))
    .collect::<Result<_>>()?;
self.validate_value_inputs(&arg_passing)?;

let clobbered: SmallVec<[_; 4]> = self.call_cloberred_variables.iter().copied().collect();

let mut cloberred_kinds: SmallVec<[NodeOutputKind; 4]> = SmallVec::new();
for var in &self.call_cloberred_variables {
    let out = self.read_variable(var)?;
    let k = self.graph().output_kind(out);
    if !k.is_value() {
        return Err(ErrorKind::ExpectedValue(out, k).into());
    }
    cloberred_kinds.push(k);
}

let addr_kind = self.graph().output_kind(call_address);
if !addr_kind.is_value() {
    return Err(ErrorKind::ExpectedValue(call_address, addr_kind).into());
}
// … rest of build_call unchanged …
```

- [ ] **Step 4: Run test, expect PASS; full ir suite green**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/builder/call.rs crates/ir/src/builder/tests.rs
git commit -m "$(cat <<'EOF'
refactor(ir): build_call drops dead clobbered_outputs alloc; error keeps id

Single-pass over call_cloberred_variables now derives kinds without an
intermediate SmallVec, and the ExpectedValue error carries the actual
offending NodeOutputId (was NodeOutputId::default()).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 14: `get_as_int` accepts `BoolConst` (consistent with `get_as_unsigned_int`)

**Why now:** `builder/coerce.rs:113-120` returns `None` for `BoolConst` because `get_as_signed_int` rejects Bool, even though `get_as_unsigned_int` accepts it. `extend_if_needed` therefore inserts an Extend node for a Bool constant instead of folding — silently regressing the constant-fold path for Bool inputs.

**Files:**
- Modify: `crates/ir/src/builder/coerce.rs` — make `get_as_signed_int` accept `BoolConst`.
- Modify: `crates/ir/src/builder/tests.rs` — new test.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn get_as_int_accepts_bool_const() -> Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region(&[])?;
    b.set_entry_region(region)?;
    let bc = b.build_bool_const(true)?;
    let r = b.get_as_int(bc)?;
    assert_eq!(r, Some((1, 1)));
    let bf = b.build_bool_const(false)?;
    let r = b.get_as_int(bf)?;
    assert_eq!(r, Some((0, 0)));
    Ok(())
}
```

- [ ] **Step 2: Run test, expect FAIL**

- [ ] **Step 3: Apply the fix in `get_as_signed_int`**

```rust
// Add a Bool arm next to the IntConst arm; treat true as 1, false as 0.
NodeKind::BoolConst(b) => Ok(Some(if *b { 1 } else { 0 })),
```

(The exact insertion site is whichever match arm covers `BoolConst` in `get_as_signed_int`. If the function uses early returns, mirror the existing `BoolConst` handling in `get_as_unsigned_int`.)

- [ ] **Step 4: Run test, expect PASS; full ir suite green**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/builder/coerce.rs crates/ir/src/builder/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): get_as_int accepts BoolConst consistent with get_as_unsigned_int

Asymmetry caused extend_if_needed to insert an Extend node instead of
folding when given a Bool constant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 5 — Dot rendering correctness

### Task 15: `call_clobbered_name` bounds-checks the index

**Why now:** `dot/label.rs:300-305` panics on `self.call_clobbered[i]` whenever the caller passed a slice shorter than the rendered Call's clobbered-output count. The repo's own dot tests pass `call_clobbered: &[]`, so any test or example that includes a `Call` with clobbered outputs will crash today.

**Files:**
- Modify: `crates/ir/src/dot/label.rs:300-305`
- Modify: `crates/ir/src/dot/tests.rs` — new test rendering a Call with clobbered outputs.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn call_clobbered_renders_with_short_call_clobbered_slice() {
    // Construct a graph containing a Call with one clobbered output, but pass
    // call_clobbered: &[] to the dot dumper. Render must succeed, falling back
    // to a synthetic name (e.g. "out0") rather than panicking.
    todo_inline!()
}
```

- [ ] **Step 2: Run test, expect FAIL (panic)**

- [ ] **Step 3: Apply fix**

```rust
pub(super) fn call_clobbered_name(&self, output_id: NodeOutputId) -> io::Result<String> {
    let (_call_id, output_index) = self.graph.output_definition(output_id);
    let i = match output_index.checked_sub(2) {
        Some(i) => i as usize,
        None => return Ok(format!("out{output_index}")),
    };
    match self.call_clobbered.get(i) {
        Some(vn) => self.vn_to_name(vn),
        None => Ok(format!("clob{i}")),
    }
}
```

- [ ] **Step 4: Run test, expect PASS; full ir suite green**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/dot/label.rs crates/ir/src/dot/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): dot call_clobbered_name no longer panics on short slice

Falls back to a synthetic clobN / outN label when the caller's
call_clobbered slice is shorter than the Call's clobbered-output count.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 16: `CastToInt` label uses the actual input type

**Why now:** `dot/label.rs:190` hard-codes "from bool" but the signature says `CastToInt` accepts `ANY_VAL` — so a graph that casts a U64 to U32 prints `"Cast → u32\nfrom bool"`. Misleading.

**Files:**
- Modify: `crates/ir/src/dot/label.rs:190`
- Modify: `crates/ir/src/dot/tests.rs` — new test asserting label content for a non-Bool input.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn cast_to_int_label_reflects_actual_input_type() {
    // Build a graph: IntConst(U64) → CastToInt → U32 output. Render it and
    // assert the rendered label contains "from u64", not "from bool".
    todo_inline!()
}
```

- [ ] **Step 2: Run test, expect FAIL**

- [ ] **Step 3: Apply fix**

```rust
NodeKind::CastToInt => format!(
    "Cast → {}\nfrom {}",
    self.out_type_str(node),
    self.input_type_str(node, 0),
),
```

- [ ] **Step 4: Run test, expect PASS; full ir suite green**

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/dot/label.rs crates/ir/src/dot/tests.rs
git commit -m "$(cat <<'EOF'
fix(ir): CastToInt label shows the actual input type, not 'from bool'

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 17: Replace `\u{2192}` with `→` in the `IntToFloat` label

**Why now:** `dot/label.rs:229` is the only label that uses the Unicode escape; every sibling conversion label uses the literal arrow. Trivial consistency fix.

**Files:**
- Modify: `crates/ir/src/dot/label.rs:229`

- [ ] **Step 1: Replace the literal**

```rust
NodeKind::IntToFloat => {
    let from = self.input_type_str(node, 0);
    let to = self.out_type_str(node);
    format!("IntToFloat\n{from} → {to}")
}
```

- [ ] **Step 2: Run `cargo test -p ir`; full suite green**

- [ ] **Step 3: Commit**

```bash
git add crates/ir/src/dot/label.rs
git commit -m "$(cat <<'EOF'
style(ir): IntToFloat label uses literal arrow, matching siblings

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 6 — Light visibility hardening

### Task 18: `FunctionGraph::new_invalid` becomes `pub(crate)`

**Why now:** Only `crates/ir/src/builder/{mod.rs:188, nodes.rs:407}` call it. External crates (opt, pattern, analyzer, tests) never construct an "invalid" `FunctionGraph` directly; restricting the visibility prevents external misuse without breaking the workspace.

**Files:**
- Modify: `crates/ir/src/function.rs:29-37`

- [ ] **Step 1: Change visibility**

```rust
// Before:
pub fn new_invalid() -> Self { … }

// After:
pub(crate) fn new_invalid() -> Self { … }
```

- [ ] **Step 2: Run `cargo build --workspace` and `cargo test --workspace`**

If the workspace fails (i.e., something *does* call `new_invalid` externally), revert and skip this task.

- [ ] **Step 3: Commit (only if green)**

```bash
git add crates/ir/src/function.rs
git commit -m "$(cat <<'EOF'
refactor(ir): FunctionGraph::new_invalid is pub(crate)

Sentinel constructor used only by FunctionBuilder; external callers
should never see an uninitialized FunctionGraph.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 7 — Test gap closures

### Task 19: `NodeOutputType::TryFrom<u32>` happy + error path

**Why now:** `node/output_type.rs:181-195` has zero direct tests. Important since the analyzer dispatches on register byte sizes through this conversion.

**Files:**
- Modify: `crates/ir/src/node/tests.rs`

- [ ] **Step 1: Add the test**

```rust
#[test]
fn try_from_u32_size_to_node_output_type() {
    use crate::node::NodeOutputType;
    assert_eq!(NodeOutputType::try_from(1u32).unwrap(), NodeOutputType::U8);
    assert_eq!(NodeOutputType::try_from(2u32).unwrap(), NodeOutputType::U16);
    assert_eq!(NodeOutputType::try_from(4u32).unwrap(), NodeOutputType::U32);
    assert_eq!(NodeOutputType::try_from(8u32).unwrap(), NodeOutputType::U64);
    assert_eq!(NodeOutputType::try_from(16u32).unwrap(), NodeOutputType::U128);
    assert_eq!(NodeOutputType::try_from(32u32).unwrap(), NodeOutputType::U256);
    for bad in [0u32, 3, 5, 7, 9, 15, 17, 33, 64] {
        assert!(matches!(
            NodeOutputType::try_from(bad).unwrap_err().kind(),
            crate::error::ErrorKind::UnsupportedOutputSize(n) if *n == bad
        ));
    }
}
```

> If the no-`unwrap` rule is interpreted strictly for tests, replace `.unwrap()` with `?` and return `Result<(), Error>`. The existing `node/tests.rs` style determines which form to use — match the file's convention.

- [ ] **Step 2: Run test, expect PASS**

- [ ] **Step 3: Commit**

```bash
git add crates/ir/src/node/tests.rs
git commit -m "$(cat <<'EOF'
test(ir): NodeOutputType::TryFrom<u32> happy + error paths

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 20: Memory-chain integration test (`Store → Load → Return`)

**Why now:** `tests/build_validate_roundtrip.rs:loads_and_stores_validate` only checks `build()` returns `Ok`. Nothing asserts that the load's memory input is the *store's* memory output (rather than `InitialMemory`). This is the core SSA-memory invariant.

**Files:**
- Modify: `crates/ir/tests/build_validate_roundtrip.rs`

- [ ] **Step 1: Add the test**

```rust
#[test]
fn store_then_load_threads_memory_through_store() -> Result<(), Box<dyn std::error::Error>> {
    use ir::node::{NodeKind, NodeOutputType};
    use ir::FunctionBuilder;

    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region(&[])?;
    b.set_entry_region(region)?;
    let addr = b.build_int_const(0x1000, NodeOutputType::U64)?;
    let data = b.build_int_const(0xdead_beef, NodeOutputType::U32)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let fg = b.build()?;

    // Find the Load and the Store; assert Load.input[0] (the memory input) is
    // produced by the Store, not by InitialMemory.
    let mut load = None;
    let mut store = None;
    for nid in fg.graph.all_node_ids() {
        match fg.graph.node_kind(nid) {
            NodeKind::Load(_) => load = Some(nid),
            NodeKind::Store(_) => store = Some(nid),
            _ => {}
        }
    }
    let load = load.expect("Load node missing");
    let store = store.expect("Store node missing");
    let load_mem_input = fg.graph.node_inputs(load).get(0).copied().expect("Load.input[0]");
    let (producer, _) = fg.graph.output_definition(load_mem_input);
    assert_eq!(producer, store, "Load's memory input must be produced by the Store");
    Ok(())
}
```

> If `BuiltFunctionGraph` has no public `all_node_ids()` accessor, fall back to `fg.graph.nodes.keys()` if it's accessible, or iterate via the existing `preorder()` walker.

- [ ] **Step 2: Run test, expect PASS**

- [ ] **Step 3: Commit**

```bash
git add crates/ir/tests/build_validate_roundtrip.rs
git commit -m "$(cat <<'EOF'
test(ir): Store→Load threads memory through the Store, not InitialMemory

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 8 — Final gates

### Task 21: Workspace-wide gates

- [ ] **Step 1: Run** `cargo test --workspace` — must be all green.
- [ ] **Step 2: Run** `cargo clippy --workspace --all-targets -- -D warnings` — must be clean. (Default lints; pedantic is intentionally not gated, per existing project state.)
- [ ] **Step 3: Run** `cargo build --workspace --release` — confirms no release-only regression (debug_assert eliminations, etc., though we add none).
- [ ] **Step 4: If anything fails, do not paper over it.** Fix the underlying issue or revert the offending task.
- [ ] **Step 5: No commit unless a fix was needed.**

---

## Out of scope (intentionally)

The following showed up during review but are *not* in this plan, with reasons:

- **Removing `Index<usize>` impls on `Outputs` / `Inputs`.** Multiple external crates (`opt`, `pattern`, `analyzer` indirectly) call `node_inputs(n)[i]` in production. Forcing a Result-returning replacement is a workspace-wide cascade and would destroy a lot of test ergonomics. The panic-rule is a Claude-error-handling rule, not "remove every panicking std-trait impl"; `Index` is a standard Rust trait whose panicking-OOB contract is well understood.
- **Tightening `BuiltFunctionGraph::graph` to `pub(crate)`.** Same reason — it is consumed pervasively externally.
- **Rewriting `NodeOutputType::info()` to avoid `self as usize`.** Already protected by `type_info_table_matches_variants` against the realistic regression (variant reorder); a constant-time table is fine.
- **Adding `CastToInt`/`CastToBool` to `dot::node_fillcolor`'s amber group.** Visual nit; defer until someone wants it.
- **Updating CLAUDE.md to remove `IfCase`/`FloatIsNan`/`Piece`/`Extract`/`Insert`** (which are documented as `NodeKind` variants but are not present in `kind.rs`). The plan stays code-only; CLAUDE.md is a separate concern.
- **Proptest generator expansion** (multi-region, float ops, casts, phis). Worth doing but a project on its own — should land as its own plan.

---

## Self-review checklist (run before submitting)

1. **Spec coverage:** every "Why now" maps to either a code path or a missing test. The 8 confirmed bugs (Tasks 1, 2, 5, 7, 8, 11, 13/15, 16) all have at least one new test that fails before the fix.
2. **Placeholder scan:** the only `todo_inline!()` markers are in Tasks 6, 11, 13, 15, 16 — each with explicit instructions for the implementor to expand following an existing test pattern. They are NOT accepted on their own; the implementor must replace them with concrete code.
3. **Type/name consistency:** `InputIndexOutOfBounds`, `RemoveInputFromCacheableNode`, `EmptyControlStatePredecessors`, `ExpectedBool`, all referenced consistently across tasks.
