# Python Stable API — Plan 3: Pattern Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the pattern DSL so every node kind is matchable and every operand/output slot is addressable, and add a user-defined join filter constraint.

**Architecture:** Four independent features, grounded by two read-only spikes. The matcher *engine* (`try_match_at`/`match_assignments`/`output_ok`/`derive_root`) already treats Control/Memory/Typed uniformly and is generic over any `PatNode`, so most of this is builder-layer glue (`node_pat.rs` + the family builders) plus their Python mirrors. Two real engine touches: `any_input`'s slot enumeration must become prefix-aware, and `AnchorKind` gains a `Control` variant. `constraints.user` reuses the existing `.when()` thread-local bridge, so `strider-pattern` stays PyO3-free.

**Tech Stack:** Rust `strider-pattern` + `strider-py` (PyO3 0.22), maturin, pytest.

## Global Constraints

- **`strider-pattern` stays PyO3-free.** `constraints.user` is a `trait UserConstraint: Debug` object (`Rc<dyn UserConstraint>`); the Python-predicate impl lives in `strider-py` and reaches the query's `Py<PyFunction>` via the existing `CURRENT_QUERY_FUNCTION` thread-local (the same bridge `.when()` uses), NOT via a new parameter through the matcher.
- **No O() regression.** `Candidates::Only` stays a direct index (no search). The commutative/existential enumeration cost is unchanged (bounded by pattern size, not graph size).
- **Behavior-preserving for existing patterns.** All current pattern tests (Rust + pytest) stay green. The `any_input` prefix-awareness change must not alter Phi's behavior (Phi's fixed prefix is exactly `[PhiToken]`, so the generalized rule reduces to today's for Phi).
- **Output slots are leaf value-binding** — capture/kind-constrain a *sibling output* of the matched node (generalizing `IfPat.capture_true`/`CallPat.res`). NOT consumer-recursion (that already exists as `IfPat.with_true`).
- **Rebuild before pytest:** `uv run maturin develop` from the **workspace root**, verify mtime.
- **Full gate before merge:** `cargo test --workspace`, `clippy --workspace`, `cargo doc -D warnings`, `cargo fmt --check`, `pytest` all green.

---

## File structure

- `crates/strider-pattern/src/matcher/walk.rs` — `ext_slots` prefix-awareness (Phase A engine).
- `crates/strider-pattern/src/node_builders/node_pat.rs` — `AnchorKind::Control`, `with_control_value`, generic `input`/`input_any` already present; `output(j)` vertex API.
- `crates/strider-pattern/src/node_builders/*.rs` + `crates/strider-pattern/src/typed/*.rs` — the six kind-root builders + `input`/`any_input`/`output` forwarding on families.
- `crates/strider-pattern/src/matcher/mod.rs` — `UserConstraint` trait + `JoinConstraint::User` + 2 match arms.
- `crates/strider-py/src/pattern.rs` — Python mirrors: builders for the new roots/slots, `PyUserConstraint` + `constraints.user`.
- `crates/strider-py/src/matcher.rs` — reuse `PyMatch` construction in `PyUserConstraint::eval`.
- `crates/strider-py/strider/pattern/__init__.pyi` + `constraints.pyi` — stubs.
- Tests: `crates/strider-pattern/src/**/tests.rs` (Rust) + `crates/strider-py/tests/python/test_pattern_completion.py` (new).

---

## Phase A — Generic `input(i)` / `any_input`

### Task 1: prefix-aware `ext_slots` for `any_input`

**Files:**
- Modify: `crates/strider-pattern/src/matcher/walk.rs` (`ext_slots` build, ~345-375)
- Test: `crates/strider-pattern/src/matcher/` tests

**Why:** `any_input`'s candidate slots today = "every non-`PhiToken` value input slot" (walk.rs:345-364), correct only because `any_input` is Phi-only (prefix `[PhiToken]`). To expose `any_input` on kinds with fixed prefixes (`Call` = `[ctrl, mem, target, …args]`), the existential must range over the kind's **variadic tail**, not "everything but PhiToken".

**Interfaces:**
- Produces: `ext_slots` = the IR node's variadic-tail value-input slots (the slots past the kind's fixed-arity prefix), excluding non-value kinds. For a kind with no variadic tail, `any_input` matches its non-prefix value inputs.

