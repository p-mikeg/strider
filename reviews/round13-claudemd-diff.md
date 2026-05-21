# Round 13 — CLAUDE.md correctness diff

Round-12 W2 fixes (E1 `OptimizerOnBuilt → Optimizer` inversion; E4 `IndirectBranchResolve` no-struct) **still hold** in the current CLAUDE.md.  Round-13 3A verified by re-derivation against current code.

## New CLAUDE.md edits proposed

### CMD-13-1 — Layer B reachability scoping (1A finding)

**Current (CLAUDE.md, validator description):**

> Layer B: bidirectional use-list consistency.

**Verifying code:** `crates/ir/src/validate/layer_b.rs:39-41` (backward sweep guard `if !reachable.contains(source) { continue; }`) and `:63-65` (forward check guard).  The implementation is more conservative than the doc — Layer B is reachability-scoped on both directions, not "iterates all nodes".

**Proposed edit:** Append the scoping qualifier:

> Layer B: bidirectional use-list consistency (reachable-only on both source and forward sides; only `check_layer_c_uniqueness` scans the full arena).

---

## No other CLAUDE.md edits required

All other CLAUDE.md claims verified by round-13 3A (25 confirmed, 0 partial, 2 refuted but in the root README + strider README, not CLAUDE.md).
