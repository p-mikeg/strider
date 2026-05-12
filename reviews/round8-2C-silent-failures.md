# Round 8 / 2C — Silent-failure hunt

**Branch:** `review/ai2`
**Trust model:** strict (re-derived from current code; no consultation of round-7 findings)
**Files audited:** `crates/*/src/**/*.rs` (production paths only; `#[cfg(test)]` modules excluded)

Rough headcount of `unwrap_or* / .ok() / map_err(|_| _) / Err(_) =>` style sites: ~115. Most are documented or arithmetically impossible (ELF reloc width casts after explicit width checks, iterator size-hint capacity defaults, pretty-print fallbacks). The findings below focus on places where the construct is actually load-bearing — i.e. it gates on a real failure mode, and the fall-through changes user-observable behaviour.

Severity legend:
- **HIGH** — a real bug or unsoundness can hide behind the silent path.
- **MED**  — analysis quality silently degrades; a user gets a worse result with no diagnostic.
- **LOW**  — cosmetic / unreachable-in-practice / well-documented invariant fallback.

---

## HIGH-severity findings

### H1. `flag_cmp_canonicalize/mod.rs:129` — `unwrap_or(a)` masks a missing capture

`crates/opt/src/flag_cmp_canonicalize/mod.rs:126-130`:
```rust
let a = m
    .output(rule.lhs_capture)
    .expect("Capture a must bind to a value output");
let b = m.output(rule.rhs_capture).unwrap_or(a);
(a, b)
```

Three lines later, `(rule.build_rhs)(graph, a_out, b_out, node)` builds a replacement subtree using `a_out` and `b_out`. The `.expect(...)` for `lhs_capture` is correctly load-bearing, but the symmetric `.unwrap_or(a)` for `rhs_capture` is *not* the same kind of fallback — it silently aliases `b_out := a_out` when the rhs capture failed to bind. If a future flag-cmp rule omits a `rhs_capture` setup or a binding bug occurs, every `IntCmp(a, a)` shape produced will fold to a tautology (`a == a → true`, `a < a → false`) and the user gets a wrong-but-plausible canonicalisation.

**Hidden errors:** any rule whose `lhs_capture == rhs_capture` would not need this fallback (so we can detect that "expected"-equal case), but any rule where they differ and we silently re-bind to `a` produces a corrupt canonical form. The matcher contract guarantees both captures bind on success, so this fallback only triggers on a bug.

**Recommendation:** mirror the `lhs_capture` arm — `.expect("rhs_capture must bind")` (or propagate `Err` via `?` with a typed error). The lhs path already documents *why* `.expect` is correct; the rhs path silently tolerates the same invariant violation. There is no defensible reason for the two captures to be treated differently.

---

### H2. `strider-py/src/reader.rs:656` — `.ok()?` swallows poisoned-mutex during `ReadOnlyMemory::read`

`crates/strider-py/src/reader.rs:656`:
```rust
let table = self.lookup_table().ok()?;
```

`PyMemoryMap::lookup_table()` (`crates/strider-py/src/reader.rs:134`) returns an `anyhow::Result` whose only failure is a poisoned `RwLock` ("MemoryMap table lock poisoned"). On the rest of the file every other access surfaces poisoning via `into_reader_err(...)`. This *one* call site discards it and returns `None`, which `LoadReadOnly` then interprets as "rodata not mapped — leave the Load alone".

