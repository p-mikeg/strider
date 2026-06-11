# Strider Deep Code Review — Consolidated Report

**Date:** 2026-06-11
**Scope:** Whole-workspace deep review across seven dimensions — three soundness
(`asm`, `opt`, `self`) and four quality (`simplify`, `runtime`, `filesplit`,
`clean`).
**Status of inputs:** Almost every raw finding is low-severity / design and was
passed through "unverified" for human triage; a handful were independently
verified (`confirmed`). This report deduplicates the raw set, ranks within each
dimension by (severity, confidence), and assigns stable ids.

---

## Executive Summary

### Counts by dimension + severity (after dedup)

| Dimension | Class | High | Medium | Low | Total |
|-----------|-------|------|--------|-----|-------|
| asm  | soundness | 0 | 0 | 2 | 2 |
| opt  | soundness | 0 | 0 | 6 | 6 |
| self | soundness | 0 | 3 | 18 | 21 |
| **soundness subtotal** | | **0** | **3** | **26** | **29** |
| simplify | quality | 0 | 0 | 24 | 24 |
| runtime  | quality | 0 | 0 | 5 | 5 |
| filesplit| quality | 0 | 3 | 2 | 5 |
| clean    | quality | 0 | 0 | 2 | 2 |
| **quality subtotal** | | **0** | **3** | **33** | **36** |
| **TOTAL** | | **0** | **6** | **59** | **65** |

No high-severity (live miscompile) finding survived. The six medium-severity
items are three doc/contract defects in the Python bindings (soundness lens) and
three module-organization defects in the SP-aware memory-SSA subsystem
(filesplit lens). Everything else is low-severity hardening, doc accuracy, or
behavior-preserving cleanup.

### Highest-priority items overall (soundness first)

1. **SND-01** (medium, conf .82) — `PyPartialMatch.__getitem__` probes uint
   before bool, so a boolean capture surfaces as a plain `int` inside a
   `.when()` predicate, while `PyMatch.__getitem__` (post-match) returns
   `bool`. Same `m[c]` lookup, two Python types. `pattern.rs:1199-1211`.
2. **SND-02** (medium, conf .80/.78) — `node_count` / `node_ids` docstrings say
   "reachable from entry" but call `all_node_ids()` (whole arena, no compaction
   after in-place optimize/rewrite), over-reporting dead nodes.
   `function.rs:256-281`.
3. **SND-03** (medium, conf .82) — Python opt post-pass docs say "four" /
   enumerate inconsistently across three locations that have already drifted
   from the actual reject set. `opt.rs:29-31, 255-257, 491-493`.
4. **OPT-01** (low, conf .40) — `peel_to_u64_const` SignExtend arm can
   sign-extend a code address into the high 64 bits, pushing a bogus resolved
   `Multiple` jump-target when no outer `And` mask follows. The most
   safety-relevant `opt` item (produces a CFG edge). `table.rs:500-508`.
5. **OPT-02** (low, conf .30) — KnownBits And/Or/Xor leave `ones`/`zeros`
   unmasked, trusting an operand-width==output-width invariant the validator
   does not enforce. `known_bits/mod.rs:166-179`.
6. **SELF-WALK-SIZE** (low, conf .30) — `reaching_store` derives store width with
   `map_or(0,…)` while the sibling `store_value_byte_size` panics on the same
   invariant; a fabricated width-0 store maps a real arg to zero slots.
   `sp_expr/walk.rs:398-407`.
7. **FS-01** (medium, conf .80) — `sp_expr/walk.rs` is misnamed: it contains no
   graph walk (the walk lives in `memory_ssa/`); it holds the alias taxonomy.
8. **FS-02** (medium, conf .60) — `memory_ssa` is a top-level sibling module
   whose only production consumer is `sp_expr`; the two halves of one analysis
   are split across top-level modules.

### Recommended fix ordering

