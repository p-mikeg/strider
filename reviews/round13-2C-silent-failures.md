# Round 13 — 2C: Silent-Failure Audit

Scope: every `unwrap_or` / `unwrap_or_default` / `unwrap_or_else` / `.ok()?` /
`.ok()` / `if let Ok(...)` (with implicit-`Err` discard) / `let Ok/Some(...) else`
/ `Err(_) =>` / `let _ = …` occurrence across `crates/*/src/`, **excluding test
modules**.  Each site classified as INTENTIONAL-FALLBACK (with inline
justification) or SWALLOWED-BUG (would mask a real error).

## 1. Per-crate counts

| Crate          | Sites | Tests excluded | All intentional? |
|----------------|------:|---------------:|:----------------:|
| `cfg`          |   6   |        ✓       |        ✓         |
| `dot`          |   1   |        ✓       |        ✓         |
| `entity-utils` |   1*  |        ✓       |        ✓         |
| `graphwalk`    |   2   |        ✓       |        ✓         |
| `ir`           |  11   |        ✓       |        ✓         |
| `opt`          |  41   |        ✓       |        ✓         |
| `pattern`      |  14   |        ✓       |        ✓         |
| `pcode-lift`   |   0   |       n/a      |       n/a        |
| `reader`       |   8   |        ✓       |        ✓         |
| `strider`      |   5   |        ✓       |        ✓         |
| `strider-py`   |  14   |        ✓       |        ✓         |
| `target`       |   0** |        ✓       |       n/a        |

\* the two `let _ =` in `entity-utils/src/worklist.rs` are inside `#[cfg(test)]`
modules — only the `unwrap_or(lower)` in `set.rs::from_iter` is non-test.
\** all 10 sites in `target/src/call_other_abi.rs` and 7 sites in
`calling_convention/tests.rs` are inside `#[cfg(test)]` modules.

No `pcode-lift` source-level matches.

## 2. SWALLOWED-BUG findings

**None.**  Every reachable non-test site has either:
- an inline justification comment (one of: "Mutex poisoning recovery",
  "lift-time canonicalisation gap", "wide-type bail", "hygiene-only post-pass",
  "doc-example marker", "fallback to default style"), OR
- a `?`-propagated error path that surfaces upstream as a typed
  `anyhow::Error` / `ir::ValidationErrors` / `ReaderError`.

Notable defensive-but-correct sites (worth re-reading in 6 months but
not bugs today):

| Location | Pattern | Why it is NOT a bug |
|----------|---------|---------------------|
| `crates/opt/src/function_args/mod.rs:552` | `let Some(result) = results.pop() else { Err(…) }` | The `len == 1` check above already proves non-empty; the helper still emits a typed `Err` on the "unreachable" arm rather than `unwrap`.  Defensive, with comment. |
| `crates/opt/src/stack_load_forward/mod.rs:326` | `let Some(frame) = work.pop() else { return None; }` | Mirrors the `last_mut`/`pop` pair guard; commented as "no-panic discipline". |
| `crates/opt/src/indirect_branch_resolve/jump_table.rs:480` & `:491` | `let Some(only) = … else { last_result = None; break; }` | Single-pred and first-pred lookups after `len()` checks; comment cites "no-panic discipline". |
| `crates/strider/src/strider/pipeline.rs:326` | `.max().unwrap_or(0)` on iterator of `RegionId.index()` | Empty CFG → `max_index = 0`, then `vec![None; 1]`; the empty case is handled correctly downstream. |

## 3. INTENTIONAL-FALLBACK section

Grouped by category; one-line justification each.

### A. Mutex / RwLock poisoning recovery (`unwrap_or_else(|p| p.into_inner())`)
- `crates/cfg/src/cfg/decode_cache.rs:60,66` — comment cites atomic-write semantics.
- `crates/strider-py/src/reader.rs:143,703` — comment cites atomic Copy-slot writes.
- `crates/strider-py/src/pattern.rs:342,362` — comment cites atomic pointer-slot writes; alternative is leaking a stale `*const Graph`.

