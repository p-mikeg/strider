# Deep Audit 2026-06-14 — Dimension 6: Dead Code

**Scope:** All 15 crates under `crates/` (incl. `strider-py` PyO3 bindings,
`strider-orchestrator/examples/`, benches, and every `tests/` dir).
**Mode:** READ-ONLY. Only this report file was written.

## Method

1. `cargo clean && cargo build --workspace` → **zero** `dead_code` / `never used`
   / `never read` / `never constructed` warnings. `cargo build --workspace
   --all-targets --exclude strider-py` (strider-py's lib-test target fails to
   *link* only because there is no libpython in this env — unrelated to dead
   code) → **zero** warnings. Conclusion: the compiler finds **no private dead
   code**; every candidate must be a `pub`/`pub(crate)` item the compiler can't
   flag.
2. Inventoried `pub fn` / `pub(crate) fn` / pub types / enum variants / struct
   fields / exported macros per crate, then grepped each identifier across the
   whole workspace, classifying callers as: definition-only, internal-non-test,
   test-only, external-crate, or strider-py(PyO3).
3. Verified every claim below with an explicit grep. Items used via a trait,
   re-export, PyO3 registration, a `#[cfg(feature = "test-injectors")]` gate,
   or the single-SSoT-constructor pattern were **excluded** as not-dead.

## Headline

The workspace is exceptionally clean. **No truly-dead production code (category
a) exists outside the `strider-ir-test-utils` dev-dep.** All findings are either
unused test-builder methods or pub surface that only tests reach.

---

## Findings

### D6-01 — `strider-ir-test-utils::Tb` has 6 never-called builder methods
- **Category:** (a) TRULY DEAD (within a test-support crate)
- **Severity:** LOW · **Confidence:** HIGH
- **Location:** `crates/strider-ir-test-utils/src/tb.rs`
  - `Tb::band` (165), `Tb::int_un_at` (210), `Tb::int_cmp_at` (220),
    `Tb::store_in` (329), `Tb::load_in` (337), `Tb::call_addr` (347)
- **Evidence:** Each grepped across all of `crates/`. The only hit for each is
  its own definition line; every other apparent hit is an unrelated substring
  (`store_inner` / `store_inputs` / `load_inputs` / `call_address` /
  `load_input_slots…`). e.g. `grep -rn '\.band\b\|fn band'` → one line (the
  def); `int_un_at` / `int_cmp_at` → one line each.
- **Action:** Delete all six. `band` duplicates `int_bin(..And..)`; the `_at`
  / `_in` variants duplicate the type-defaulted builders that callers actually
  use. (test-utils is a dev-dep, so no downstream-stability concern.)

### D6-02 — `Interval::upper_exclusive` reachable only from tests
- **Category:** (b) TEST-ONLY-IN-PRODUCTION
- **Severity:** LOW · **Confidence:** HIGH
- **Location:** `crates/strider-opt/src/value_range/mod.rs:60`
- **Evidence:** `grep -rn upper_exclusive crates` → definition + **9** call
  sites, all in `crates/strider-opt/src/value_range/tests.rs`. Zero production
  or cross-crate callers; not used by any other `value_range` code.
- **Action:** Either `#[cfg(test)]`-gate it (it pins a documented `Interval`
  property the tests assert) or fold the computation inline into the tests and
  delete the method.

### D6-03 — `Graph::value_has_one_use` reachable only from tests
- **Category:** (b) TEST-ONLY-IN-PRODUCTION / (c) UNUSED PUB SURFACE
- **Severity:** LOW · **Confidence:** HIGH
- **Location:** `crates/strider-graph/src/graph.rs:358`
- **Evidence:** `grep -rn value_has_one_use crates` → definition + callers only
  in `strider-graph/tests/proptest_invariants.rs` and
  `strider-ir/src/graph/tests.rs`. No internal-graph caller, no production
  consumer in strider-ir/opt/pattern, no PyO3 use. (Sibling not-a-finding:
  `value_uses` / single-use checks done elsewhere go through the use-list
  cursor directly, not this convenience predicate.)
- **Action:** Keep as a deliberate query if it's meant to be library surface,
  but it is currently exercised only by invariant tests — candidate to move the
  helper next to the tests, or document it as intended generic-lib API. As
  pub surface on a generic crate it is defensible; flagged for visibility.

