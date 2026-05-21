# Round 12 — Ask 8 pass 2: invariant-violation audit

Branch: `review/ai6` · Trust model: strict (no prior-round reviews; no other round-12 reports read).

## Verdict

**2 findings, both HIGH-impact.** They interact: F-2 only becomes independently observable once F-1 is fixed.

## Findings

### INV-1 — `DeadBranchElimination` corrupts `StackStorePhi` by removing fixed-arity inputs
- **Severity:** HIGH (confidence 88)
- **Invariant:** `StackStorePhi` has fixed arity 3: `[PhiToken, Memory, Data]` (per `crates/ir/src/node_signature.rs:352`).
- **Declared at:** `crates/ir/src/node_signature.rs:352` + CLAUDE.md "Asm-fingerprint side-table" / "Memory" section.
- **Violation path:** `crates/opt/src/dead_branch/mod.rs:145-165`.
- **What's wrong:** The loop at lines 146-150 collects all consumers of the `ControlState`'s `PhiToken` output (`cs_phi_out = cs_outputs[1]`) into `phi_nodes`. `StackStorePhi.inputs[0]` is exactly that same `PhiToken` (established by `StackStoreDetect` at `detect.rs:57-62`), so every `StackStorePhi` whose owning ControlState has a dead predecessor appears in `phi_nodes`. The loop at lines 160-165 then calls `ctx.remove_node_input(phi_node, phi_input_idx)` where `phi_input_idx = dead_idx + 1`. For `dead_idx=0` this removes `inputs[1]` = `memory`; for `dead_idx=1` this removes `inputs[2]` = `data`. The guard `phi_input_idx < phi_len` (where `phi_len == 3`) passes for both cases. Since `StackStorePhi` is non-cacheable (`is_cacheable()` at `kind.rs:285`), `remove_node_input` succeeds without error. The resulting node has 2 inputs instead of 3.

  Layer C explicitly skips `StackStorePhi` in the phi-arity check (`layer_c.rs:163-165` — comment "StackStorePhi has fixed arity 3 enforced elsewhere"), so the corruption goes undetected. Downstream, `step_through_stack_store_phi` (`sp_expr.rs:138-141`) checks `inputs.len() < 3` and returns `MayAlias` (pessimistic but not catastrophic for alias analysis). Any consumer reading `inputs[1]` as `memory` or `inputs[2]` as `data` reads the wrong slot.
- **Fix:** Guard the phi-nodes loop in `dead_branch/mod.rs`:
  ```rust
  for phi_node in phi_nodes {
      if matches!(*ctx.node_kind(phi_node), NodeKind::StackStorePhi { .. }) {
          continue; // fixed-arity 3: [PHI, MEM, DATA]; never remove inputs
      }
      let phi_len = ctx.node_inputs(phi_node).len() as u32;
      if phi_input_idx < phi_len {
          ctx.remove_node_input(phi_node, phi_input_idx)?;
      }
  }
  ```
- **Regression test:** Build a graph where a `ControlState` has 2 predecessors and a `StackStorePhi` under it; eliminate one branch with `DeadBranchElimination`; assert the `StackStorePhi` still has exactly 3 inputs and that `node_kind == StackStorePhi`.

### INV-2 — `stack_phi_offsets` side-table stale after dead-branch elimination
- **Severity:** HIGH (confidence 82); only independently observable after INV-1 is fixed
- **Invariant:** For each `StackStorePhi`, `Graph::stack_phi_offsets[node].len()` equals the number of live predecessors of the owning `ControlState`.
- **Declared at:** CLAUDE.md "asm-fingerprint side-table" section; enforced implicitly by `step_through_stack_store_phi`.
- **Violation path:** `crates/opt/src/dead_branch/mod.rs:130-175` (violation); `crates/opt/src/sp_expr.rs:143-155` (consumption).
- **What's wrong:** After DBE removes a dead predecessor at index `dead_idx` from a ControlState, `Graph::stack_phi_offsets` for any `StackStorePhi` under that ControlState retains N entries (one per *original* predecessor). `step_through_stack_store_phi` iterates all stored offsets and returns `MayAlias` if any entry overlaps the load's query range. A stale dead-predecessor offset that coincidentally overlaps the query range produces a spurious `MayAlias`, blocking `StackLoadForward` from forwarding an otherwise unambiguous store-load pair. This is a silent pessimisation — no assertion fires, but a valid forwarding opportunity is permanently lost.
- **Fix (option A — prune at DBE):** after the INV-1 skip-guard, walk every `StackStorePhi` consumer of the dying ControlState's phi token and remove `stack_phi_offsets[node][dead_idx]`.
- **Fix (option B — validate at consumer):** in `step_through_stack_store_phi`, cross-check `offsets.len()` against the number of live ControlState predecessors and skip stale entries rather than returning `MayAlias`.
- **Regression test:** Build a graph with two SP-disjoint stores on two predecessors; eliminate one branch; assert subsequent `Load` at the surviving offset forwards through to the surviving `StackStore` data (post-fix this works; pre-fix the load remains).

## Categories verified clean

✓ **Graph dedup-cache injection** — `store.rs::create_node` inserts via borrowed-key probe unconditionally; `retain_reachable` rebuilds from scratch in `compact.rs:194-228`.

✓ **`asm_fingerprints` superset / union** — `pipeline.rs::after_replace` and `rewrite.rs::rewrite_rule` union fingerprints into every freshly-created RHS node before `replace_all_uses`. Manual passes (StackStoreDetect, FlagCmpCanonicalize, IfCondInversion) all call `extend_asm_fingerprint_from` after `create_node`.

✓ **`call_clobbered_overrides` invariant** — `SecondaryMap<NodeId, Vec<rsleigh::Vn>>` has no implicit shrinkage path; reads default to the CC's static clobber set when missing.

✓ **`VarPhi` / `MemPhi` arity vs `ControlState` predecessor count** — DBE correctly strips variadic inputs from VarPhi/MemPhi (the `phi_input_idx < phi_len` guard handles the unbounded-tail case). Layer-C `check_layer_c_phis` enforces.

✓ **`wide_consts` interning** — `gc_wide_consts` (`compact.rs:248-297`) rebuilds the dedup table over only live nodes. No `IntConstWide` creation path bypasses interning (`build_int_const_wide` is the only entry).

✓ **CFG `start_addr_to_region_id` consistency** — `add_region` and `split_region` both maintain the map unconditionally; no removal path leaves stale entries.

✓ **`ElfFileMemReader` after `apply_elf_relocations_autoload`** — partial-rollback is the documented contract at `elf.rs:671-681`. `regions.truncate(base_len)` restores on Err. No invariant violation beyond the explicitly documented design.

✓ **`RegionLiftHandles` / per-iteration index consistency** — handles snapshot at iteration close; index rebuilt per-iteration in the orchestrator (`crates/strider/src/orchestrator.rs`).

✓ **`StackStorePhi` fixed-arity 3 contract — at construction** — `StackStoreDetect` always builds with 3 inputs. INV-1 above is the mutation-time violation.

## Files reviewed

- `crates/opt/src/dead_branch/mod.rs`, `crates/opt/src/redundant_phis/mod.rs`
- `crates/opt/src/stack_store/{detect,mod}.rs`, `crates/opt/src/sp_expr.rs`
- `crates/ir/src/{node_signature.rs,validate/layer_c.rs,node/kind.rs}`
- `crates/ir/src/graph/{store,uses,compact}.rs`
- `crates/pattern/src/rewrite.rs`, `crates/opt/src/pipeline.rs`
- `crates/reader/src/elf.rs`
- `crates/strider/src/orchestrator.rs`