### B. Lift-time-canonicalisation peephole partial walks (return `None` to skip)
- `crates/opt/src/sp_expr.rs:215,243` — i64 conversion of sign-extended constant; `None` triggers retry next fixed-point iteration after `ConstantFold` collapses.
- `crates/opt/src/known_bits/mod.rs:88,221,261` — width fallbacks documented to mirror Sleigh `OpBehavior` semantics; pre-fix bug rationale captured inline.
- `crates/opt/src/known_bits/mod.rs:30,174,326,343,498` — `u64_type_mask` returns `None` for `U80`/`U128`/`U256`; KnownBits is 64-bit-bound by design and bails rather than corrupting wider types.
- `crates/opt/src/load_readonly/mod.rs:69,75,85,89` — bail on non-const addr / wide load type / OOB ROM read.
- `crates/cfg/src/cfg/builder/indirect_resolve.rs:298,302,312,315` — same shape inside the mini-graph resolver.
- `crates/opt/src/stack_load_forward/mod.rs:85,225` — non-value-typed outputs bail rather than miscompile.
- `crates/opt/src/stack_store/detect.rs:31` — SP-decomposition fails → `NoChange`.
- `crates/opt/src/dead_branch/mod.rs:55` — cond not a const → `NoChange`.
- `crates/opt/src/function_args/mod.rs:246,256,293,317` — non-value type / offset miss / load-group skip; each is a real classifier-skip case, not a swallow.
- `crates/opt/src/indirect_branch_resolve/stack_array.rs:104,105,322,517` — `i64::try_from` / `u32::try_from` on bounded values where overflow is a separate skip condition.
- `crates/opt/src/indirect_branch_resolve/jump_table.rs:305,451,528,637,661,709` — table-shape classifier bails rather than emitting an unsound resolution.
- `crates/opt/src/indirect_branch_resolve/mod.rs:161` — `node_inputs_exact::<3>` arm is structurally guaranteed; comment documents the only-reachable-arm fact.
- `crates/opt/src/constant_fold/rules.rs:221,246` — capture-binding guards inside `when_match` closures; standard rewrite-rule shape.
- `crates/opt/src/if_cond_inversion/mod.rs:80` — non-2-input `If` (validator catches earlier); bail.
- `crates/ir/src/builder/coerce.rs:81,105` — masked u128 → u64/i64 narrowing; `None` for out-of-range constants is a real classifier result.
- `crates/ir/src/ops/consts.rs:21` — same shape on the read-side.

### C. Lift-time canonicalisation `Err(_) =>` arms (cfg / reader)
- `crates/cfg/src/cfg/builder/region_builder.rs:406` — `u32::try_from(addr_off)` failure → `DidntFinishProcessing`; comment cites "strict-on-emission check will surface any real problem with full context".
- `crates/reader/src/elf.rs:590,823` — `symbol_by_index` / `sec.data()` failures bucketed onto `stats.skipped_malformed_target` / `stats.autoload_section_parse_failures`; comment explicitly contrasts with weak-extern bucket.  This is the **opposite** of silent failure: failures are *counted* on a stats struct so callers can detect malformed ELF.
- `crates/reader/src/elf.rs:520,524,567,571` — same shape, all four feed `RelocationStats` buckets.
- `crates/reader/src/elf.rs:483,690` — `obj.dynamic_relocations()` absent → return empty stats; documented contract.

### D. Hygiene-only post-pass discards (`let _ = detach_unreachable_nodes`)
- `crates/opt/src/function_args/mod.rs:109` — comment cites "detach result is hygiene-only".
- `crates/opt/src/redundant_phis/mod.rs:196` — comment cites "Detaching unreachable zombies is bookkeeping, not progress".

### E. Infallible-write discards
- `crates/dot/src/lib.rs:202` — `write!(out, …)` to a `String`; comment cites `String::write_str` always returns `Ok(())`.
- `crates/strider/src/strider/insn/mod.rs:141` — `let _ = build_call_other_terminal(…)?;` — the `?` propagates errors; the `let _` only discards the successful `NodeId` because the caller doesn't need it.  (NB: the `?` makes this a propagated-Result, not a swallow.)
- `crates/graphwalk/src/lib.rs:48,79` — `let _ = try_successors/predecessors` inside the non-`try_` convenience wrappers; the `ControlFlow` return is discarded because the wrapper doesn't expose short-circuit.

