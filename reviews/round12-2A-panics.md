# Round 12 — 2A: production-panic audit

Branch: `review/ai6`
Methodology: scanned every `.rs` file under `crates/**` excluding `tests/`,
`examples/`, `benches/` paths plus files named `tests.rs`, `*_tests.rs`, and
`test_support.rs` (all unconditionally `#[cfg(test)]`-gated by convention in
this workspace).  In-file `#[cfg(test)] mod foo { … }` blocks are excluded by
tracking brace depth past a `cfg(test)` attribute.  Verified the file-name
exclusion list against actual gating: every `_tests.rs` and `test_support.rs`
include site in the workspace is `#[cfg(test)]`-gated (checked
`grep -rn '_tests\|test_support' crates/ --include='*.rs'`).

## Summary

| Crate         | unwrap | expect | panic! | unreachable! | assert! | debug_assert! |
|---------------|--------|--------|--------|--------------|---------|---------------|
| cfg           | 0      | 0      | 0      | 0            | 0       | 0             |
| dot           | 0      | 0      | 0      | 0            | 0       | 0             |
| entity-utils  | 0      | 0      | 0      | 0            | 0       | 0             |
| graphwalk     | 0      | 0      | 0      | 0            | 0       | 0             |
| ir            | 0      | 3      | 0      | 0            | 0       | 1             |
| opt           | 0      | 4      | 0      | 0            | 0       | 1             |
| pattern       | 0      | 0      | 0      | 0            | 0       | 0             |
| pcode-lift    | 0      | 0      | 0      | 0            | 0       | 2             |
| reader        | 0      | 0      | 0      | 0            | 0       | 0             |
| strider       | 0      | 2      | 0      | 0            | 0       | 0             |
| strider-py    | 0      | 0      | 0      | 0            | 0       | 0             |
| target        | 0      | 0      | 0      | 0            | 0       | 0             |
| **TOTAL**     | **0**  | **9**  | **0**  | **0**        | **0**   | **4**         |

Additional scan for `assert_eq!` / `assert_ne!` / `debug_assert_eq!` /
`debug_assert_ne!` / `todo!` / `unimplemented!` in the same scope: **zero
hits** workspace-wide.

**Top-line result:** the workspace remains in excellent shape — zero raw
`unwrap()`, zero `panic!()`/`unreachable!()`, zero free-standing `assert!()`
in production code.  All 13 panic-emitting calls are already annotated with
`#[allow(clippy::expect_used)]` (where required) or are `debug_assert!`
calls with detailed comments naming the by-construction invariant.

**Delta vs Round 11**: +1 net site — a new `debug_assert!` was added in
`crates/ir/src/graph/store.rs:88` as a defence-in-depth check on
`Graph::set_node_kind`'s slot-shape contract.  Already well documented.
All other sites are unchanged from R11.

## Findings

Every site below is **already correctly annotated and justified**.  All
LOW severity; none should propagate as `Result`.

### ir crate

#### IR.1 — `BuiltFunctionGraph::compact` post-remap entry lookup

- **Severity:** LOW
- **Where:** `crates/ir/src/function.rs:282`
- **Code:** `.expect("entry must survive its own compaction")`
- **Annotation:** `#[allow(clippy::expect_used)]` at line 279 with a 4-line
  rationale comment (lines 275–278) naming the invariant.
- **Invariant:** `Graph::retain_reachable` walks forward from `entry`; the
  entry node is reachable from itself by definition, so the remap always
  contains it.
- **Justified?** YES.  Unchanged from R11.

#### IR.2 — `Graph::retain_reachable` pass-2 node remap

- **Severity:** LOW
- **Where:** `crates/ir/src/graph/compact.rs:118`
- **Code:** `.expect("just installed in pass 1")`
- **Annotation:** `#[allow(clippy::expect_used)]` at line 116 with a 6-line
  comment (lines 110–115) describing the two-pass structure.
- **Invariant:** Pass 1 installs every reachable node into `remap.nodes`;
  Pass 2 iterates the same `reachable` set.
- **Justified?** YES.  Unchanged from R11.

#### IR.3 — `Graph::retain_reachable` pass-2 output remap

- **Severity:** LOW
- **Where:** `crates/ir/src/graph/compact.rs:127`
- **Code:** `.expect("input references an output whose producing node was unreachable")`
- **Annotation:** `#[allow(clippy::expect_used)]` at line 126.
- **Invariant:** A bidirectional-use-list property: every input's output
  producer is reachable iff the input's owning node is reachable.
- **Justified?** YES.  Unchanged from R11.

#### IR.4 — `Graph::set_node_kind` slot-shape `debug_assert!` (NEW since R11)

- **Severity:** LOW
- **Where:** `crates/ir/src/graph/store.rs:88`
- **Code:**
  ```rust
  debug_assert!(
      crate::node_signature::slot_counts_match_kind(self, node_id, &kind),
      "set_node_kind: node {node_id:?} (old={old_kind:?}) slot shape does \
       not match new kind {kind:?}'s expected signature — call \
       add_node_input/remove_node_input first to reshape inputs"
  );
  ```
- **Comment context:** lines 82–87 explain that `apply_link_register` and
  similar producers reshape inputs via `add_node_input` / `remove_node_input`
  before calling `set_node_kind`, so a mismatch is a caller-side bug.
- **Doc context:** `# Errors` section (lines 67–74) explicitly documents
  that the debug-assertion is part of the API contract.
- **Invariant:** The caller must reshape inputs to match the new kind's
  expected signature before mutating the kind.
- **Justified?** YES — the runtime safety in release builds comes from
  `expected_signature` checking by the validator on the next `build()` /
  `validate()` call.  The debug-only assertion shortens the feedback loop
  during development.

