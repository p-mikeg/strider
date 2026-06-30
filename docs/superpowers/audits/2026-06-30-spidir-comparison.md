# strider-ir / strider-graph vs spidir `ValGraph` — comparative function audit

**Date:** 2026-06-30
**Branch:** `audit/2026-06-30-spidir-comparison`
**Reference:** spidir (`../../Downloads/spidir-main/crates/ir`), a hand-written,
AI-free, expert-built sea-of-nodes compiler IR — trusted as a correctness
baseline.

## Goal

Question every function/flow in `strider-graph` and `strider-ir` against
spidir's equivalent: is it really needed, or can we live without it **without
hurting big-O runtime or correctness**? Delete anything whose keep-reason isn't
good enough.

## Method

Five parallel comparison passes (one per area), each reading both codebases,
followed by call-site verification of every flagged candidate. spidir shares
crate DNA with us (`entity-utils`, `graphwalk`, `graphmock`), so it is a fair
apples-to-apples yardstick for the *generic* graph machinery.

## Headline

**Almost nothing is deletable, and the reasons hold up under scrutiny.** Of
~290 functions across the two crates, the agents proposed ~30
delete/simplify candidates; **on call-site verification, essentially all of
them turned out to be used or load-bearing.** The divergences from spidir are
not fat — they are two deliberate, justified differences:

1. **Domain.** strider is a *binary-analysis* tool; spidir is a *compiler
   backend*. ~70 `Function` methods (calling conventions, register aliasing,
   asm fingerprints, stack offsets, arg-index, tracked varnodes, wide integer
   types) model machine code. spidir legitimately has no analog. Not
   deletable.
2. **Model.** strider's `Graph` is *mutational* (edges added/removed/rewired
   in place during an interleaved destructive+nondestructive fixed-point
   optimizer); spidir's is *immutable-after-build* (all construction via an
   external `CachingBuilder`, never mutated). strider's dedup-cache
   invalidation, dense-arena compaction, and `EditFunction` live-set
   bookkeeping exist to make that model O(edits) instead of O(n·edits). Not
   deletable without a quadratic regression.

The comparison is therefore best read as **validation** of strider's design,
plus a short list of genuine micro-simplifications and a few spidir *ideas*
worth borrowing.

---

## Candidates the agents flagged — and why each survives

| Candidate (agent verdict) | Reality on verification | Verdict |
|---|---|---|
| `value_kind_ref` → "redundant with `value_kind`" (DELETE) | `value_kind` returns `V` by value (needs Copy/Clone); `value_kind_ref` returns `&V`. strider-pattern's payload `V` is **not** cheaply Copy — 6 pattern call sites need the by-ref form. | **KEEP** |
| `node_id_from_u32` → "unsafe given generation counter" (DELETE) | It's the **validating** `u32 → Option<NodeId>` conversion at the Python boundary (returns `None` for stale ids). 4 py call sites depend on it. | **KEEP** |
| `MissingEntryNode` validator variant → "unreachable, entry non-optional" (DELETE) | The match enforces **exactly one** `Entry` node (it also catches the duplicate-entry case). A pass that rewrote the entry node's kind is exactly the corruption the validator exists to catch. | **KEEP** |
| `producer_inputs_exact` → "0 callers, dead" (DELETE) | False negative from a grep that missed the turbofish form `producer_inputs_exact::<2>(…)`. **8** real callers in flag-cmp + value-range. | **KEEP** |
| `petgraph_view` + `Vertex` → "dead / feature-gate" (DELETE/MOVE) | strider-pattern's `staging` + `matcher` run `petgraph::algo::toposort` and `DfsPostOrder`/`Reversed` over the pattern graph (producer-before-consumer staging) **through these impls**; the proptest suite exercises them too. | **KEEP** |
| `asm_fingerprint_exempt` → "strider-specific" (DELETE) | Used by the always-on validator's fingerprint check to exempt Entry/Region/Phi/initial-state kinds. Removing it breaks validation. | **KEEP (domain-specific)** |
| `compact` / `retain_reachable` / `gc_consts` → "no spidir analog" (DELETE) | spidir never recycles ids, so it never compacts. strider keys its optimizer on dense `DenseEntitySet`/`SecondaryMap` arenas; compaction keeps iteration dense. Load-bearing. | **KEEP** |
| ~17 graph "convenience helpers" → "inline at call sites" (SIMPLIFY) | Usage counts: `producer` 355×, `all_node_ids` 149×, `kind_of_value` 68×, `nth_input` 26×. Inlining a 50–355× helper is *more* code, not less, and buys nothing. | **KEEP** |
| `EditFunction` live/roots/queue/flags → "spidir just re-walks" (SIMPLIFY) | strider's optimizer interleaves destructive + nondestructive rules in one fixed-point loop and queries `is_live` / `live_of_kind` repeatedly mid-pass. A re-walk per query is O(n); the cached set makes it O(1). Removing it reintroduces the quadratic. | **KEEP** |
| const interner (`intern_int_const*`, `const_value`) → "spidir inlines u64" (SIMPLIFY) | Two real reasons: (a) wide constants (I80/I128/I256/I512) can't sit inline without bloating `NodeKind` past its hard-won 16 bytes; (b) the interner stores them out-of-line behind a `u32` `ConstId`. spidir only has u64 constants, so it sidesteps both. | **KEEP** |
| `node_signature` table → "spidir hand-writes per-kind verifiers" (KEEP, noted) | strider's declarative slot table is *queried* by the validator AND carries slot roles for rendering; adding a node kind is one match arm vs spidir's ~742-line per-kind verify file. Strictly better at our scale. | **KEEP** |