### F. Variadic-tail / default-fill on infallible iterators
- `crates/entity-utils/src/set.rs:126` — `upper.unwrap_or(lower)` on `size_hint`; standard `FromIterator` capacity-hint shape.
- `crates/ir/src/dot/label.rs:279,304,322,338,353,356` — render fallbacks (synthetic test graphs without `call_clobbered`, missing user-op names); commented as "fall back to a synthetic clobN/outN label" so rendering doesn't panic on test graphs.
- `crates/ir/src/dot/render.rs:211` — `owned_label.as_deref().unwrap_or(label)` — fallback to a static label string in the renderer.
- `crates/strider-py/src/cfg.rs:64,75`, `graph.rs:95,122`, `dot.rs:9` — `style.unwrap_or("dark")` / `unwrap_or("dark_cfg")` for Python keyword-arg defaults.
- `crates/strider-py/src/run.rs:70` — `per_address_ccs.unwrap_or_default()` for an absent optional kwarg.
- `crates/strider/src/orchestrator.rs:788` — `override_cc.unwrap_or_else(|| strider.calling_convention())` — explicit "no override supplied, use function default" path; comment documents.
- `crates/strider/src/strider/pipeline.rs:318` — `opts.all_vns.unwrap_or_else(|| self.find_all_unique_vns(cfg))` — same shape, optional vn-set.

### G. Validator local skips (after structural checks elsewhere)
- `crates/ir/src/validate/layer_a.rs:69,86` — past-head arity in fixed-arity sig; a count mismatch was already reported by Layer A's arity check.
- `crates/ir/src/validate/layer_b.rs:73` — `node_input_id_at(idx)` after `0..input_count`; documented as "internal-consistency bug rather than user input. Skip the offending index".
- `crates/ir/src/validate/layer_c.rs:293` — `outputs.next()` after structural check; Layer-A sig mismatch already reported on the missing output.

### H. Pattern-matcher walk-helper guards (forward / output-index walks)
- `crates/pattern/src/matcher/mod.rs:323,660` — discriminant-bucket miss / single-input cast unwrap.
- `crates/pattern/src/pat/guards.rs:65` — non-value-output target → predicate guard fails closed.
- `crates/pattern/src/pat/node_pat.rs:453,464,478,547` — out-of-range input/output indices fail the match (correctness-preserving).
- `crates/pattern/src/pat/builders/branch.rs:105,140,143`, `memory.rs:89,92,206,209`, `walk_helpers.rs:29,32` — builder-internal walk guards; same shape.

### I. Polymorphic-extract probes (`if let Ok(…)`)
- `crates/strider-py/src/reader.rs:425,728,763`, `pattern.rs:388,391` — Python `extract::<T>()` probe-with-fallback; the standard PyO3 pattern for discriminated unions.

### J. Doc-comment markers
- `crates/cfg/src/cfg/options.rs:164`, `crates/dot/src/lib.rs` (let _ in writer), `crates/opt/src/worklist.rs:81`, `crates/pattern/src/lib.rs:134`, `crates/pattern/src/pat/ctor/wildcards.rs:91` — `# let _ = …;` inside doctests / module-level docs.
- `crates/pattern/src/macros.rs:177,181` — `let _ = &$hy;` macro-internal hygiene marker.

### K. Test-only assertion shape (NOT in this audit's scope but worth noting)
All 10 hits in `target/src/call_other_abi.rs` use `unwrap_or_else(|| panic!(…))` inside `#[cfg(test)]` blocks — `expect`-equivalent; not production code.

## 4. Conclusion

✓ **All silent-failure sites classified intentional, no action needed.**

Strider's silent-failure discipline is consistent across the workspace:
- Mutex/RwLock poisoning uniformly recovers via `into_inner()` with inline
  justification comments referencing the atomic-write invariant.
- Optimizer passes return `OptimizationResult::NoChange` (or `None` from
  classifier helpers) on classifier-skip conditions, never `unwrap()`.
- The ELF relocation path uses *counted* `RelocationStats` buckets in place
  of silent `Err(_) =>` swallows — programmatic callers can detect
  malformed-ELF without scraping stderr.
- The KnownBits and SP-decomposition layers have explicit width-bail
  paths (`u64_type_mask` returning `None`) where the previous "collapse
  to 0" behaviour silently corrupted wide-type analysis (round-fix
  rationale captured inline at `known_bits/mod.rs:80-93,320-328`).
- Pattern matcher and validator local skips are bounded by structural
  guarantees that earlier layers / arity checks enforce; commented as
  "no-panic discipline" on the rare defensive guards.

No HIGH/MED swallow findings.  No edits made.
