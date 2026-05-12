# Round 11 — 2A: production-panic audit

## Summary

| Crate         | unwrap | expect | panic! | unreachable! | assert! | debug_assert! |
|---------------|--------|--------|--------|--------------|---------|---------------|
| cfg           | 0      | 0      | 0      | 0            | 0       | 0             |
| dot           | 0      | 0      | 0      | 0            | 0       | 0             |
| entity-utils  | 0      | 0      | 0      | 0            | 0       | 0             |
| graphwalk     | 0      | 0      | 0      | 0            | 0       | 0             |
| ir            | 0      | 3      | 0      | 0            | 0       | 0             |
| opt           | 0      | 4      | 0      | 0            | 0       | 1             |
| pattern       | 0      | 0      | 0      | 0            | 0       | 0             |
| pcode-lift    | 0      | 0      | 0      | 0            | 0       | 2             |
| reader        | 0      | 0      | 0      | 0            | 0       | 0             |
| strider       | 0      | 2      | 0      | 0            | 0       | 0             |
| strider-py    | 0      | 0      | 0      | 0            | 0       | 0             |
| target        | 0      | 0      | 0      | 0            | 0       | 0             |
| **TOTAL**     | **0**  | **9**  | **0**  | **0**        | **0**   | **3**         |

**Top-line result:** the workspace is in excellent shape — zero raw `unwrap()`, zero `panic!()`/`unreachable!()`, zero free-standing `assert!()` outside test modules. All 12 production panic-emitting calls are **already annotated** with `#[allow(clippy::expect_used)]` (where required) or are `debug_assert!` calls with detailed comments naming the exact by-construction invariant the call relies on.

## Findings

Grouping by site. Every site below is **justified** (LOW severity); none should propagate as `Result`.

### ir crate

#### IR.1 — `BuiltFunctionGraph::compact` post-remap entry lookup

- **Severity:** LOW
- **Where:** `crates/ir/src/function.rs:261`
- **Code:** `.expect("entry must survive its own compaction")` (annotated at line 258 with `#[allow(clippy::expect_used)]` and a 4-line comment naming the invariant).
- **Invariant relied on:** `Graph::retain_reachable` walks forward from `entry`; the entry node is reachable from itself by definition, so the remap always contains it.
- **Justified?** YES.  Already correctly annotated.

#### IR.2 — `Graph::retain_reachable` second-pass node remap

- **Severity:** LOW
- **Where:** `crates/ir/src/graph/compact.rs:118`
- **Code:** `.expect("just installed in pass 1")` (annotated at line 116).
- **Invariant relied on:** Pass 1 (lines 83–106) installs every reachable node into `remap.nodes`; Pass 2 iterates the same `reachable` set, so the lookup cannot return `None`.
- **Justified?** YES.

#### IR.3 — `Graph::retain_reachable` second-pass output remap

- **Severity:** LOW
- **Where:** `crates/ir/src/graph/compact.rs:127`
- **Code:** `.expect("input references an output whose producing node was unreachable")` (annotated at line 126).
- **Invariant relied on:** "every input's output producer is reachable iff the input's owning node is reachable" — a structural property of the IR's bidirectional use-list.
- **Justified?** YES.  A violation here would indicate a corrupted graph.

### opt crate

#### OPT.1 — `flag_cmp_canonicalize::apply_rule` lhs-capture binding extraction

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:137`
- **Code:** `.expect("Capture a must bind to a value output")` (annotated at line 134).
- **Invariant relied on:** "the rule's `lhs` always captures `lhs_capture` at a value-producing position." Each `Rule` is constructed in `build_rules()` with an LHS pattern that places `var(a)` (which is value-producing) at the captured position.  Because `match_at` succeeded above (line 123), the binding contract guarantees `output(_)` returns `Some`.
- **Justified?** YES.

#### OPT.2 — `flag_cmp_canonicalize::apply_rule` rhs-capture binding extraction

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:142`
- **Code:** `.expect("Capture b must bind to a value output")` (annotated at line 140).
- **Invariant relied on:** Same as OPT.1 but for the binary-rule second capture.
- **Justified?** YES.

#### OPT.3 — `flag_cmp_canonicalize::build_int_cmp` outputs-exact

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:185`
- **Code:** `.expect("IntCmpOp produces 1 output")` (annotated at line 184).
- **Invariant relied on:** `IntCmpOp` is constructed three lines above (line 174) with exactly one `NodeOutputKind::OutputType(Bool)`; `node_outputs_exact::<1>` enforces that count.
- **Justified?** YES.

#### OPT.4 — `flag_cmp_canonicalize::build_bool_neg` outputs-exact

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:199`
- **Code:** `.expect("BoolNeg produces 1 output")` (annotated at line 198).
- **Invariant relied on:** Same as OPT.3.
- **Justified?** YES.

