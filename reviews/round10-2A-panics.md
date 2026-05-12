# Round 10 — Production-Panic Audit

**Scope:** All `crates/*/src/**/*.rs`. Excluded: `crates/*/tests/`, `crates/*/examples/`, `crates/*/benches/`, inline `#[cfg(test)]` blocks, `test_utils.rs`, `rewrite_tests.rs`, files whose entire content is a test module.

**Method:** Grepped `.unwrap()`, `.expect(`, `panic!(`, `unreachable!(`, `assert!(`, `debug_assert!(` per file. Confirmed scope of every `#[cfg(test)]` boundary.

---

## SITE 1 — `compact.rs`: remap node lookup after first pass

- **Severity:** LOW
- **Where:** `crates/ir/src/graph/compact.rs:117-118`
- **Verdict:** justified
- **Reasoning:** Pass 1 unconditionally inserts every element of `reachable` into `remap.nodes`; Pass 2 iterates the identical Vec. Internal-invariant assertion only.

## SITE 2 — `compact.rs`: live node's input points to zombie output

- **Severity:** LOW
- **Where:** `crates/ir/src/graph/compact.rs:126-129`
- **Verdict:** justified
- **Reasoning:** `walk_graph` follows backward-data edges, so any node whose output feeds a reachable node is itself reachable. `detach_unreachable_nodes` severs inputs FROM zombies, never FROM live nodes. Layer-B use-list consistency check would have caught it earlier. Internal-invariant assertion.

## SITE 3 — `function.rs`: entry survives its own compaction

- **Severity:** LOW
- **Where:** `crates/ir/src/function.rs:237-239`
- **Verdict:** justified
- **Reasoning:** `retain_reachable` calls `walk_graph(self, entry)`; entry is always the first node in the reachable set. Internal-invariant assertion.

## SITE 4 — `flag_cmp_canonicalize`: capture bindings after successful pattern match

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:135-137` and `:140-143`
- **Verdict:** justified
- **Reasoning:** `match_at` already succeeded. Every `Rule`'s `lhs` places `lhs_capture` at a value-producing position. `Match::output(c)` returns `None` only for control-flow captures; rules are statically value-producing. Contract enforced by `Rule` construction.

## SITE 5 — `flag_cmp_canonicalize`: single output from freshly-created node

- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:181-182` and `:195-196`
- **Verdict:** justified
- **Reasoning:** Both helpers call `graph.create_node(...)` with a literal one-element output-kinds slice. `node_outputs_exact::<1>` failure requires an internal `Graph` invariant violation. Construction-time postcondition assertion.

---

## Counts

| Site | File | Line | Kind | Verdict |
|------|------|------|------|---------|
| 1 | `ir/src/graph/compact.rs` | 118 | `.expect` | justified |
| 2 | `ir/src/graph/compact.rs` | 127 | `.expect` | justified |
| 3 | `ir/src/function.rs` | 239 | `.expect` | justified |
| 4a | `opt/src/flag_cmp_canonicalize/mod.rs` | 137 | `.expect` | justified |
| 4b | `opt/src/flag_cmp_canonicalize/mod.rs` | 142 | `.expect` | justified |
| 5a | `opt/src/flag_cmp_canonicalize/mod.rs` | 182 | `.expect` | justified |
| 5b | `opt/src/flag_cmp_canonicalize/mod.rs` | 196 | `.expect` | justified |

**Totals:** 7 production panics; 7 justified; **0 unjustified**.

**By crate:**

| Crate | Total | Justified | Unjustified |
|-------|-------|-----------|-------------|
| `ir` | 3 | 3 | 0 |
| `opt` | 4 | 4 | 0 |
| All others | 0 | — | — |

**Result:** No unjustified panics in any production code path. The codebase propagates errors via `anyhow::Result` throughout and uses `#[allow(clippy::expect_used)]` with inline justification comments at every intentional panic site.