- [ ] **Step 1: Read the current `ext_slots` construction** (walk.rs ~345-375) and the kind's fixed-arity source. The prefix arity per kind comes from `node_signature::expected_signature` (the fixed input slots before the variadic tail). Confirm how to get "fixed prefix length" for a `NodeKind` — grep `expected_signature` / `fixed`/`variadic` in `crates/strider-ir/src/node_signature.rs`. The rule: `ext_slots` = value-kind input slots with index `>= fixed_prefix_len(kind)`.

- [ ] **Step 2: Write the failing test** — an `any_input` on a `Call` pattern must bind an ARG (variadic tail), never the `ctrl`/`mem`/`target` prefix. Build a Call with a known arg and assert `call(...).any_input(var(c))` binds `c` to the arg, and does NOT bind the target/mem. (Use the existing pattern-test fixtures; grep `crates/strider-pattern/src/node_builders/flow.rs` tests for how Call patterns are tested.) Also add a regression assert that Phi's `any_input` is unchanged (binds a value predecessor, never the PhiToken).

- [ ] **Step 3: Run to verify it fails** — `cargo test -p strider-pattern <name>`. Expected: FAIL (today `any_input` on Call would offer prefix slots too, or the builder doesn't exist yet — if `Call.any_input` isn't a builder yet, this test can't be written on Call; in that case test the engine via a direct `NodePat` with a fixed prefix, OR sequence this task AFTER Task 2 exposes `any_input`. Prefer: test through the lowest-level `NodePat::input_any` on a synthetic multi-prefix node.)

- [ ] **Step 4: Implement** — change `ext_slots` to skip the kind's fixed prefix:

```rust
// walk.rs, where ext_slots is built (~345):
// OLD: every input slot whose value-kind != PhiToken
// NEW: every input slot at index >= fixed_prefix_len(ir_kind) whose value-kind is a real value.
let prefix = crate::matcher::fixed_prefix_len(ir_node_kind); // new helper (see below)
let ext_slots: Vec<usize> = (0..ir_input_count)
    .filter(|&i| i >= prefix)
    .filter(|&i| is_value_input(ir_node, i))   // keep the existing non-value exclusion
    .collect();
```

Add `fixed_prefix_len(kind: &NodeKind) -> usize` (in `matcher/mod.rs` or a `node_signature` helper) returning the count of fixed input slots before the variadic tail per `expected_signature`. For `Phi` the prefix is `[PhiToken]` → 1, so `ext_slots` starts at index 1 — identical to today's "skip PhiToken" for Phi. Verify the Phi regression test still passes.

- [ ] **Step 5: Run to verify it passes**, `cargo clippy -p strider-pattern` clean.

- [ ] **Step 6: Commit** — `feat(pattern): prefix-aware any_input slot enumeration`

### Task 2: expose `input(i)` / `any_input` on all node families (Rust + Python)

**Files:**
- Modify: the family builders in `crates/strider-pattern/src/node_builders/*.rs` (`flow.rs` Call/CallOther/Switch/Ret/IndirectBranch/Unreachable, `memory.rs` Load/Store, `phi.rs` already has them) — forward `.input(i, p)` / `.any_input(p)` to `NodePat::input` / `NodePat::input_any` (which exist, node_pat.rs:125-142). Mirror in `crates/strider-py/src/pattern.rs` (`node_builder!` macro or per-builder `.input`/`.any_input`).
- Test: `crates/strider-py/tests/python/test_pattern_completion.py` (new)

**Interfaces:**
- Consumes: Task 1's prefix-aware `any_input`.
- Produces: `<pat>.input(i, sub)` and `<pat>.any_input(sub)` on every node-family builder, Rust + Python. Slot `i` is the kind's own operand numbering (each builder documents its slot base, as `PhiPat.input` does with its `+1` PhiToken shift).

- [ ] **Step 1: Write the failing pytest** (`test_pattern_completion.py`) — e.g. `call(...).any_input(int_const(c))` finds a call whose some-arg is an int const; `store(...).input(0, var(a))` pins the address operand. Use `built_function` fixtures with known shapes.
- [ ] **Step 2: Rebuild + run to verify it fails** — no `any_input` attribute on `CallPat`.
- [ ] **Step 3: Implement** the forwarding methods on each Rust family + the Python mirror. Each `.input(i, p)` forwards to the builder's `NodePat` with the correct slot base (document the base per family; for kinds with a control/mem prefix, `i` is the operand index the user sees — decide and document whether it's absolute or tail-relative; recommend **tail-relative** for variadic families so `call(...).input(0, …)` means "first arg", consistent with `CallPat.arg`).
- [ ] **Step 4: Rebuild + run to verify it passes** + full pytest suite green + clippy.
- [ ] **Step 5: Commit** — `feat(pattern): generic input(i)/any_input on all node families`