### opt crate

#### OPT.1 — `flag_cmp_canonicalize::apply_rule` lhs-capture extraction

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:137`
- **Code:** `.expect("Capture a must bind to a value output")`
- **Annotation:** `#[allow(clippy::expect_used)]` at line 134 with a 7-line
  rationale (lines 127–133).
- **Invariant:** The rule's `lhs` always captures `lhs_capture` at a
  value-producing position; `match_at` succeeded above so the binding is
  populated.
- **Justified?** YES.  Unchanged from R11.

#### OPT.2 — `flag_cmp_canonicalize::apply_rule` rhs-capture extraction

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:142`
- **Code:** `m.output(c).expect("Capture b must bind to a value output")`
- **Annotation:** `#[allow(clippy::expect_used)]` at line 140.
- **Invariant:** Same as OPT.1.
- **Justified?** YES.  Unchanged from R11.

#### OPT.3 — `flag_cmp_canonicalize::build_int_cmp` outputs-exact

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:185`
- **Code:** `let [out] = graph.node_outputs_exact::<1>(n).expect("IntCmpOp produces 1 output");`
- **Annotation:** `#[allow(clippy::expect_used)]` at line 184 with a 4-line
  rationale (lines 180–183).
- **Invariant:** `IntCmpOp` is constructed three lines above (line 174) with
  exactly one `NodeOutputKind::OutputType(Bool)`; `node_outputs_exact::<1>`
  enforces and returns that single output.
- **Justified?** YES.  Unchanged from R11.

#### OPT.4 — `flag_cmp_canonicalize::build_bool_neg` outputs-exact

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:199`
- **Code:** `let [out] = graph.node_outputs_exact::<1>(n).expect("BoolNeg produces 1 output");`
- **Annotation:** `#[allow(clippy::expect_used)]` at line 198.
- **Invariant:** Same as OPT.3 — `BoolUnaryOp::Neg` is constructed with
  exactly one Bool output.
- **Justified?** YES.  Unchanged from R11.

#### OPT.5 — `apply_link_register` placeholder shape `debug_assert!`

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/inplace.rs:62`
- **Code:** `debug_assert!(graph.node_inputs(placeholder).len() >= 3, …)`
- **Comment context:** lines 56–61 pin the 3-input placeholder shape
  (`[control, memory, target_value]`) via the `matches!(kind,
  NodeKind::IndirectBranch)` guard at line 50 plus IR-builder invariants.
- **Invariant:** `IndirectBranch` always has exactly 3 inputs.
- **Justified?** YES — defence-in-depth; `?` on
  `remove_node_input` (line 66) provides runtime safety in release builds.
  Unchanged from R11.

### pcode-lift crate

#### PL.1 — `read_reg_vn` shift-bound `debug_assert!`

- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/vn_io.rs:247`
- **Code:** `debug_assert!(shift_value < (container_reg.size as u64) * 8, …)`
- **Comment context:** lines 241–246 document this as a future-proof
  guard against Sleigh spec emission errors.
- **Invariant:** Sleigh's per-architecture `.sla` register layouts cannot
  legally place a sub-register at a byte offset ≥ container size;
  `find_largest_fitting_register` enforces containment.
- **Justified?** YES.  Unchanged from R11.

#### PL.2 — `write_reg_vn` shift-bound `debug_assert!`

- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/vn_io.rs:321`
- **Code:** `debug_assert!(shift_bits < (container_reg.size as u64) * 8, …)`
- **Invariant:** Same as PL.1 (write-path mirror).
- **Justified?** YES.  Unchanged from R11.

### strider crate

#### ST.1 — `strider::test_utils::strider_for_arch` Sleigh probe

- **Severity:** LOW
- **Where:** `crates/strider/src/test_utils.rs:36`
- **Code:** `arch.probe_regs().expect("strider test-utils: probe_regs")`
- **Annotation:** module-level `#![allow(clippy::expect_used, clippy::panic)]`
  at line 20, documented in the comment at lines 16–19, and via the
  `# Panics` doc-section at lines 27–33.
- **Invariant:** The module is a test fixture exposed `pub` only because
  Cargo cannot activate per-crate `feature = "test-utils"` flags from
  sibling integration tests.  Failures denote setup-time programming
  errors (missing `.sla`).
- **Justified?** YES.  Unchanged from R11.
- **Note:** R11 flagged this as the only `pub` API surface that can panic;
  the optional hardening (return `Result<Strider>`) is still open but
  remains out of scope.

#### ST.2 — `strider::test_utils::strider_for_arch` Strider construction

- **Severity:** LOW
- **Where:** `crates/strider/src/test_utils.rs:37`
- **Code:** `Strider::new(arch, regs, cc).expect("strider test-utils: Strider::new")`
- **Annotation:** module-level allow + `# Panics` doc.
- **Invariant:** Same module-level rationale as ST.1.  Failure denotes a
  CC register-name resolution error (statically wrong CC preset).
- **Justified?** YES.  Unchanged from R11.

## Conclusion

- **HIGH-severity findings:** 0
- **MED-severity findings:** 0
- **LOW-severity findings:** 13 (all already correctly annotated)

No code changes are required.  Every panic-emitting site outside test
scaffolding has the `#[allow(clippy::expect_used)]` annotation (or module-
level allow) paired with a comment naming the by-construction invariant.
The orchestrator stays fully `anyhow::Result`-propagating and cannot panic
on any user-supplied binary, calling convention, or memory-reader callback.

The R11 → R12 delta is +1 site: a new `debug_assert!` in
`crates/ir/src/graph/store.rs:88` validating `set_node_kind`'s slot-shape
contract.  Its rationale is documented inline (4-line comment, plus a
matching `# Errors` doc-section on the public method).  No action needed.