#### OPT.5 — `apply_link_register` placeholder shape `debug_assert!`

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/inplace.rs:62`
- **Code:** `debug_assert!(graph.node_inputs(placeholder).len() >= 3, …)`
- **Invariant relied on:** The 3-input placeholder shape (`[control, memory, target_value]`) is pinned by the `matches!(kind, NodeKind::IndirectBranch)` guard at line 50 plus the input-arity invariant of `IndirectBranch` enforced by the validator and lift-time builder.
- **Justified?** YES — defence-in-depth check that fires only in debug builds; the `?` on `remove_node_input` (line 66) provides the runtime safety in release.

### pcode-lift crate

#### PL.1 — `read_reg_vn` shift bound `debug_assert!`

- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/vn_io.rs:247`
- **Code:** `debug_assert!(shift_value < (container_reg.size as u64) * 8, …)`
- **Invariant relied on:** Sleigh's per-architecture `.sla` register layouts cannot legally place a sub-register at a byte offset ≥ container size; `find_largest_fitting_register` enforces containment, so `shift_value = (reg.offset - container.offset) * 8` is always `< container_bits`.  The 6-line comment at lines 241–246 documents this as a "future-proof guard against Sleigh spec changes."
- **Justified?** YES.

#### PL.2 — `write_reg_vn` shift bound `debug_assert!`

- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/vn_io.rs:321`
- **Code:** `debug_assert!(shift_bits < (container_reg.size as u64) * 8, …)`
- **Invariant relied on:** Same as PL.1 (write-path mirror of the read-path check).
- **Justified?** YES.

### strider crate

#### ST.1 — `strider::test_utils::strider_for_arch` Sleigh probe

- **Severity:** LOW
- **Where:** `crates/strider/src/test_utils.rs:36`
- **Code:** `arch.probe_regs().expect("strider test-utils: probe_regs")`
- **Invariant relied on:** This module is documented (lib.rs:42–50, test_utils.rs:1–14) as a test-fixture helper exposed `pub` only because Cargo cannot activate per-crate `feature = "test-utils"` flags from sibling integration tests.  The module-level `#![allow(clippy::expect_used, clippy::panic)]` at line 20 is documented in the comment at lines 16–19.  The `# Panics` doc-section names the failure mode (missing `.sla` file at compile time) as a programming error.
- **Justified?** YES — but this is the only site reachable from external user code (since `pub mod test_utils` is unconditionally exposed).  A user who calls `strider_for_arch(...)` from production code would hit it.  The decision is documented and the failure mode (missing Sleigh spec) is a genuine setup-time error, not a runtime input issue, but a strict reviewer might want this further sandboxed.
- **Possible hardening (optional, NOT a fix request):** Either rename the module to make the test-only nature unmistakable (e.g. `test_fixtures`) or change the public signature to return `Result<Strider>` and let the test-side helpers `.unwrap()` instead.

#### ST.2 — `strider::test_utils::strider_for_arch` Strider construction

- **Severity:** LOW
- **Where:** `crates/strider/src/test_utils.rs:37`
- **Code:** `Strider::new(arch, regs, cc).expect("strider test-utils: Strider::new")`
- **Invariant relied on:** Same module-level rationale as ST.1 — failure here is a CC register-name resolution error, which means the calling-convention preset is statically wrong (a programming error, not a runtime input).
- **Justified?** YES.

## Conclusion

- **HIGH-severity findings:** 0
- **MED-severity findings:** 0
- **LOW-severity findings:** 12 (all already correctly annotated and commented)

No code changes are required.  The codebase has reached the state the audit checklist is hunting for: every single panic-emitting site outside test scaffolding is paired with an `#[allow(clippy::expect_used)]` annotation **and** a comment naming the by-construction invariant.  The orchestrator is fully `anyhow::Result`-propagating and cannot panic on any user-supplied binary, calling convention, or memory-reader callback.

The only site touching the public API surface is `strider::test_utils::strider_for_arch` (ST.1/ST.2) — already documented as test-fixture-only with a module-level allow.  If a future hardening pass wanted zero panic surfaces in `pub` API, the cleanest path is to make `strider_for_arch` return `Result<Strider>` (caller writes `.unwrap()` in the test code instead).  Optional and out of scope here.