---

## Phase B — Output slots (leaf value-binding)

### Task 3: generic `output(j, sub)` / `any_output(sub)`

**Files:**
- Modify: `crates/strider-pattern/src/node_builders/node_pat.rs` (generalize the `control_output`/secondary-output vertex API used by `IfPat.capture_true`), the family builders, and `crates/strider-py/src/pattern.rs`.
- Test: `test_pattern_completion.py`

**Design (from the spike):** a matched node's sibling outputs are already walked in `try_match_at`'s `finalize` (walk.rs:414-429) — each produced-output vertex with a `.capture` binds that output's `ValueId`. `IfPat.capture_true`/`false` declare these via `b.control_output(node, slot)`. Generalize to `output(j)` (declare an output vertex at slot `j` with an optional capture + kind/width constraint) and `any_output` (declare a vertex that binds/constrains *some* output). Leaf-only: no sub-pattern recursion into the output's consumers (that's `with_true`, out of scope).

**Interfaces:**
- Produces: `<pat>.output(j).capture(c)` / `.output(j, kind/width constraint)` and `<pat>.any_output(...)` on node families, binding/constraining a sibling output. Rust + Python.

- [ ] **Step 1: Study the existing mechanism** — `IfPat.capture_true` (flow.rs:504-523, `b.control_output` at 540-547), the `finalize` secondary-output loop (walk.rs:414-429), `output_ok` (walk.rs:696-713). Determine the minimal generic API on `NodePat`: `output(slot)` → declare a `PatValue` output vertex at `slot` with optional capture + `OutputKindSpec`. `any_output` → a vertex with a sentinel slot the matcher tries against each produced output (mirror `ANY_INPUT_SLOT`, but on the output side — decide the enumeration policy and document it).
- [ ] **Step 2: Write the failing test** — a pattern that captures a sibling output (e.g. a `Call`'s 2nd return value via `output(j).capture(c)`), asserting `c` binds the right value; `any_output` binds some output.
- [ ] **Step 3: Rebuild/run to verify it fails.**
- [ ] **Step 4: Implement** the generic `output(j)`/`any_output` on `NodePat` (declaring output vertices) + family forwarding + Python mirror. Reuse the existing `finalize` loop for `output(j)`; add the any-output enumeration if `any_output` is included (if the any-output policy proves subtle, ship `output(j)` first and defer `any_output` with a `log`/note).
- [ ] **Step 5: Rebuild/run to verify it passes** + full suite + clippy.
- [ ] **Step 6: Commit** — `feat(pattern): output(j)/any_output sibling-output binding`

---

## Phase C — Six kind roots

### Task 4: `AnchorKind::Control` + `region()` / `entry()`

**Files:**
- Modify: `crates/strider-pattern/src/node_builders/node_pat.rs` (`AnchorKind` enum + `lower` + `with_control_value`), a new/existing builder module for `region`/`entry`, `crates/strider-py/src/pattern.rs`.
- Test: `test_pattern_completion.py` + Rust unit.

**Design (from the spike):** the matcher already matches Control-rooted nodes (`IfPat` proves it); the only gap is that `AnchorKind` has `Value`/`Memory`/`None` but no `Control`, so a builder can't *seal* a control-anchored root. Add `AnchorKind::Control(slot)` mirroring `Memory(slot)`.

**Interfaces:**
- Produces: `AnchorKind::Control(usize)` + `NodePat::with_control_value(slot)` + a `lower` arm (`AnchorKind::Control(s) => Some(b.control_output(node, s))`); `region()` and `entry()` pattern builders (Rust + Python). `region()` supports variadic control predecessors via `input`/`any_input` (reusing Phi's tail machinery).

- [ ] **Step 1: Add `AnchorKind::Control`** + `with_control_value` + the `lower` arm (node_pat.rs, mirroring the `Memory` case at ~209-217). Rust unit test: a `NodePat::node(Entry).with_control_value(0)` seals and its `Pattern` roots.
- [ ] **Step 2: Write the failing pytest** — `region()` matches a Region node (e.g. `count_regions` cross-check: `len(fn.find_all(region()))` equals the region count); `entry()` matches exactly the Entry node.
- [ ] **Step 3: Verify it fails.**
- [ ] **Step 4: Implement** `region()` / `entry()` builders (Rust `strider-pattern` + Python). `entry()` = `NodePat::node(Entry).with_control_value(0)`; `region()` = `NodePat::node(Region).with_control_value(0)` + variadic control-predecessor `input`/`any_input`.
- [ ] **Step 5: Verify it passes** + full suite + clippy.
- [ ] **Step 6: Commit** — `feat(pattern): AnchorKind::Control + region()/entry() roots`

### Task 5: `initial_memory()` / `segment_op()` / `cpool_ref()` / `new_()`

**Files:**
- Modify: `crates/strider-pattern/src/node_builders/` (or `typed/` for `segment_op`), `crates/strider-py/src/pattern.rs`.
- Test: `test_pattern_completion.py`

**Design (from the spike):** all four map onto existing anchor kinds — no core change. `initial_memory()` ≈ `mem_phi()` (zero-input memory root). `segment_op()` is a value-producing fixed-arity typed builder (like `int_binary`). `cpool_ref()`/`new_()` are variadic value-rooted builders (like `ret` but `Value`-anchored).

**Interfaces:**
- Produces: `initial_memory()`, `segment_op()` (with `op_id` + 2 operands), `cpool_ref()`, `new_()` builders, Rust + Python.

- [ ] **Step 1: Write the failing pytest** — `initial_memory()` matches the InitialMemory node (cross-check: `mem_walk()[0]` from Plan 2 is InitialMemory); `segment_op`/`cpool_ref`/`new_` match their kinds on a fixture that has them (if no fixture exercises SegmentOp/CPoolRef/New, assert the builders exist + `find_all` returns `[]` cleanly on a fixture lacking them, and unit-test the Rust builder seals).
- [ ] **Step 2: Verify it fails.**
- [ ] **Step 3: Implement** the four builders. `initial_memory` = `NodePat::node(InitialMemory).with_mem_value(0)`. `segment_op(op_id)` = a typed fixed-2-arity value builder carrying the `op_id` predicate. `cpool_ref`/`new_` = `NodePat::value(kind, 0)` + variadic `input`.
- [ ] **Step 4: Verify it passes** + full suite + clippy.
- [ ] **Step 5: Commit** — `feat(pattern): initial_memory/segment_op/cpool_ref/new roots`

---

## Phase D — `constraints.user`

### Task 6: `UserConstraint` trait + `JoinConstraint::User` (strider-pattern)

**Files:**
- Modify: `crates/strider-pattern/src/matcher/mod.rs` (near `JoinConstraint`, ~475; `captures()` ~557; `passes()` ~721)
- Test: `crates/strider-pattern/src/matcher/` tests (a Rust `UserConstraint` impl, no Python needed).

**Interfaces:**
- Produces:
  ```rust
  pub trait UserConstraint: std::fmt::Debug {
      fn captures(&self) -> Vec<Capture>;
      fn eval(&self, tuple: &JoinedMatch, function: &Function) -> Option<bool>;
  }
  // in enum JoinConstraint:
  User(std::rc::Rc<dyn UserConstraint>),
  ```

- [ ] **Step 1: Write the failing test** — a Rust struct impl'ing `UserConstraint` (e.g. "the value bound to `c` is even") used in a `find_joined_constrained` call, asserting it filters the tuples. Also test it composes: `negate(User(...))` and `all_of([Dominates, User])` (via the existing `Not`/`And` variants).
- [ ] **Step 2: Verify it fails** — no `User` variant.
- [ ] **Step 3: Implement:**
  - Add the `UserConstraint` trait (with the `: Debug` supertrait so `#[derive(Debug)]` on the enum still works, and `Rc` keeps `#[derive(Clone)]` working).
  - Add `User(Rc<dyn UserConstraint>)` to `JoinConstraint`.
  - `captures()` arm: `JoinConstraint::User(u) => u.captures()` (feeds the connectivity + range-restriction checks — the closure DECLARES its captures).
  - `passes()` arm: `JoinConstraint::User(u) => u.eval(tuple, self.function)` (`ConstraintEval.function` is the `&Function`).
  - Confirm `#[derive(Debug, Clone)]` on `JoinConstraint` still compiles.
- [ ] **Step 4: Verify it passes** + `cargo clippy -p strider-pattern` clean.
- [ ] **Step 5: Commit** — `feat(pattern): UserConstraint trait + JoinConstraint::User filter`

### Task 7: `constraints.user` Python binding

**Files:**
- Modify: `crates/strider-py/src/pattern.rs` (`PyUserConstraint` impl + `user` `#[pyfunction]` + `register_constraints`), reuse `crates/strider-py/src/matcher.rs` `PyMatch` + `crates/strider-py/src/pattern.rs` `current_query_function` (~1211) thread-local.
- Test: `test_pattern_completion.py`

**Design (from the spike):** `PyUserConstraint { predicate: PyObject, captures: Vec<Capture> }` impls `UserConstraint`. `eval` reuses the `.when()` bridge: `Python::with_gil`, get `(function, generation)` from `current_query_function`, build `PyMatch { inner: tuple.clone(), function, generation }`, call the predicate, extract `Option<bool>` — mirroring `run_when_predicate` (pattern.rs:1219-1294) including its control-flow-exception stash (`PENDING_CONTROL_FLOW`) so a raised predicate propagates as `StriderError` after the query (drained by `run_query`, function.rs:567-569).

**Interfaces:**
- Produces: `strider.pattern.constraints.user(predicate, *captures) -> JoinConstraint`, where `predicate` is `Callable[[Match], bool | None]` and `captures` are the `Capture`s it reads.

- [ ] **Step 1: Write the failing pytest** — `find_all([pat], constraints=[user(lambda m: m.const_uint(c) > K, c)])` filters matches by the Python predicate; assert it composes (`negate(user(...))`); assert a raised predicate surfaces as `StriderError`.
- [ ] **Step 2: Rebuild + verify it fails** — no `constraints.user`.
- [ ] **Step 3: Implement** `PyUserConstraint` (impl `UserConstraint`, `Debug` prints `"User(<predicate>)"`), the `user(predicate, captures)` `#[pyfunction]` wrapping `JoinConstraint::User(Rc::new(PyUserConstraint{...}))`, and register it in `register_constraints` (pattern.rs:3581) via `add_fn!(user)`. `eval` mirrors `run_when_predicate` verbatim (GIL, thread-local function, PyMatch, extract, exception stash).
- [ ] **Step 4: Rebuild + verify it passes** + full pytest suite + clippy.
- [ ] **Step 5: Commit** — `feat(py): constraints.user filter constraint`

---

## Phase E — Stubs + full gate

### Task 8: stubs + gate

**Files:**
- Modify: `crates/strider-py/strider/pattern/__init__.pyi` (new builders + `input`/`any_input`/`output`/`any_output` on pat classes), `crates/strider-py/strider/pattern/constraints.pyi` (`user`).

- [ ] **Step 1: Add stubs** for every new Python surface: the six root builders (`region`/`entry`/`initial_memory`/`segment_op`/`cpool_ref`/`new_`), the `.input`/`.any_input`/`.output`/`.any_output` methods on the pattern classes, and `constraints.user`. Hand-edit (stub_gen is broken in-env). Match existing `.pyi` style.
- [ ] **Step 2: Verify stubs** — `python -c "import strider; import strider.pattern; import strider.pattern.constraints"` clean; if pyright available, no new errors in the pattern stubs.
- [ ] **Step 3: Full gate**

```bash
cargo test --workspace 2>&1 | tail -5    # gate on real exit code
cargo clippy --workspace 2>&1 | tail -3
cargo fmt --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps 2>&1 | tail -3
uv run maturin develop
uv run pytest crates/strider-py/tests/python -q
```
Expected: all green, 0 skips.

- [ ] **Step 4: Commit** — `docs(py): stub pattern-completion additions`

---

## Self-review notes

- **Spec coverage:** §G generic slots (Tasks 1-3), §G kind roots (Tasks 4-5), §H `constraints.user` (Tasks 6-7). Commutative matching untouched (`any_input` is a complement, per the spec). Named slot accessors (`store.addr`, `call.arg`) remain as sugar — not removed.
- **Confirm-before-coding:** `fixed_prefix_len` source (Task 1 — the variadic-tail boundary per `expected_signature`); the `any_output` enumeration policy (Task 3 — ship `output(j)` first if `any_output` proves subtle); whether `segment_op` fits the `typed/` fixed-arity form (Task 5).
- **Engine-touch discipline:** only two real matcher-engine changes (Task 1 `ext_slots`, Task 4 `AnchorKind::Control`); everything else is builder-layer + Python. The `constraints.user` trait keeps `strider-pattern` PyO3-free.
- **Type consistency:** `UserConstraint::eval(tuple, function)` matches `passes()`'s available `&Function`; `captures()` returns `Vec<Capture>` feeding the existing connectivity check.