**Hidden errors:** lock poisoning is a process-level invariant break (some other thread panicked while holding the lock). Treating it as "address not mapped" hides the corruption and lets `LoadReadOnly` produce inconsistent results across calls (sometimes the rodata Load folds, sometimes it doesn't, depending on which mutex was poisoned).

**User impact:** silently degraded analysis with zero diagnostic; the user sees flaky pattern-match results.

**Recommendation:** propagate via `eprintln!` + `None` (matching the pattern crate's `wrap_when` and the rom adapter's `read` at lines 568-589 in the same file), or escalate to a hard `panic` since poisoning indicates an already-broken invariant.

Same flavour at `crates/strider-py/src/reader.rs:666`:
```rust
let endianness = *self.endianness.read().ok()?;
```
Returns `None` (= "not mapped") on a poisoned `endianness` RwLock. Same fix.

---

### H3. `strider-py/src/pattern.rs:292` — `.ok()?` swallows graph-pointer mutex poison

`crates/strider-py/src/pattern.rs:289-298`:
```rust
fn with_graph<R>(&self, f: impl FnOnce(&ir::Graph) -> R) -> Option<R> {
    let guard = self.graph_ptr.lock().ok()?;
    let ptr = (*guard)?;
    // SAFETY: ...
    let graph_ref = unsafe { &*ptr };
    Some(f(graph_ref))
}
```

This is the predicate-callback path used by every `.when(...)` call from Python. If the inner mutex is poisoned (a previous predicate callback panicked while holding it), every subsequent `.when()` invocation silently treats *all* matches as no-match — `find_all` returns empty results, no diagnostic, no exception.

The sister `clear_graph_ptr()` at line 282 deliberately *does* recover from poisoning via `unwrap_or_else(|poisoned| poisoned.into_inner())` because (per its comment) leaving stale pointers is unsafe. The `with_graph` path silently bypasses that same recovery and instead returns `None`.

**Hidden errors:** cross-call effects of a panicking predicate; the user has no way to see that subsequent `.when()` calls are dead.

**Recommendation:** either share `clear_graph_ptr`'s `unwrap_or_else(|p| p.into_inner())` recovery, or `eprintln!` the poison error before returning `None`. Silent inert behaviour after a predicate panic is the worst of both worlds.

---

### H4. `opt/src/indirect_branch_resolve/inplace.rs:126-129` — `unwrap_or(U64)` masks a real type error in tail-call splicing

`crates/opt/src/indirect_branch_resolve/inplace.rs:126-129`:
```rust
let target_int_ty = graph
    .output_kind(target_value)
    .as_integer_or_err()
    .unwrap_or(ir::node::NodeOutputType::U64);
```

`apply_tail_call` is splicing a fresh `Call → Return` chain onto the placeholder. `target_value` is the third input of an `IndirectBranch` placeholder pinned at lift time and *must* be an integer-typed value (it's the runtime branch target). If `as_integer_or_err()` fails, the placeholder shape was wrong — but here we silently default to U64 and continue building. On a 32-bit ARM target, this could silently truncate-or-extend a non-integer producer's value into a synthesised U64 IntConst, then call `replace_all_uses` against an output of the wrong width and corrupt the IR.

**Hidden errors:** any classifier-misclassification that lets a non-integer producer reach `apply_tail_call`; any future ARM/MIPS Thumb-bit rewrite that changes the target's type.

**Recommendation:** propagate the error — `.as_integer_or_err()?` (the function already returns `Result`). The "default to U64" fallback is unjustified; if the IR is malformed we want to surface it, not paper over it.

---

### H5. `opt/src/indirect_branch_resolve/mod.rs:281` — `anchor_contexts.get(addr).unwrap_or(&empty_ctx)`

`crates/opt/src/indirect_branch_resolve/mod.rs:281`:
```rust
let ctx = self.anchor_contexts.get(addr).unwrap_or(&empty_ctx);
```

Twenty lines later this `ctx` flows into `apply_link_register` (with `&ctx.ret_val_outputs`) and `apply_tail_call` (with `&ctx.arg_passing_outputs`, `&ctx.clobbered_kinds`, `&ctx.ret_val_outputs`). If a placeholder anchor's context wasn't built (a bug in the orchestrator's `build_anchor_calling_context` plumbing or a stale `unresolved_anchors` entry), we silently splice in a `Return` with zero return values and a `Call` with zero args/clobbers — the function suddenly behaves as if its calling convention had no live registers across the call.

**Hidden errors:** any orchestrator-plumbing bug where `anchor_contexts` is populated for a different `unresolved_anchors` set than what the pass iterates. Pattern queries downstream silently see the wrong set of `FunctionArg` / `Return` operands.

**User impact:** no error, no warning; just wrong-but-plausible IR for one anchor.

**Recommendation:** treat a missing entry as a bug — `anchor_contexts.get(addr).ok_or_else(|| anyhow!("no AnchorCallingContext for {addr:?}"))?`. The `empty_ctx` fallback was almost certainly meant to cover the test path that constructs the pass directly; production callers always populate every anchor.

---

## MED-severity findings

### M1. `opt/src/indirect_branch_resolve/classify.rs:54-64` and 88-94 — `eprintln` on `analyze_known_bits` failure

`crates/opt/src/indirect_branch_resolve/classify.rs:54-64`:
```rust
let known = match crate::analyze_known_bits(fg) {
    Ok(k) => k,
    Err(e) => {
        eprintln!("strider: classify_anchor: analyze_known_bits failed: {e:?}");
        return None;
    }
};
```

`analyze_known_bits` only returns `Err` on a `Kb::merge` contradiction (per the comment) — i.e. provably-incompatible constants reach the same output, which is a real IR-level bug. Returning `None` makes the indirect-branch resolver "fail open": the orchestrator interprets this as "this anchor doesn't classify, try again next iteration" — and at fixed point it surfaces the original `UnresolvedIndirectBranch`, not the underlying `Kb::merge` contradiction.

The eprintln is better than silent, but a known-bits contradiction warrants a propagated `Err` (especially since `IndirectBranchResolve::optimize` *does* return `Result<OptimizationResult>` already at line 261, which is where the cached known-bits computation lives — only the *unconditional* `classify_anchor` / `classify_anchor_with_rom` entry points lose this).

Same exact pattern at `crates/strider/src/indirect_resolve/classify.rs:49-56` (the `strider` re-export) — that wrapper has no excuse to swallow the error since strider's orchestrator paths threading through `optimize_built` already pass `&known` and would propagate the underlying `Err`.

**Recommendation:** convert these one-shot helpers to `Result<Option<ResolvedTargets>>` and `?`-propagate. The current `Option`-only return type is the root cause.

---

### M2. `cfg/src/cfg/decode_cache.rs:60, 66` — `unwrap_or_else(|e| e.into_inner())` swallows poison without comment about *what* was lost

`crates/cfg/src/cfg/decode_cache.rs:60`:
```rust
let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
guard.get(&addr).cloned()
```

The doc-comment at lines 53-57 *does* justify this: "Mutex poisoning recovers via `into_inner` — a previous panic while holding the lock leaves the map intact, and a stale entry can't miscompile." That reasoning is correct *for the cache contents*, but it does nothing to surface the original panic that caused the poison. If a panic happened during `cache.insert(...)` from one thread, every subsequent `cache.get(...)` from another thread silently absorbs the poison and the user has no signal that an earlier panic occurred.

This is "INTENTIONAL FALLBACK" by design (single-Sleigh-context-scoped read-only cache; correctness isn't compromised), but the error-handling contract should at minimum log the poison once via stderr to flag the originating panic. As-is, a panic anywhere in the orchestrator that holds this lock is fully invisible.

**Recommendation:** wrap a `static AtomicBool` or use `eprintln!("strider: DecodeCache mutex was poisoned by an earlier panic; results may be partial")` on the first poison observed. Mark severity as MED rather than HIGH because cache contents are immutable.

---

### M3. `strider-py/src/dot.rs:16-22` — silent fallback to `dark` on unknown style names

`crates/strider-py/src/dot.rs:15-22`:
```rust
pub fn dot_style_for(name: Option<&str>) -> dot::DotStyle {
    match name.unwrap_or("dark") {
        "dark_cfg" => dot::DotStyle::dark_cfg(),
        "empty" => dot::DotStyle::empty(),
        // "dark" and any unknown name fall through to dark.
        _ => dot::DotStyle::dark(),
    }
}
```

Comment says "any unknown name fall through to dark" — explicit but user-hostile. A Python user passing `style="lite"` (typo for `light`, which doesn't exist; or a future style not yet implemented) gets a dark-themed output with zero indication their argument was wrong.

This is degraded analysis (rendered output) without warning. A pyo3 `PyValueError` on unknown name would be the correct behaviour.

**Recommendation:** return `Err(pyo3::exceptions::PyValueError::new_err(format!("unknown dot style {name:?}; valid: dark, dark_cfg, empty")))` when `name` is `Some(unrecognised)`. Keep `None → dark` as the default.

---

### M4. `cfg/src/cfg/builder/region_builder.rs:397-407` — `CallOther` user-op shape errors swallowed as `DidntFinishProcessing`

`crates/cfg/src/cfg/builder/region_builder.rs:397-407`:
```rust
let id_vn = match insn.inputs.first() {
    Some(v) => v,
    None => return Ok(ProcessInsnRes::DidntFinishProcessing),
};
if id_vn.addr_space != rsleigh::VnSpace::CONST {
    return Ok(ProcessInsnRes::DidntFinishProcessing);
}
let id_u32 = match u32::try_from(id_vn.addr_off) {
    Ok(v) => v,
    Err(_) => return Ok(ProcessInsnRes::DidntFinishProcessing),
};
```

The comment at lines 392-396 says "Fall through to today's behaviour... IR layer's strict-on-emission check will surface any real problem with full context." That defence requires an audit: does the IR layer actually re-check the input shape with full context? At `crates/strider/src/strider/insn/mod.rs:120-129`, the IR-layer check is:
```rust
let id_vn = pcode_lift::first_input_or_err(insn)?;
if id_vn.addr_space != rsleigh::VnSpace::CONST {
    bail!("opcode {:?} expects a CONST input at position 0", insn.opcode);
}
let user_op_id = id_vn.addr_off;
let user_op_id_u32 = u32::try_from(user_op_id)
    .map_err(|_| anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32"))?;
```

OK — the IR layer *does* re-check. But the cfg layer's three early-returns short-circuit only the `NoReturn` detection, not the rest of the path: a malformed `CallOther` flows through the cfg as a regular instruction and is only surfaced when strider's `IrStrider::handle_call_other` retries the same shape check. That works in practice but creates a subtle asymmetry: any new cfg-side feature that depends on the user-op classification (e.g. a future "tail-call CallOther" or "stack-allocating CallOther") will silently miss it on malformed inputs.

This is documented and currently sound — MED-rated because a future feature could regress it.

**Recommendation:** in the long term, hoist the shape validation into `pcode_lift` and have both cfg + ir consume the validated form. Short term, no change required, but add an assertion comment cross-referencing the strider-side check.

---

### M5. `reader/src/elf.rs:556-565` — dynamic symbol resolution `.ok()` silently buckets failures into `skipped_unresolved_target`

`crates/reader/src/elf.rs:554-569`:
```rust
let target_addr = match reloc.target() {
    RelocationTarget::Symbol(idx) => {
        let resolved = if let Some(dynsym) = obj.dynamic_symbol_table() {
            dynsym
                .symbol_by_index(idx)
                .map(|s| (s.address(), s.is_undefined()))
                .ok()
        } else {
            obj.symbol_by_index(idx)
                .map(|s| (s.address(), s.is_undefined()))
                .ok()
        };
        let Some((addr, undef)) = resolved else {
            stats.skipped_unresolved_target += 1;
            continue;
        };
```

The `.ok()` pair discards the `object` crate's specific symbol-lookup error (e.g. "index out of range", "section header malformed") and just bumps a counter. The user sees `skipped_unresolved_target = N` with no detail about *why* — was it a bad symbol index (corrupt ELF), an unsupported encoding, or a legitimate undef symbol?

`stats.unsupported_r_types` tracks the analogous case for unsupported relocation type codes, but there's no equivalent for "symbol_by_index failed". A malformed ELF that intermittently fails resolution looks identical to a binary with many undef external symbols.

**Recommendation:** add `stats.symbol_lookup_failed_count` (or a small bucket of the first-N error strings), so callers can distinguish "100 undef externals" from "1 corrupt symtab entry". Logging at warn-level on first failure would also work.

---

### M6. `reader/src/elf.rs:768-771` — `eprintln` on section data parse failure during autoload

`crates/reader/src/elf.rs:761-773`:
```rust
match sec.data() {
    Ok(d) => {
        if d.is_empty() {
            return false;
        }
    }
    Err(e) => {
        eprintln!(
            "strider: ELF section {:?} data parse failed: {e}; skipping for site-coverage",
            sec.name().unwrap_or("?"),
        );
        return false;
    }
}
```

The `eprintln` is the right idea, but `find_loadable_section_containing` is called inside a `.find(...)` predicate that iterates sections — so a single corrupt section in the middle of the table emits a one-shot stderr line then *continues searching*, eventually returning `None` (autoload finds no section) without any final message tying the parse failure to the eventual "site has no region" outcome.

This is partial visibility — MED. The user sees one line about a parse failure but no link to "autoload skipped reloc at site X because no section covered it".

**Recommendation:** thread the diagnostic into `RelocationStats` (e.g. `stats.section_parse_failures: Vec<(SectionName, ErrString)>`) so the final `apply_elf_relocations_autoload` summary reflects the corruption. Currently the parse failure is only on stderr.

---

### M7. `strider-py/src/run.rs:70` — `unwrap_or_default()` for optional per-address-CCs

`crates/strider-py/src/run.rs:70`:
```rust
let per_address_ccs = per_address_ccs.unwrap_or_default();
```

Argument is `Option<HashMap<u64, PyCallingConvention>>`. `None` → empty map → no per-address CC overrides. Documented in the function signature; INTENTIONAL FALLBACK.

**Severity:** LOW — listed for completeness only, no action needed.

---

### M8. `strider/orchestrator.rs:778, 784` — `try_into().ok()?` and `node_outputs_exact::<1>(nid).ok()?` after `create_node`

`crates/strider/src/orchestrator.rs:778-786`:
```rust
let ty: ir::node::NodeOutputType = vn.size.try_into().ok()?;
let nid = graph.graph.create_node(
    ir::node::NodeKind::InitialVar(vn),
    [],
    [ir::node::NodeOutputKind::OutputType(ty)],
);
let [out] = graph.graph.node_outputs_exact::<1>(nid).ok()?;
```

Two failure paths here both silently return `None`:
1. `vn.size.try_into()` — a `vn` with a size unsupported by `NodeOutputType` (e.g. zero, or 3, or 5+ outside `{1,2,4,8,10,16}`). Documented in the doc-comment ("Returns None when the varnode's byte size has no matching NodeOutputType"). LOW.
2. `node_outputs_exact::<1>(nid).ok()?` — but we *just created* `nid` with one output (the `[OutputType(ty)]` slice). Returning `None` here means our IR-construction invariants are broken. This should be `.expect(...)` or `?` with a typed error.

**Severity:** the second one is MED — silently falls through on an IR-arena bug (an invariant the next `unwrap` in the call site would expose loudly). Convert to `.expect("InitialVar must have exactly one output")`.

---

### M9. `cfg/src/cfg/builder/region_builder.rs:165` — `unwrap_or(u64::MAX)` for `pcode_count`

`crates/cfg/src/cfg/builder/region_builder.rs:165`:
```rust
let pcode_count = u64::try_from(lift_res.insns.len()).unwrap_or(u64::MAX);
```

`lift_res.insns.len()` is a `usize`. On a 64-bit target `usize → u64` is infallible (`try_from` always returns `Ok`). On a hypothetical 128-bit target this branch matters; on 32-bit it's also infallible. The fallback is unreachable on every architecture this project supports.

**Severity:** LOW — unreachable.

---

## LOW-severity / well-justified

### L1. `ir/src/wide_const.rs:78, 92` — `try_into().ok()?` after explicit length check

```rust
if bytes.len() != 32 { return None; }
let mut limbs = [0u64; 4];
for (i, limb) in limbs.iter_mut().enumerate() {
    let chunk: [u8; 8] = bytes[i * 8..(i + 1) * 8].try_into().ok()?;
```

The `try_into::<[u8;8]>()` after the explicit `len() == 32` guard is unreachable. INTENTIONAL FALLBACK (defensive). LOW.

### L2. `opt/src/known_bits/mod.rs:137-289` — `known.get(&_).copied().unwrap_or_default()`

Multiple sites (137, 138, 172, 213, 249, 259, 272, 289). All documented at line 110-126: "Outputs absent from the map have no proven bit info; treat them as the all-unknown default." Sound widening, not silent failure. INTENTIONAL FALLBACK. LOW.

### L3. `opt/src/indirect_branch_resolve/jump_table.rs:309, 313, 426, 640, 664` — small-domain `try_from` casts

Each is bounded by an explicit prior check (`s_u128 >= 64`, `ty.is_integer()`, etc.). The `.ok()?` triggers only on a logic bug elsewhere; the local fall-through is sound. INTENTIONAL FALLBACK. LOW.

### L4. `opt/src/sp_expr.rs:206, 234` — `i64::try_from(signed_i128).ok()`

Decomposes SP-relative offsets. An `i128 → i64` failure means an offset >= 8 EiB or <= -8 EiB, which is structurally impossible for a single function's stack frame. Silent `None` is the right answer. LOW.

### L5. `ir/src/dot/label.rs:279, 304, 322` and `ir/src/dot/render.rs:211` — pretty-print fallbacks

Visualisation code; falling back to `Debug` formatting is the right answer when the pretty-name lookup misses (synthetic graphs from tests, third-party builders that bypass side-tables). LOW.

### L6. `target/src/call_other_abi.rs:387, 406, 483, 532, 546, 592, 600, 720, 762` — `unwrap_or_else(|| panic!(...))` in tests

All inside `#[cfg(test)]` per the file structure (test-only assertions for the static name table). LOW.

### L7. `reader/src/lib.rs:188` — `usize::try_from(u64).ok()?`

`usize::try_from(addr.checked_sub(self.start_addr)?).ok()?` — on a 64-bit target this is infallible. On 32-bit the cast can fail when an address > 4 GiB is requested; returning `None` (= "address not in this region") is the right answer. INTENTIONAL FALLBACK. LOW.

### L8. `opt/src/function_args/mod.rs:523` — `results.pop().unwrap_or(true)`

```rust
debug_assert_eq!(results.len(), 1, "mem_chain_is_dirty: result-stack invariant");
let result = results.pop().unwrap_or(true);
```

The `debug_assert` says the invariant; the `unwrap_or(true)` is the conservative-on-bug fallback (treat as "dirty memory"). Sound widening. LOW.

### L9. `strider/src/strider/pipeline.rs:321, 329` — `unwrap_or_else(|| self.find_all_unique_vns(cfg))` and `.max().unwrap_or(0)`

Optional caller-supplied `all_vns` falls back to the default computation; `.max().unwrap_or(0)` on an empty iterator returns 0 (the right "no regions" default for a `vec![None; max+1]`). INTENTIONAL FALLBACK. LOW.

### L10. `strider/src/orchestrator.rs:695` — `.unwrap_or_else(|| strider.calling_convention())`

Per-address CC override falls back to the function-default CC. Documented just above. INTENTIONAL FALLBACK. LOW.

### L11. `strider-py/src/cfg.rs:57, 68` and `strider-py/src/graph.rs:75, 100` — style defaults

Same family as M3 (the `dot.rs` central helper). The wrapper helpers `to_html` / `html_str` default to `"dark_cfg"` / `"dark"` when the Python caller passes `None`. The `None` case is a legitimate default; only the unknown-name case (M3) silently falls through. The wrappers themselves are LOW.

### L12. `strider-py/src/{graph,reader,opt,pattern}.rs` — `.map_err(|_| anyhow!(...))` for poisoned-mutex paths

Numerous sites. Each loses the `PoisonError`'s inner panic data (which `PoisonError::into_inner` would recover). The replacement `anyhow!("X lock poisoned")` carries enough context to debug. LOW.

### L13. `pattern/src/rewrite.rs:55` — `None => return Ok(false)`

A no-match in `rewrite_rule` is correctly "no change" (`Ok(false)`). Documented. INTENTIONAL FALLBACK. LOW.

### L14. `opt/src/stack_store/call_args.rs:205` — `None => return dense_prefix(slots)`

Documented at lines 198-206: out-of-set offsets terminate the walk; the prefix walker returns what it found. INTENTIONAL FALLBACK. LOW.

### L15. `reader/src/elf.rs:849` — `_ => return None` in `image_relative_reloc`

Catch-all for unrecognised arch + r_type combos. The caller buckets it into `skipped_unsupported_kind` (per `record_unsupported`'s contract). INTENTIONAL FALLBACK. LOW.

### L16. `reader/src/elf.rs:578` — `Err(_) => continue` on `section_by_index` failure

Bumps `stats.skipped_unresolved_target` and continues. Same flavour as M5 but specific to section indices (typically not user-visible since the dynsym path handles symbols and this branch is for `RelocationTarget::Section`, which only ELF objects with section relocations use). LOW.

### L17. `strider-py/src/pattern.rs:285` — `unwrap_or_else(|p| p.into_inner())` in `clear_graph_ptr`

This *is* the documented poison-recovery path. INTENTIONAL FALLBACK. LOW. (The inverse — the silent-fail path at line 292 — is H3.)

### L18. `strider-py/src/reader.rs:691, 726` — `if let Ok(m) = ob.extract::<PyMemoryMap>()` for polymorphic FromPyObject

This is the standard pyo3 polymorphic-extraction idiom: try first variant, fall back on extraction error. Both arms are explicitly handled and the final fallthrough returns a typed `PyTypeError`. INTENTIONAL FALLBACK. LOW.

### L19. `strider-py/src/pattern.rs:318, 321` — `if let Ok(c) = ob.extract::<PyRef<'_, PyCapture>>()` for `CaptureKeyOwned`

Same polymorphic-extraction idiom as L18. INTENTIONAL FALLBACK. LOW.

### L20. `strider-py/src/pattern.rs:400` — `if let Ok(b) = py_proxy.try_borrow(py)` then `b.clear_graph_ptr()`

The borrow can fail if the Python predicate is currently holding a borrow itself. Falling through silently means the proxy's graph pointer is *not* cleared — that's technically a stale-pointer hazard the next time someone re-uses the proxy, but the comment at lines 398-402 says the proxy is dropped right after this scope. Defensible since the proxy is short-lived; would benefit from an `eprintln` if the borrow ever fails (would indicate a re-entrant predicate, which is its own bug). LOW (borderline MED).

---

## Summary table

| ID | Severity | File:line | Issue |
|----|----------|-----------|-------|
| H1 | HIGH | `opt/src/flag_cmp_canonicalize/mod.rs:129` | `unwrap_or(a)` silently re-binds rhs capture to lhs on missing binding |
| H2 | HIGH | `strider-py/src/reader.rs:656, 666` | `.ok()?` swallows poisoned-mutex in `ReadOnlyMemory::read` |
| H3 | HIGH | `strider-py/src/pattern.rs:292` | `.ok()?` swallows graph-pointer mutex poison; predicate-callback path |
| H4 | HIGH | `opt/src/indirect_branch_resolve/inplace.rs:129` | `unwrap_or(U64)` masks malformed indirect-branch target type |
| H5 | HIGH | `opt/src/indirect_branch_resolve/mod.rs:281` | Missing `AnchorCallingContext` silently substitutes empty ctx |
| M1 | MED | `opt/src/indirect_branch_resolve/classify.rs:54-94`, `strider/src/indirect_resolve/classify.rs:49` | `analyze_known_bits` failures eprintln'd then converted to `None` instead of propagated |
| M2 | MED | `cfg/src/cfg/decode_cache.rs:60, 66` | `Mutex` poison silently absorbed; no diagnostic about originating panic |
| M3 | MED | `strider-py/src/dot.rs:16` | Unknown style names silently fall back to `dark` |
| M4 | MED | `cfg/src/cfg/builder/region_builder.rs:397-407` | `CallOther` shape errors converted to `DidntFinishProcessing` (asymmetric with IR-layer check) |
| M5 | MED | `reader/src/elf.rs:556-565` | Symbol-lookup errors bucketed indistinguishably with undef externals |
| M6 | MED | `reader/src/elf.rs:768-771` | Section parse failure printed once, not threaded into `RelocationStats` |
| M8 | MED | `strider/src/orchestrator.rs:784` | `node_outputs_exact::<1>(nid).ok()?` after `create_node` masks IR-arena bug |
| L1-L20 | LOW | various | Documented / unreachable / intentional |

---

## Highest-leverage fixes

If the team can only fix three things in one sitting, the highest leverage are:

1. **H1** (`flag_cmp_canonicalize` `unwrap_or(a)`) — five-character change to `.expect(...)`; protects the canonicalisation step every downstream pattern depends on.
2. **H2 + H3** (`strider-py` `.ok()?` poison swallowing) — the entire FFI surface either correctly propagates poisoned-mutex errors or it doesn't; these three sites are the only stragglers and they sit on the hot path (every `ReadOnlyMemory::read` and every `.when(...)` predicate).
3. **H5** (`anchor_contexts.get(addr).unwrap_or(&empty_ctx)`) — silent loss of every register-passing argument and clobber on a placeholder, with no diagnostic. Turn it into a typed error.

H4 and the MED tier follow naturally if the same audit pass continues.
