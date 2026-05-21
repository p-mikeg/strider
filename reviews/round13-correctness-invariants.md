# Round 13 — Ask-8 pass 2: invariant-violation audit

Branch: `review/ai7`.

## Verdict

**2 findings.** Both rev1ve the R12 INV-1/INV-2 (DBE corrupts StackStorePhi) — Round 12 W1 verification concluded those were not real because the typical const-If path goes through an intermediate region's CS.  Round 13's re-derivation flags a narrower scenario where the precondition CAN be met: a function with conditional `alloca` / inline-asm that diverges SP across branches.

## Findings

### INV13-1 — `DeadBranchElimination` can remove inputs from `StackStorePhi` (fixed-arity-3 violation, narrow trigger)
- **Severity:** MED (confidence 82) — narrow precondition but real correctness path
- **Invariant:** `node_signature.rs:352` — `StackStorePhi` has fixed arity 3: `[PhiToken, Memory, Data]`.  Layer A enforces this for all reachable nodes at every `OptimizerPipeline::run → validate`.
- **Violation path:** `crates/opt/src/dead_branch/mod.rs:146-165`.  After confirming `!dead_subgraph_escapes`, the cleanup loop iterates `dead_uses`.  For each `(cs_node, dead_idx)` it collects `phi_nodes = output_uses(cs_phi_out)` — ALL consumers of the ControlState's PhiToken output.  `StackStorePhi.inputs[0]` is exactly that PhiToken (set by `StackStoreDetect` at `detect.rs:57-60`).  So StackStorePhi nodes appear in `phi_nodes`.  Line 163: `remove_node_input(phi_node, dead_idx + 1)` removes input[1]=MEMORY or input[2]=DATA from the fixed-arity-3 node.  Guard `phi_input_idx < phi_len` passes (phi_len=3 for StackStorePhi).
- **Trigger:** A join ControlState must have both (a) a dead control input from a const-If, AND (b) a `StackStorePhi` consuming its PhiToken.  (b) requires `Store` address that decomposes to `VarPhi(sp)` at that ControlState — i.e., SP diverges across the branches.  Conditional `alloca`, inline-asm modifying SP, or some bare-metal / kernel code patterns can produce this.  Round 12's INV-1 dismissal noted that "the dead-ctrl from const-If is consumed by intermediate region CSes, not by the join CS where StackStorePhi lives."  That holds for *typical* ABI-compliant functions, but the SP-divergent case is a legitimate path that violates the invariant.
- **Fix:** Add a kind-guard in the phi-node loop:
  ```rust
  for phi_node in phi_nodes {
      if matches!(*ctx.node_kind(phi_node), NodeKind::StackStorePhi { .. }) {
          // Fixed arity 3; per-pred info lives in stack_phi_offsets.
          let mut offsets = ctx.stack_phi_offsets(phi_node).to_vec();
          if (dead_idx as usize) < offsets.len() {
              offsets.remove(dead_idx as usize);
              ctx.set_stack_phi_offsets(phi_node, offsets);
          }
          continue;
      }
      let phi_len = ctx.node_inputs(phi_node).len() as u32;
      if phi_input_idx < phi_len {
          ctx.remove_node_input(phi_node, phi_input_idx)?;
      }
  }
  ```
- **Regression test:** Construct a function with `if cond { sp -= 16 } else { sp -= 32 }; *sp = …; if (const true) {…} else {…}` so a `StackStorePhi` appears at the inner ControlState and an upstream const-If creates a dead branch.  Run DBE.  Assert StackStorePhi inputs unchanged at 3.

### INV13-2 — `stack_phi_offsets` stale after DBE (consequential to INV13-1)
- **Severity:** MED (confidence 80)
- **Where:** `crates/opt/src/sp_expr.rs:143-155`; `crates/opt/src/dead_branch/mod.rs`.
- **Invariant (implicit):** `stack_phi_offsets[n]` reflects only live predecessors.
- **Path:** After DBE removes the dead predecessor from the ControlState, the CS has `n-1` inputs.  With INV13-1 fixed (StackStorePhi inputs untouched), the StackStorePhi still has its 3 inputs, but `stack_phi_offsets` retains all N original entries.  `step_through_stack_store_phi` iterates all stored offsets and returns `MayAlias` if any overlaps the query range.  A stale dead-pred offset overlapping the query produces a spurious `MayAlias`, blocking `StackLoadForward` from forwarding a valid store-load pair.  Silent pessimisation (sound but pessimistic).
- **Fix:** Included in INV13-1's recommended change (the `offsets.remove(dead_idx)` line in the StackStorePhi continue arm).

## Categories verified clean

✓ **Graph dedup-cache injection** — `add_node_input` / `remove_node_input` guard against cacheable; `set_node_kind` guards against cacheable; `update_input` evicts before mutating; `compact::retain_reachable` rebuilds from scratch.  No bypass.

✓ **asm_fingerprints superset/union** — `pipeline::after_replace` and `pattern::rewrite_rule` union fingerprints before `replace_all_uses`.  Manual passes all call `extend_asm_fingerprint_from`.  `FunctionArgDetect::detect_stack_args` intentionally omits stack-arg inheritance (documented).

✓ **`BuiltFunctionGraph::call_clobbered`** — field `pub(crate)`; only `set_call_clobbered_for_test` mutates post-construction.

✓ **VarPhi/MemPhi arity vs CS pred count** — DBE maintains via `phi_input_idx = dead_idx + 1` + CS removal at `dead_idx`.  Layer C `check_layer_c_phis` enforces.

✓ **wide_consts interning** — `intern_wide_const` is the only `IntConstWide` path.  `gc_wide_consts` rebuilds correctly.

✓ **CFG `start_addr_to_region_id` consistency** — `add_region` and `split_region` both maintain the map.

✓ **`ElfFileMemReader` partial rollback** — `apply_elf_relocations_with_extender` documents partial rollback (elf.rs:671-681); `regions.truncate(base_len)` restores on Err.

✓ **`RegionLiftHandles` per-iteration index** — `RegionIndex` rebuilt from each iteration's snapshot (orchestrator.rs:144-149).

✓ **StackStorePhi fixed-arity 3 at construction** — `StackStoreDetect` always creates with exactly `[phi_token, memory, data]`.  Mutation-time violation only (INV13-1 above).

## Files reviewed

- `crates/opt/src/dead_branch/mod.rs`, `redundant_phis/mod.rs`
- `crates/opt/src/stack_store/{detect,mod}.rs`, `crates/opt/src/sp_expr.rs`
- `crates/ir/src/{node_signature.rs, validate/layer_c.rs, node/kind.rs}`
- `crates/ir/src/graph/{store,uses,compact}.rs`
- `crates/pattern/src/rewrite.rs`, `crates/opt/src/pipeline.rs`
- `crates/reader/src/elf.rs`
- `crates/strider/src/orchestrator.rs`
