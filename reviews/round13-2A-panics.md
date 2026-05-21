# Round 13 — 2A: production-panic audit

Branch: `review/ai7`.  Scanned every `.rs` file under `crates/*/src/` excluding `tests/`, `examples/`, `benches/` paths, files named `tests.rs` / `*_tests.rs` / `test_support.rs`, and in-file `#[cfg(test)] mod` blocks.

## Verdict

**0 HIGH / 0 MED / 12 LOW.  All sites correctly annotated and justified.  Delta from R12: −2 sites (the EC-3 `debug_assert!` in `vn_io.rs` were converted to runtime `Err` in W1).**

## Summary table

| Crate | unwrap | expect | panic! | unreachable! | assert! | debug_assert! |
|---|---|---|---|---|---|---|
| cfg | 0 | 0 | 0 | 0 | 0 | 2 |
| dot | 0 | 0 | 0 | 0 | 0 | 0 |
| entity-utils | 0 | 0 | 0 | 0 | 0 | 0 |
| graphwalk | 0 | 0 | 0 | 0 | 0 | 0 |
| ir | 0 | 3 | 0 | 0 | 0 | 1 |
| opt | 0 | 5 | 0 | 0 | 0 | 1 |
| pattern | 0 | 0 | 0 | 0 | 0 | 0 |
| pcode-lift | 0 | 0 | 0 | 0 | 0 | 0 |
| reader | 0 | 0 | 0 | 0 | 0 | 0 |
| strider | 0 | 2* | 0 | 0 | 0 | 0 |
| strider-py | 0 | 0 | 0 | 0 | 0 | 0 |
| target | 0 | 0 | 0 | 0 | 0 | 0 |
| **TOTAL** | **0** | **10** | **0** | **0** | **0** | **4** |

\* In `src/test_utils.rs` (test-fixture module with module-level `#![allow(clippy::expect_used, clippy::panic)]`).

Also scanned `assert_eq!` / `assert_ne!` / `todo!` / `unimplemented!` — zero hits in production scope.

## Per-site listing (all justified)

| ID | File:line | Site | Justification |
|---|---|---|---|
| IR.1 | `crates/ir/src/function.rs:275` | `.expect("entry must survive its own compaction")` | walk_graph reaches `entry` from itself by definition |
| IR.2 | `crates/ir/src/graph/compact.rs:118` | `.expect("just installed in pass 1")` | two-pass structure iterates same `reachable` set |
| IR.3 | `crates/ir/src/graph/compact.rs:127` | `.expect("input references an output whose producing node was unreachable")` | bidirectional use-list invariant |
| IR.4 | `crates/ir/src/graph/store.rs:88` | `debug_assert!(slot_counts_match_kind(...))` | caller-contract early-detection guard; validator catches in release |
| OPT.1 | `crates/opt/src/pipeline.rs:59` | `.expect("replace_all_uses: cursor invariant upheld by while-guard")` | R12 TY-3 by-construction null-cursor impossible |
| OPT.2 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:137` | `.expect("Capture a must bind to a value output")` | match_at success implies LHS captures bound |
| OPT.3 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:142` | `m.output(c).expect(...)` | same as OPT.2 |
| OPT.4 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:185` | `node_outputs_exact::<1>(n).expect("IntCmpOp produces 1 output")` | create_node provides exactly one Bool output |
| OPT.5 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:199` | `node_outputs_exact::<1>(n).expect("BoolNeg produces 1 output")` | same |
| OPT.6 | `crates/opt/src/indirect_branch_resolve/inplace.rs:62` | `debug_assert!(graph.node_inputs(placeholder).len() >= 3, ...)` | IndirectBranch always 3 inputs by builder; release safety via `?` on remove_node_input |
| CFG.1 | `crates/cfg/src/cfg/options.rs:195` | `debug_assert!(false, "set_function_max_size(0) is meaningless")` | API misuse; silently falls back to unbounded in release |
| CFG.2 | `crates/cfg/src/cfg/options.rs:221` | `debug_assert!(false, "set_function_boundary(Bounded { max_size: 0 }) is meaningless")` | same as CFG.1 for the FunctionBoundary enum overload |
| ST.1/2 | `crates/strider/src/test_utils.rs:35-36` | `arch.probe_regs().expect(...)`, `Strider::new(...).expect(...)` | test fixture module with module-level allow + `# Panics` doc |

## Note

Round 1B + Ask-8 edge-cases both noted that CFG.1 and CFG.2 only fire in debug builds; the corrective assignment (`self.options.fn_max_size = None`) runs in both modes so the **behaviour** is correct.  The residual concern is documentation/coverage: round-13 1B proposes converting these to a real `Result`-returning ctor or hardening to `panic!` so the intent is enforced in release builds.  See `round13-1B-pcode-lift-cfg.md` EC-1-FOLLOWUP and `round13-correctness-edge-cases.md` EC-1.

## Conclusion

Workspace lint config (`Cargo.toml` workspace lints) denies `clippy::unwrap_used`, `expect_used`, `panic`, `unreachable`, `todo` globally.  Every justified site has either `#[allow(clippy::expect_used)]` adjacent + a comment naming the invariant, OR a module-level allow scoped to test-fixture modules.  No code changes required from this audit.