**Net deletions from the spidir comparison: 0.** The reasons are good enough.

---

## Genuine simplifications available (small, optional)

These are real but minor; none are spidir-driven deletions, and each needs the
full test gate:

1. **Validator `check_graph_invariants_region` control-kind check** — agent 5
   argues the per-predecessor "is control" check is subsumed by
   `check_local_typing` (which already types every Region input edge). If
   confirmed non-redundant-free, drop the duplicate check (keep the
   non-empty-region check). *Recommendation: verify overlap, then trim.*
2. **`node_cache::avoid_sentinel`** — spidir's `expand_hash` (golden-ratio
   multiply) is a one-liner achieving the same `u64::MAX` sentinel avoidance.
   Cosmetic; no behavior change. *Recommendation: low priority.*
3. **1–2-use micro-helpers** (`next_use` 1×, `next_node_id` 1×, `has_node` 2×)
   on `Graph` — inlinable, but each is a one-liner already and the project's
   own prior audits kept similarly-thin accessors. *Recommendation: leave.*

## spidir ideas worth *adding* (not pruning)

The comparison surfaced two checks spidir has that strider lacks — additive,
not deletions, offered for your call:

1. **Control-output single-use invariant.** spidir verifies every control
   output is used exactly once (`UnusedControl` / `ReusedControl`). strider
   doesn't. ~20 lines; catches malformed control wiring from surgical edits.
2. **Data-flow domination.** spidir verifies every use is dominated by its
   definition via the dominator tree. strider has `control_dominators` already,
   so the machinery exists; this would be a real strengthening of the
   validator. Larger effort.

---

## Implementation outcomes (follow-up)

The user requested items 1, 3, and 4. Outcomes:

- **#1 — Region control-kind check (DONE).** Removed `RegionNonControlPredecessor`
  (the check and the error variant): a non-Control Region predecessor is already
  flagged by `check_local_typing` against the Region signature's variadic `CTRL`
  tail, reported as `NodeInputKindMismatch`. The `EmptyRegionPredecessors` check
  stays — local typing's variadic-from-zero arity cannot express "≥ 1
  predecessor". Verified by re-pointing the existing test at the surviving error
  before deleting.

- **#3 — control-output single-use (DONE, full invariant).** Added
  `check_graph_invariants_control_single_use` porting spidir's
  `verify_control_outputs`: both `ReusedControlOutput` (fan-out — caught 2
  genuinely malformed fixtures, now fixed) and `UnusedControlOutput` (dangling
  control) are enforced. Enforcing the unused half surfaced that real MIPS32
  div/mod IR fails it — the compiler's div-by-zero `break` guard lifts to a
  NoReturn-trap `CallOther` whose control **deliberately dangled** ("control
  ends here"), and strider had no control-sink node. Fixed at the root by adding
  **`NodeKind::Unreachable`** (a `[CTRL] → []` terminator mirroring spidir's
  `Unreachable`); the lifter now sinks every NoReturn trap's control into it, so
  every control edge reaches a terminator. ~6 minimal synthetic fixtures gained
  an explicit terminator. Verified: full workspace `--no-fail-fast` 0 failures +
  pytest 870 (MIPS32 div/mod now green) + clippy clean.

- **#4 — data-flow domination (BLOCKED on a scheduler).** spidir's check pins
  every floating sea-of-nodes data node to a dominator-tree location via a
  *schedule*, then checks dominance (phi operands against their predecessor
  block, not the phi's region). strider is **unscheduled** — pure data nodes
  float and `control_dominators` only covers the control subgraph — so there is
  no location to check a floating value's "definition point" against. A faithful
  port needs a scheduler (a substantial new component); a restricted control-only
  variant would largely duplicate the existing phi-token / phi-arity / region
  checks. Recommend either building a scheduler as its own project or skipping.

## Conclusion

strider-ir and strider-graph hold up against a hand-written expert reference:
the extra surface over spidir is explained, in every case checked, by binary
analysis (domain) or in-place mutation (model). The prior dedup/dead-code
audits already removed the fat. **No function meets the "reason isn't good
enough" bar for deletion.** The actionable output is the two optional
micro-trims and the two spidir-inspired validator additions above — all of
which are your call.
