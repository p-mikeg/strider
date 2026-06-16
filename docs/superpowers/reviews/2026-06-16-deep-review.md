# Deep Review 2026-06-16 — 8-dimension every-line review

Branch: `feature/deep-review-2026-06-15`. 8 read-only subagents, one per dimension,
each instructed to ignore comments/memory/prior reviews and verify every claim
against actual code. Findings below carry a **[verified by main]** tag where I
re-checked them myself.

## Headline
- **No soundness bugs found** in any of the three soundness dimensions
  (graph-vs-asm, optimization correctness, code-vs-itself). The lift/opt path is
  in strong shape; agents gave detailed positive verification of the trickiest
  transforms.
- The actionable work is **cleanup + simplification + a few test gaps + one
  perf redesign** (the deferred jump-table cone-extraction).

---

## A. Dead code (agent 6) — all [verified by main], lowest risk

| ID | Item | Location | Action |
|----|------|----------|--------|
| D1 | 4 dead Cargo deps | strider-graph→graphwalk; strider-pattern→entity-utils; strider-reader→strider-ir; strider-reader→strider-target | remove dep lines (zero source refs each) |
| D2 | 4 test-only `EditFunction` methods | `strider-ir/src/function/edit.rs:213-233` `is_live`/`is_root`/`live_snapshot`/`roots_snapshot` | delete or `#[cfg(test)]` |
| D3 | 2 dead pub fns | `graphwalk/src/lib.rs:223,366` `entity_preorder`/`entity_postorder` | delete (tests can call `PreOrder::new`/`PostOrder::new`) |

Cleared (NOT dead): petgraph_view (representation surface), all #[pymethods],
strider-pattern node_count/output_count (already #[cfg(test)]), AliasMode::Strict
(reachable via py), bitor/try_collapse/next_node_id/has_kind (production callers).

## B. Simplification / helpers (agent 1)

| ID | Item | Value/Risk |
|----|------|------------|
| IR-1 | No semantic-slot accessors → ~10 sites hand-roll `node_inputs_exact::<N>(n)[k]` for If-cond, IndirectBranch target, Store addr/data, Load addr. value_range:528 reimplements it worse [verified]. Add `if_cond`/`indirect_branch_target`/`store_addr`/`store_data`/`load_addr`. | HIGH / LOW |
| IR-2 | `value_kind(v).as_value()...byte_size()` chain at 6+ sites → route through existing `value_type_opt` + new `byte_size_of` helper | MED / LOW |
| PAT-1 | BoolConst/FloatConst `MatchPat`/`TemplatePat::compile` byte-identical apart from builder type → route through existing `BuilderLike` | MED-HIGH / LOW |
| LIFT-1 | int/float/bool unary&binary handler 6-7 step skeleton (~140 LOC) → generic `process_binary`/`process_unary` + family trait | MED / MED |
| PAT-2 | Matcher `find_all`/`find_first` + `try_at_node`/`try_match_at_node` copy-paste → one ControlFlow closure | LOW-MED / LOW |
| PAT-3 | 3 near-identical binary-op factory macros → 1 parameterized | LOW / MED |
| OPT-1 | sp_expr `(class,size,space)` triple threaded positionally → `AddrAccess` struct | MED / LOW |
| LIFT-2 | `extract_bit_pos_u8(vn,opcode,label)` opcode derivable | LOW / min |

## C. SSoT / redundant args (agent 7)

| ID | Item | [verified] |
|----|------|-----------|
| R1 | `try_fold_const_load_at(..., endianness)` — derivable from `ctx.function().endianness()`, 1 call site | yes |
| R2 | `apply_one_relocation(obj, ..., endian_le)` — `endian_le` derivable from `obj`, 2 sites (strider-reader) | — |
| — | No state-duplication / boundary violations. Doc drift: LiftOptions `all_vns` no longer a field. | — |
| R3/R4 | weak (need contract tightening) — skip | — |

## D. Runtime (agent 5)

| ID | Item | |
|----|------|--|
| HOT-1 | Jump-table classifier clones whole fn + runs full `default_pipeline` **per table entry**: O(R·Pipeline(N)), R≤4096. `post_opt/indirect_branch_resolve/table.rs:105-134,250-279`. The deferred "O(N) cone-extraction". Real redesign. | dominant analyze cost when a table is present |
| P-LOW | `sp_memo.clear()` between the two SP-reading post-passes wastes warmed cones; safe to drop. `pipeline.rs:536` | constant factor |
| — | Everything else confirmed O(n)/O(n log n). | |

## E. Code-vs-itself (agent 4) — no reachable bugs
- LOW: `StackArgs::index_of`/`slot_of` unchecked `offset - base_offset` (not reachable; presets all base_offset≥0). Harden with `checked_sub` + reject negative base_offset in `try_new`.
- `Endianness::read_uint` documented panic on len>16 — guarded by sole caller. Not a bug.

## F. Edge-case test gaps (agent 8)
| GAP | Item | Risk |
|-----|------|------|
| 1 | x87 **10-byte container** sub-register read/write untested (all aliasing tests use 8/16-byte) | HIGH |
| 4 | jump-table `MAX_TABLE_ENTRIES=4096` boundary (4096 resolves / 4097 defers) untested | MED |
| 3 | outgoing **3-slot odd-span** stack arg cursor advance untested (only 2/4-slot) | MED |
| 2 | wide-const GC after `compact` untested | LOW |
| 5 | zero-collectable-slots degenerate path | LOW-MED |
| 6 | KnownBits popcount/lzcount at I64; SignExtend at I1 | LOW-MED |

Discarded false positives (already covered): ConstantFold div0/INT_MIN/shift-≥-width,
compact side-table remaps, LoadForward narrower-store bail, jump-table single-entry.