1. **Doc/contract corrections (cheap, ship first):** SND-01 (reorder two
   blocks), SND-02 (one docstring or one `walk().count()` swap, both
   accessors), SND-03 (correct the count word + consolidate the post-pass list),
   plus the doc-only `self`/`clean` items CLN-01, CLN-02, SELF-DOC-* — all are
   single-line edits with no behavior change.
2. **Soundness hardening (defensive, no live bug):** OPT-01 (mask SignExtend to
   entry width), OPT-02 (mask the four KnownBits fields), SELF-WALK-SIZE (route
   through the shared helper so the invariant is enforced identically).
3. **Filesplit reorg of the memory-SSA subsystem:** FS-01 → FS-02 → FS-03 →
   FS-04 as one coordinated rename/move sequence (do FS-01 rename first; the
   re-export names don't change so no consumer churn).
4. **Behavior-preserving dedup (quality, batchable):** the `simplify` cluster,
   highest-confidence first (SIMP-IRDOT-CAST .83, SIMP-IRDOT-WIDE .80,
   SIMP-PAT-WIDE .70, SIMP-IREDIT .86 …).
5. **Runtime micro-opts (only if a profile justifies):** RT-* are all O(n) or
   benign-O(n·k) with small k; defer unless region/site counts grow.

---

## Soundness dimensions

### `self` — internal consistency / contracts (most populous)

| id | sev | conf | file:lines | problem (one line) |
|----|-----|------|-----------|--------------------|
| SND-01 | medium | .82 | py/pattern.rs:1199-1211 | `PyPartialMatch.__getitem__` uint-before-bool inverts documented bool-first order; bool capture surfaces as int |
| SND-02 | medium | .80/.78 | py/function.rs:256-281 | `node_count`/`node_ids` say "reachable" but return whole arena (no compaction after in-place opt) |
| SND-03 | medium | .82/.55 | py/opt.rs:29-31,255-257,491-493 | post-pass doc says "four" but lists three; three drifted enumerations of the post-pass set |
| SELF-WALK-SIZE | low | .30 | opt/sp_expr/walk.rs:398-407 | `reaching_store` uses `map_or(0)` while sibling helper panics; width-0 store maps arg to 0 slots |
| SELF-IREDIT-DUP | low | .86 | ir/function/edit.rs:611-622 | `replace_value` computes `producer(old)` twice (guaranteed equal) |
| SELF-PY-OPTCOUNT | low | .82 | py/opt.rs:255-257 | (folded into SND-03) "four" vs three names |
| SELF-CFGREGION-DOC | low | .82 | cfg/region_builder.rs:496-520 | stale doc: helper claims to be shared with direct Branch arm; it has one caller |
| SELF-IREDIT-NOOP | low | .55 | ir/function/edit.rs:611-623 | `replace_value` does fingerprint+enqueue work on no-op `old==new` self-replace |
| SELF-IRNODE-BOOLDOC | low | .55 | ir/node/value_kind.rs:69-73 | `is_bool` doc references a nonexistent `Bool` ValueType |
| SELF-IRBUILD-TRUNC | low | .50 | ir/builder/builder_ext.rs:61-72 | `truncate_if_needed` widens constants but leaves non-constants narrow (type-asymmetry) |
| SELF-GRAPH-RAU | low | .50 | strider-graph/graph.rs:432-439 | `replace_all_uses` returns `true` and walks all uses even when `old==new_val` |
| SELF-VNIO-DOC | low | .70 | ir/builder/vn_io.rs:188-201 | doc claims float bit-reinterpret that `convert_to_int_if_needed` never does |
| SELF-VALRANGE-UNREACH | low | .45 | opt/value_range/mod.rs:203-219 | dead `unwrap_or_else(top)` fallback in multi-input phi union |
| SELF-IRBUILD-FP | low | .30 | ir/builder/builder_ext.rs:555-562,594-598 | IntConst↔FloatConst folds don't union source const's asm-fingerprint |
| SELF-IRCALL-EXCL | low | .40 | ir/builder/call.rs:112-119 | `terminate`+`advance_memory` mutual-exclusivity documented but unenforced on a pub API |
| SELF-IREDIT-ATTACH | low | .40 | ir/function/edit.rs:244-305 | `will_attach_value` can over-mark liveness if attached onto a dead consumer (documented, unguarded) |
| SELF-LIFTCAST | low | .25 | lift/cast.rs:114-123 | `handle_subpiece` feeds `read_vn` into a strict shift without `convert_to_int_if_needed` (siblings coerce) |
| SELF-VALRANGE-SINK | low | .20 | opt/value_range/mod.rs:680-689 | `single_control_consumer` returns first use without asserting the single-sink invariant |
| SELF-GRAPH-CACHE | low | .30 | strider-graph/node_cache.rs:99-113 | rehash closure trusts non-sentinel `node_hashes` invariant with no debug-assert |
| SELF-PEEP-LIVE | low | .30 | opt/peephole.rs:142-158 | re-enqueued node may be killed before dequeue; doc overstates the liveness contract |
| SELF-PATMATCH-TY | low | .20 | pattern/matcher/walk.rs:287-295 | post-match `ty` silently defaults to I1 for non-value (control) roots |
| SELF-PATTEMPL-OUT | low | .40 | pattern/template/mod.rs:281-298 | output-slot mapping indexes a gap-closed vector by declared slot (asymmetric vs inputs) |
| SELF-PATTEMPL-FLT | low | .30 | pattern/template/mod.rs:241-252 | `FnIntConst` on a float/zero-width output silently yields `IntConst(Small(0))` |
| SELF-PAT-WIDEONES | low | .30 | pattern/typed/value_ops.rs:309-321 | BitNot template all-ones only correct ≤128 bits; I256/I512 upper words zero-filled |
| SELF-PATBUILD-IDX | low | .30 | pattern/graph_ext.rs:71-78 | `consumed_inputs` indexes `input_slots[i]` raw; panics on a desync instead of erroring |
| SELF-PATBUILD-BR | low | .25 | pattern/node_builders/flow.rs:297-312 | `with_true`/`with_false` silently overwrite a prior call instead of composing |
| SELF-PATBUILD-VN | low | .60 | pattern/match_result.rs:88-112 | `get_vn` resolves the binding's node twice (`value_definition` then `get_node`) |
| SELF-UTIL-EDGE | low | .40 | orchestrator/lib.rs:380-400 | `edge_set_of` collapses `Single(k)` and `Multiple([k])`; a kind-flip reads as "no change" |

**SND-01 fix:** reorder `PyPartialMatch::__getitem__` to probe `get_bool`
before `get_uint`, mirroring `PyMatch::__getitem__`. Confirmed accurate; behavior
of wider ints is preserved because `get_bool` is I1-only.

**SND-02 fix:** pick one contract for both accessors. Since the sibling
`count_regions` already uses the entry walk, switch both to
`function.walk().count()` / collect over `function.walk()`; alternatively reword
both docstrings to "all arena node ids (reachable or not)". Confirmed.

**SND-03 fix:** correct "four" → "three" in `add`'s doc and consolidate the
post-pass enumeration to one canonical comment/constant referenced by
`ErasedPostPass`, `add`, and the `into_erased` reject arm so they cannot drift.
(SELF-PY-OPTCOUNT folded in here.)

**SELF-WALK-SIZE fix:** call the shared `store_value_byte_size(function.graph(),
data)` so both the panic-on-malformed-IR path and the reaching-store path enforce
the same invariant rather than fabricating a width-0 store that downstream
`ceil(size/increment)` slot-span math mishandles.

**SELF-IREDIT-DUP fix (highest confidence, .86):** reuse `from` for
`old_producer`; `replace_all_uses` between the two lookups never reassigns
`old`'s producer.

### `opt` — optimizer soundness

| id | sev | conf | file:lines | problem (one line) |
|----|-----|------|-----------|--------------------|
| OPT-01 | low | .40 | opt/indirect_branch_resolve/table.rs:500-508 | SignExtend peel can sign-extend a code address into high 64 bits → bogus resolved target |
| OPT-02 | low | .30 | opt/known_bits/mod.rs:166-179 | And/Or/Xor leave ones/zeros unmasked; trusts unenforced operand==output width |
| OPT-03 | low | .45 | opt/constant_fold/eval_int.rs:40-43 | `bit_mask_u128` is `u128::MAX` for I256/I512; sound only because wide consts never reach here |
| OPT-04 | low | .25 | opt/indirect_branch_resolve/table.rs:568-577 | `extract_idx_and_stride` accepts stride 0; enumeration reads N identical entries (dedup saves it) |
| OPT-05 | low | .20 | opt/sp_expr/decompose.rs:236-255 | And-arm treats any constant-masked SP value as a stack base, including non-alignment masks |

**OPT-01 fix:** after computing `signed`, mask to the Extend output's declared
width before returning (`(signed as u64) & out_ty.bit_mask_u64()`), matching the
Truncate arm — sign-extension beyond the entry's own width is never a valid jump
target.

**OPT-02 fix:** `& type_mask` all four lattice fields in the And/Or/Xor arms,
mirroring the already-masked sibling fields and the shift arms — cheap hardening
that removes the dependency on an invariant the validator does not enforce.

**OPT-03/OPT-05 fix:** document the precondition (or add a `debug_assert`) at the
fold site; both are currently unreachable but the safety lives two layers away
from the code that relies on it.

### `asm` — target / ABI fidelity

| id | sev | conf | file:lines | problem (one line) |
|----|-----|------|-----------|--------------------|
| ASM-01 | low | .30 | target/call_other_abi.rs:223-233 | x86_64 syscall ABI omits the flags register from `implicit_writes` (kernel clobbers RFLAGS) |
| ASM-02 | low | .25 | target/calling_convention/mod.rs:568-585 | MIPS O32 callee-saved list includes `gp`, which is caller-saved scratch in PIC |

**ASM-01 fix:** add the Sleigh eflags/rflags varnode name to the syscall row's
`implicit_writes` (same for ARM `swi` / x86 `swi`), or document why flag-clobber
is intentionally not modeled.

**ASM-02 fix:** confirm against the in-scope ABI; if PIC, drop `gp` from
`callee_saved_regs` so `Call` clobbering stays conservative. Otherwise document
the fixed-`$gp` assumption.

---

## Quality dimensions

### `simplify` — reuse / dedup / behavior-preserving cleanup

| id | sev | conf | file:lines | problem (one line) |
|----|-----|------|-----------|--------------------|
| SIMP-IREDIT | low | .86 | ir/function/edit.rs:611-622 | (= SELF-IREDIT-DUP) double `producer(old)` lookup |
| SIMP-IRDOT-CAST | low | .83 | ir/function/dot/label.rs:205-267 | ~10 cast/conversion arms repeat `"{prefix}\n{from} → {to}"` (confirmed) |
| SIMP-IRDOT-WIDE | low | .80 | ir/function/dot/{raw,label}.rs | wide-const hex rendering duplicated and already drifted (confirmed) |
| SIMP-PAT-WIDE | low | .70 | pattern/typed/consts.rs:235-242 | AnyIntConst Wide arm reinlines `first_int_const_value` |
| SIMP-WALK-SIZE | low | .72 | opt/sp_expr/walk.rs:398-401 | (= SELF-WALK-SIZE quality face) reinlines `store_value_byte_size` with divergent error handling |
| SIMP-PATBUILD-VN | low | .60 | pattern/match_result.rs:88-112 | (= SELF-PATBUILD-VN) double binding→node resolution |
| SIMP-IRDATA | low | .60 | ir/function/data.rs:401-478 | `call_ret_vals_for`/`call_clobbered_for` rebuild the same callee-saved/ret sets |
| SIMP-CONSTFOLD-RIGHT | low | .60 | opt/constant_fold/rules.rs:197-226 | five const-on-right rules duplicate an identical body (only op ctor differs) |
| SIMP-MEMSSA-SUCC | low | .60 | opt/memory_ssa/mod.rs:300-342 | (also RT-MEMSSA) successors() recomputed twice per node |
| SIMP-WALK-STORE3 | low | .55 | opt/sp_expr/walk.rs:176-180,122-129 | Store [mem,addr,data] + size re-extracted via 3 separate paths from one Store id |
| SIMP-IRBUILD-BITCAST | low | .55 | ir/builder/builder_ext.rs:535-600 | int↔float bitcast constructors duplicate the skeleton |
| SIMP-CONSTFOLD-DROP | low | .55 | opt/constant_fold/rules.rs:360-408 | two guard closures repeat the get_uint+truncate_low_mask extraction |
| SIMP-RDR-SYMSLOT | low | .55 | reader/elf/relocations.rs:229-260 | GOT/PLT-slot and MIPS-REL32 arms identical except size detector |
| SIMP-PATBUILD-MEM | low | .55 | pattern/node_builders/memory.rs:118-339 | LoadPat/StorePat ~120 lines mirrored (shared filter set) |
| SIMP-PY-OPACC | low | .55 | py/matcher.rs:184-225 | six op-variant accessors byte-identical except bindings method |
| SIMP-IRBUILD-BINARY | low | .50 | ir/builder/builder_ext.rs:309-445 | strict two-operand build idiom repeated across binary/cmp builders |
| SIMP-CONSTFOLD-SCARRY | low | .50 | opt/constant_fold/eval_int.rs:167-225 | Scarry/Sborrow arms near-identical; share a signed-overflow helper |
| SIMP-CONSTFOLD-ERR | low | .50 | opt/constant_fold/eval_int.rs:144-148 | `eval_int_cmp` reinvents the "expected integer type" error already in `require_signed` |
| SIMP-PATBUILD-OP | low | .50 | pattern/bindings.rs:245-316 | six `get_*_op` extractors repeat node-lookup + single-arm match |
| SIMP-PY-PARTIAL | low | .50 | py/pattern.rs:1159-1211 | PyPartialMatch typed-accessor dispatch duplicates PyMatch (already drifted → SND-01) |
| SIMP-PATTEMPL-INT | low | .45 | pattern/template/mod.rs:208-253 | FnIntConst hand-rolls value_ty→IntPayload routing the IR builder already owns |
| SIMP-PATTYPED-BOOL | low | .45 | pattern/typed/value_ops.rs:820-835 | `bool_binary_out`/`bool_one_out` one-off helpers duplicate `compile_bool_binary` tail |
| SIMP-IRNODE-SLOT | low | .30 | ir/node/node_signature.rs:190-262 | three identical AnyValue slot consts differ only in name/role |
| SIMP-IRDOT-CMP | low | .55 | ir/function/dot/label.rs:232-234 | IntCmpOp/FloatCmpOp arms duplicate `"{op}\n{from} → i1"` |
| SIMP-IRVIEWER | low | .40 | ir/viewer.rs:317-369 | five near-duplicate `require_*_kind` guard methods |
| SIMP-GRAPH-EXACT | low | .40 | strider-graph/graph.rs:180-217 | `node_inputs_exact`/`node_outputs_exact` share arity-check + array scaffold |
| SIMP-RDR-SEC | low | .40 | reader/elf/sections.rs:159-221 | segment/section collectors duplicate accept/parse/empty-skip/push skeleton |
| SIMP-IRDOT-RAM | low | .40 | ir/function/dot/label.rs:39-51 | `pretty_vnspace` redundant RAM arm overlaps the code-space "ram" case |
| SIMP-PATTEMPL-TY | low | .30 | pattern/template/mod.rs:323-374 | `node_value_ty`/`output_kinds_for` re-derive value-output resolution two ways |

**Top simplify picks (do first, highest confidence, behavior-preserving):**
- **SIMP-IREDIT (.86)** — drop the duplicate `producer(old)` lookup.
- **SIMP-IRDOT-CAST (.83, confirmed)** — add `fn cast_label(&self, node, prefix)
  -> String` and collapse the ~10 cast/conversion arms (and SIMP-IRDOT-CMP via a
  sibling `cmp_label`).
- **SIMP-IRDOT-WIDE (.80, confirmed)** — extract one `fmt_wide_const(storage) ->
  String` in `dot/mod.rs`; both `raw.rs` and `label.rs` call it (reconcile the
  trim strategy, value 0 must match).
- **SIMP-PAT-WIDE (.70)** — replace the AnyIntConst Wide arm body with
  `first_int_const_value(f, ir_node).is_some()`.

### `runtime` — algorithmic / allocation cost

| id | sev | conf | file:lines | problem (one line) |
|----|-----|------|-----------|--------------------|
| RT-MEMSSA | low | .60 | opt/memory_ssa/mod.rs:300-342 | `successors()` recomputed twice per node; second pass re-collects MemPhi inputs (O(2E), avoidable alloc) |
| RT-IRDATA | low | .50 | ir/validate/graph_invariants.rs:234-244 | per override-Call validation re-derives full reg lists, O(C·N) over all_vns |
| RT-RDR-AUTOLOAD | low | .55 | reader/elf/relocations.rs:595-636,567-588 | `find_loadable_section_containing` re-parses section data per uncovered site (no memo); winner parsed twice |
| RT-RDR-PATCH | low | .30 | reader/elf/relocations.rs:337,945-957 | pass-2 patch loop still linear-scans regions per relocation despite pass-1 CoverageIndex |
| RT-GRAPH-RMINPUT | low | .45 | strider-graph/graph.rs:378-397 | `remove_node_input` is O(k); front-removal in a loop is O(k²); no back-to-front/batch helper |

**Top runtime picks:** all are O(n)/O(n·k) with bounded k (no live O(n²) on
real inputs), so these are *defer-unless-profiled*. If acted on:
- **RT-MEMSSA (.60)** — compute successors once at `Frame::Enter`, carry into
  `Frame::Exit`; halves successor derivation and removes the second MemPhi-input
  allocation. (Shares the fix with SIMP-MEMSSA-SUCC.)
- **RT-IRDATA (.50)** — validation needs only the *count*; add a count-only /
  combined-split derivation (also resolves SIMP-IRDATA) or memoize by CC identity
  for the pass duration.
- **RT-RDR-AUTOLOAD (.55)** — build a sorted (addr,end,section) index once and
  cache `sec.data()` so the winning section is parsed once, not per-site-twice.

### `filesplit` — module organization

| id | sev | conf | file:lines | problem (one line) |
|----|-----|------|-----------|--------------------|
| FS-01 | medium | .80 | opt/sp_expr/walk.rs (whole) | misnamed: contains the alias taxonomy, not a graph walk (the walk is in `memory_ssa/`) |
| FS-02 | medium | .60 | opt/memory_ssa/mod.rs + lib.rs:43 | `memory_ssa` is a top-level sibling whose sole production consumer is `sp_expr` |
| FS-03 | medium | .55 | opt/memory_ssa/mod.rs:84-368 | one file conflates oracle trait, phi-join policy, and DFS engine |
| FS-04 | low | .45 | opt/sp_expr/walk.rs (layers) | one file bundles verdict table, oracle adapter, and pass façade (three change-reasons) |
| FS-05 | low | .40 | opt/sp_expr/decompose.rs:46-106 | `int_const_signed` is a generic IR const-peeler miscoloured as SP code (also imported by table.rs) |

#### Proposed target module tree

The five filesplit findings converge on one reorganization: pull the entire
SP-aware memory-SSA subsystem under `sp_expr`, and split each file along its
single responsibility.

```
crates/strider-opt/src/
└── sp_expr/
    ├── mod.rs              # re-exports unchanged; doc bullets corrected
    │                       #   (drop the "walk = alias classification" line)
    │
    ├── decompose.rs        # SpDecomposer only (SP-rooted decomposition)
    │   FS-05 → move int_const_signed OUT to a neutral home:
    │
    ├── alias.rs            # ← was walk.rs (FS-01 rename)
    │                       #   verdict layer (FS-04 split):
    │                       #   AddrClass, AliasVerdict, classify_addr,
    │                       #   cmp_same_class_offsets, alias_verdict,
    │                       #   store_alias_verdict  (+ its verdict-table tests)
    │
    ├── cfg.rs              # ← façade + oracle (FS-04 split):
    │                       #   SpAliasCfg, SpAliasOracle, ReachingSpStore
    │
    └── mem_ssa/            # ← was top-level memory_ssa/ (FS-02 move)
        ├── mod.rs          # MemorySSAWalker trait + find_nearest_clobber /
        │                   #   may_clobber default methods (the public seam) (FS-03)
        ├── engine.rs       # MemSsaWalk, Resolve, Frame, join_phi_results
        │                   #   (the private DFS engine) (FS-03)
        └── tests.rs        # engine + trait tests, moved with the code

crates/strider-opt/src/
└── int_helpers.rs (or fold next to other const readers)   # FS-05 new home
    pub(crate) fn int_const_signed(function, value) -> Option<i64>
    # imported by sp_expr/decompose.rs AND indirect_branch_resolve/table.rs
    # so neither has to reach into sp_expr for a non-SP utility
```

Sequencing note: do **FS-01** (rename `walk.rs` → `alias.rs`, edit `mod walk;`
and one doc bullet) first — re-exported item names are unchanged, so no consumer
import changes. Then **FS-02** (move `memory_ssa/` under `sp_expr/mem_ssa/`;
`SpAliasOracle`'s `use crate::memory_ssa::MemorySSAWalker;` becomes a `super`
path). Then the intra-file splits **FS-03** (mem_ssa trait vs engine) and
**FS-04** (alias verdict vs façade). **FS-05** is independent and can land any
time.

### `clean` — small hygiene

| id | sev | conf | file:lines | problem (one line) |
|----|-----|------|-----------|--------------------|
| CLN-01 | low | .90 | ir/function/dot/label.rs:308-315 | copy-paste-drifted doc: two contradictory `Returns the …` summary sentences |
| CLN-02 | low | .55 | ir/function/dot/render.rs:36-43 | needless intermediate `walk` binding in `iter_nodes` |

**CLN-01 fix:** delete the dangling first sentence so the doc starts at "Returns
the label for a Call / CallOther output past [Control, Memory]."
**CLN-02 fix:** return the collected vector directly.

---

## Deduplication notes

The raw set contained these overlaps, merged here:

- **Store-data byte-size divergence** — raw `self`@398-407 and `simplify`@398-401
  describe the same `map_or(0)` vs `store_value_byte_size` issue → **SELF-WALK-SIZE**
  (soundness) / **SIMP-WALK-SIZE** (quality face). `SIMP-WALK-STORE3` is the
  *broader* "one Store id, three extraction paths" helper idea and is kept
  distinct.
- **Python `node_count` + `node_ids`** — both are the one "all_node_ids vs
  reachable, no compaction" defect → **SND-02**.
- **Python opt post-pass doc drift** — `add` "four-vs-three" and the
  `ErasedPostPass`/`add`/`into_erased` three-copy drift are the same
  documentation-source problem → **SND-03** (folding SELF-PY-OPTCOUNT and
  SIMP-PY-OPTDRIFT).
- **`replace_value` double producer lookup** — appears as both `self` and
  `simplify` → **SELF-IREDIT-DUP** / **SIMP-IREDIT** (same fix).
- **`get_vn` double binding resolution** — `self` and `simplify` faces →
  **SELF-PATBUILD-VN** / **SIMP-PATBUILD-VN**.
- **memory_ssa `successors()` twice** — tagged `runtime` in the raw set but with
  unit `MEM-simplify`; surfaces in both lenses → **RT-MEMSSA** / **SIMP-MEMSSA-SUCC**
  (one fix: carry successors into the Exit frame).

Cross-cutting **"pass the derived datum, not the NodeId, twice"** pattern (the
prompt's example) recurs across SELF-IREDIT-DUP, SELF-PATBUILD-VN,
SIMP-WALK-STORE3, and SIMP-PATTEMPL-TY — each independently re-derives a value
already determined by a known id. They are kept as separate ids (different files,
different fixes) but share the same root cause and should be reviewed together.

---

## Verification addendum (2026-06-11, post-review re-check)

The fan-out verifiers refuted **zero** of 78 findings, which warranted manual
re-verification before any code change. Every item below was re-checked against
the live code by hand; **two of the report's own fix recommendations were wrong**
and were corrected before landing.

### Corrections to this report

- **SND-02 — the report's preferred fix is unsound.** It suggested switching
  `node_count`/`node_ids` to an entry walk ("reachable"). But
  `tests/python/test_compact.py` *depends on the arena semantics*
  (`uncompacted.node_count() > after.node_count()`; line 49: "uncompacted arena
  keeps culled nodes"). An entry walk would make compacted and uncompacted
  counts equal and break that suite. The **behavior is correct; only the
  docstrings were wrong** — fixed to say "arena (reachable or not)".
- **SND-03 — description was imprecise.** The drift is at `opt.rs:255` ("The
  **four** single-shot post-passes" followed by a list of **three**); the
  reject arm (`491-493`) handles exactly those three. `IndirectBranchClassify`
  is a post-pass but is orchestrator-appended, not user-registerable, so the
  type doc at `opt.rs:27-30` listing four is accurate in its own context. Fix
  was a one-word count correction, not a multi-site "drifted enumeration".

### Landed on `develop`

| commit | findings actioned |
|--------|-------------------|
| `ebca30fb` | SIMP-IRDOT-CAST, SIMP-IRDOT-CMP, CLN-01, SIMP-IREDIT / SELF-IREDIT-DUP |
| `b7b10879` | OPT-01 (SignExtend mask), OPT-02 (KnownBits field masking), SELF-WALK-SIZE / SIMP-WALK-SIZE |
| `7c850ce2` | SND-01 (bool-before-uint, + regression test), SND-02 (docs), SND-03 (count) |

All three batches: per-crate `cargo test` + `cargo clippy` green; full Python
suite (852 tests) green after a maturin rebuild. SND-01 followed a real TDD
red→green (the regression test failed against the pre-fix module).

### Deliberately NOT actioned (with reasons)

- **OPT-03 / OPT-04 / OPT-05** — guard/precondition documentation for paths that
  are currently unreachable (wide consts never reach `bit_mask_u128`; stride-0 is
  dedup-saved; non-alignment SP masks don't occur). Per the project's
  "trust validator invariants / panic on structural invariants" stance, adding
  `debug_assert`s for unreachable paths is marginal; left as-is.
- **ASM-01 / ASM-02** — syscall RFLAGS clobber and MIPS-O32 `$gp` are ABI-table
  judgment calls that need confirmation against the in-scope ABI before changing
  `implicit_writes` / `callee_saved_regs`; flagged for the maintainer, not
  auto-changed (a wrong edit here is a real miscompile).
- **Runtime (RT-\*)** — all O(n)/O(n·k) with small k; the report itself marks
  them defer-unless-profiled. RT-MEMSSA in particular touches the documented
  read-walk-vs-narrow-mutate borrow boundary in the memory-SSA walker and is not
  worth destabilizing without a profile.
- **Remaining `simplify` backlog** — the lower-confidence micro-dedups
  (SIMP-CONSTFOLD-\*, SIMP-PATBUILD-\*, SIMP-IRBUILD-\*, …) are real but low-value
  churn in an already heavily-audited codebase; left for cherry-picking rather
  than landed en masse.
- **Filesplit (FS-01..05)** — all five structural claims independently verified
  accurate (walk.rs holds the alias taxonomy not a walk; `memory_ssa` has a
  single production importer; `int_const_signed` is cross-imported by
  `indirect_branch_resolve/table.rs`). **Not executed**: module layout in this
  subsystem is a maintainer design decision (there is an existing agreed
  read-walk/narrow-mutate separation), so the proposed tree above is presented
  for a decision rather than applied unilaterally.