### D6-04 — `PositionalArgLayout::stack_offset_of` reachable only from tests
- **Category:** (b) TEST-ONLY-IN-PRODUCTION
- **Severity:** LOW · **Confidence:** HIGH
- **Location:** `crates/strider-target/src/calling_convention/mod.rs:398`
- **Evidence:** `grep -rn stack_offset_of crates` → definition + ~10 assertions
  in `strider-target/src/calling_convention/tests.rs` only. Production stack-arg
  paths (`FunctionArgDetect` / `CallStackArgCollect`) use `first_stack_index`
  and the `StackArgs::offset_of` / `slot_of` family, not this layout-level
  wrapper. No cross-crate or PyO3 caller.
- **Action:** It is reasonable public ABI-layout surface, but presently only
  tests call it. Either keep as documented API or `#[cfg(test)]`/inline it.

---

## Notable NON-findings (verified used; do not re-flag)

These were investigated as plausible dead code and proven live:

- **`#[cfg(feature = "test-injectors")]` corruptors** —
  `Graph::corrupt_clear_first_use` / `corrupt_retarget_input`
  (`strider-graph/src/graph.rs:487,495`) are feature-gated test fault-injectors
  used by `strider-ir/src/validate/tests.rs`. Correctly isolated, **not dead**.
- **IRViewer / IRWalker default methods** — `count_kind`, `has_kind`,
  `walk_kind`, `reachable_kind_iter`, `reverse_postorder_filter`,
  `postorder_filter`, `infer_float_type`, `const_value`, `get_as_unsigned_int`
  / `get_as_signed_int` / `get_as_int`, every `require_*` (incl. the
  single-caller `require_bool_value` @ `builder/nodes.rs:173`,
  `require_phi_token_kind` @ `builder/nodes.rs:249`), `int_const_i128`
  (strider-pattern `bindings.rs:216`), `int_const_wide_le_bytes`,
  `bool_const_val`, `memory_output_of` — **all have ≥1 production caller.**
  (The earlier "get_as_float / get_as_bool / require_typed are dead" guess was
  spurious: those identifiers do not exist in the codebase.)
- **`raw_dot` / `raw_html`** — `raw_html` reaches `strider-py/src/function.rs`
  (`raw_html_str`/`to_raw_html`); `raw_dot` has 3 callers. Live debug surface.
- **`node_id_from_u32`** (`strider-graph`) — used in production
  `strider-py/src/function.rs` + `node.rs`. Live.
- **strider-pattern fluent builders** (`int_ne`, `unary`, `binary`, `wildcard`,
  `preceded_by`, `from_kind`, `missing_binding`, `first_value_input_type`,
  `discriminant`, the `set_*` private-ish setters, …) — every one is consumed
  either inside `strider-pattern/src` by builder impls/macros, by the
  `strider-py` mirror, or by pattern tests. None dead.
- **Rare IR enum variants** — `ValueType::{I80,I256,I512,F80}` and
  `NodeKind::{New,CPoolRef,SegmentOp}` are all constructed and/or matched
  (60–135 hits each). Live.
- **Generic-crate surfaces** (`dot`, `entity-utils`, `graphwalk`,
  `read-only-memory`, `strider-graph`) — every pub item resolved to a consumer
  in strider-ir / strider-pattern / opt / strider-py / examples.
- **PyO3 surface in strider-py** — `#[pymethods]`/`#[pyfunction]`/`#[pyclass]`
  items are live Python API by registration even with zero Rust callers; not
  dead.
- **Optimizer passes** — every pass type is registered in `default_pipeline()`
  or constructed in tests/py; none orphaned.

## Counts

| Category | HIGH | MED | LOW | total |
|---|---|---|---|---|
| (a) Truly dead | 0 | 0 | 6 (D6-01, the 6 Tb methods) | 6 items / 1 finding |
| (b) Test-only-in-production | 0 | 0 | 3 (D6-02, D6-03, D6-04) | 3 |
| (c) Unused pub surface | 0 | 0 | (D6-03 also fits c) | 0 net-new |

**Net: 4 findings (6 dead items + 3 test-only pub methods). No HIGH/MED dead
subsystems. No truly-dead production (non-test-utils) code.**
