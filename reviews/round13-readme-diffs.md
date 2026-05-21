# Round 13 — Per-crate README diffs

## R13-RM-1 — Root `README.md:221`: `IndirectBranchResolve` struct claim is stale

- **Severity:** MED (actively misleading)
- **Current text:** "`IndirectBranchResolve` is a producer-shape classifier for `BranchIndirect` placeholders — it implements the `Optimizer` trait but is instantiated *directly* by the strider orchestrator, not registered in any of the three named pipelines above."
- **Why stale:** The struct was deleted in Round 11 W9 S1.1.  `grep -rn "struct IndirectBranchResolve" crates/opt/src/` returns 0 hits.  CLAUDE.md was fixed in Round 12 W2 E4; root README was missed.
- **Proposed:** "`opt::indirect_branch_resolve` is a module of free-function classifiers (link-register-return, jump-table, stack-array dispatch, tail-call, the `Truncate(IntConst)` / `Extend(IntConst)` arms) and in-place IR editors (`apply_link_register`, `apply_tail_call`).  There is no `Optimizer`-implementing struct — the strider orchestrator calls them directly, outside any pipeline."

## R13-RM-2 — `crates/strider/README.md:14`: `start_addr: u64` is stale

- **Severity:** LOW
- **Current text:** "`RunConfig<'a, R>` — input bundle: `strider: &Strider`, `start_addr: u64`, …"
- **Why stale:** Round-12 R12-T-H changed the field type to `cfg::MachineInsnAddr`.  `From<u64>` exists for ergonomic construction.
- **Proposed:** "`RunConfig<'a, R>` — input bundle: `strider: &Strider`, `start_addr: cfg::MachineInsnAddr` (construct via `cfg::MachineInsnAddr::new(addr)` or `addr.into()`), …"

## Other crate READMEs

| README | Sampled | Drift found |
|---|---|---|
| `crates/ir/README.md` | yes | clean |
| `crates/opt/README.md` | yes | clean (R12 W2 R-2/R-3 fixes hold) |
| `crates/cfg/README.md` | yes | clean (R12 W2 R-4/R-5 tier-2 fixes hold) |
| `crates/strider-py/README.md` | yes | clean (R12 W2 R-6/R-7 float_is_nan fixes hold) |
| `crates/pcode-lift/README.md` | yes | clean (R12 W2 R-8 partial-write fix holds) |
| `crates/target/README.md` | yes | clean |
| `crates/reader/README.md` | yes | clean |
| `crates/pattern/README.md` | yes | clean |
| `crates/graphwalk/README.md`, `entity-utils/README.md`, `dot/README.md` | yes | clean (small crates, doc-stable) |

## Summary

| Edit | File | Severity |
|------|------|----------|
| R13-RM-1 | `README.md:221` | MED |
| R13-RM-2 | `crates/strider/README.md:14` | LOW |

Two edits total.  All R12 W2 README fixes are confirmed still holding.
