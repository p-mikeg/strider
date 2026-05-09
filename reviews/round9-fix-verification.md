# Round 9 — Pre-fix verification of HIGH findings

Each Round-9 HIGH finding re-verified against actual source code, with cited file:line and quoted code. The sysret finding (H-1) was already disproven on inspection. This pass re-validates the other 8.

**Verdict legend:**
- **CONFIRMED** — agent's claim matches the code; bug is real
- **CONFIRMED-LOW** — bug is real but very-low-impact in practice
- **PARTIAL** — claim has merit but the existing comment/design has a justification
- **BY DESIGN** — claim describes documented behaviour, not a bug
- **FALSE** — agent misread the code

---

## H-1 — `sysret` classified `NoReturn`

**Source:** EA3 CRITICAL-1.
**Verdict:** **FALSE.**

SYSRET genuinely terminates the analyzed kernel function: control transfers to user-mode at `RCX` (a different address space, different CPL). For single-function analysis of the kernel side, terminating the region is correct. The agent's "kernel exit truncated from CFG" framing was misleading — termination IS the desired behaviour for the kernel function being analyzed.

**Action:** **Skip.** No fix.

---

## H-2 — `ConstantFold::rule_and_dist` and other multi-node rules emit fingerprint-empty inner nodes

**Source:** EA1 Finding 1.
**Locations:** `crates/opt/src/constant_fold/rules.rs:62-71`, `crates/pattern/src/rewrite.rs:81-95`.

**Code (rewrite.rs:81-94):**
```rust
match outcome {
    BuildOutcome::Skip => Ok(false),
    BuildOutcome::Out(new_out) => {
        // ... comment about absorbing rewritten root's fingerprint ...
        let new_node = ctx.graph.get_node_from_output(new_out);
        ctx.graph.extend_asm_fingerprint_from(new_node, node);
        let changed = ctx.graph.replace_all_uses(root_out, new_out)?;
        Ok(changed)
    }
}
```

**Code (rules.rs:62-71, `rule_and_dist`):**
```rust
let rule_and_dist = boxed_rule(rewrite_rule(
    and(or(and(var(a), any_int_const(c1)), and(var(b), any_int_const(c2))), any_int_const(c3)),
    or(
        and(var(a), int_const_with!([c1: uint, c3: uint] => c1 & c3)),
        and(var(b), int_const_with!([c2: uint, c3: uint] => c2 & c3)),
    ),
));
```

**Verdict:** **CONFIRMED.** The RHS builds three new nodes (outer `Or`, two inner `And`s), plus two fresh `IntConst` masks if `C1&C3` / `C2&C3` aren't already cached. `extend_asm_fingerprint_from` is called only on the outermost `Or` (`new_node`). The two inner `And` nodes are non-exempt (`IntBinaryOp::And`), and freshly-built `IntConst(C1&C3)` nodes are also non-exempt. They will fail `validate_with_options(check_asm_fingerprints: true)`.

**Severity assessment:** MED. The default `validate(graph, entry)` does NOT check fingerprints — only opt-in `validate_with_options { check_asm_fingerprints: true }` does. Production strider uses default `validate`, so this doesn't break anything user-facing. But the asm-fingerprint *contract* (CLAUDE.md: "passes may grow fingerprints but must never shrink them or replace a node with one whose fingerprint omits an ancestor's addresses") is technically violated for any caller who enables the opt-in check.

**Action:** **Fix.** Modify `rewrite.rs:81-94` to walk the freshly-built RHS subtree and union the contributor into every fresh non-exempt interior node (not just the root). Or, document that multi-node-RHS rules need to use `create_node_attributed` directly. The TDD approach: write test that fails with `check_asm_fingerprints: true` after running `ConstantFold` on a graph that triggers `rule_and_dist`, then fix.

---

## H-3 — `FunctionArgDetect` exact-width path drops Load fingerprint

**Source:** Ask-8 R2 Finding 1.
**Location:** `crates/opt/src/function_args/mod.rs:329-352`.

**Code:**
```rust
for (load, load_ty) in load_types {
    let [old_out] = fg.node_outputs_exact::<1>(load)?;
    if load_ty == max_type {
        // FunctionArg is exempt from the fingerprint check; no need
        // to absorb the load's fingerprint into it (and doing so
        // would couple FunctionArg's identity to the loads it
        // happens to subsume).
        fg.replace_all_uses(old_out, new_out)?;
    } else {
        // Truncate path — absorbs load fingerprint into Truncate
        ...
        fg.extend_asm_fingerprint_from(trunc, load);
        fg.replace_all_uses(old_out, trunc_out)?;
    }
    fg.detach_node_inputs(load);
}
```

**Verdict:** **PARTIAL / DEBATABLE.**

The existing comment explicitly addresses this case: "FunctionArg is exempt from the fingerprint check; no need to absorb the load's fingerprint into it." The asymmetry with the Truncate path is justified — Truncate is non-exempt and would fail the opt-in fingerprint check otherwise; FunctionArg is exempt by design (structural node).

The agent's concern is that downstream consumers of the new FunctionArg output lose the Load's contributing address from their ancestor set. But:
- Each consumer already has its own fingerprint (its own asm address from lift time).
- The contract phrase "replace a node with one whose fingerprint omits an ancestor's addresses" applies to the NODE BEING REPLACED, not its consumers.
- FunctionArg replaces the Load directly; FunctionArg is exempt, so it doesn't need a fingerprint.

The Truncate path absorbs because the Truncate is itself non-exempt and would otherwise fail the opt-in check. The exact-width path doesn't need this because the replacement (FunctionArg) IS exempt.

**Action:** **No fix.** The existing comment correctly justifies the asymmetry. The agent appears to have misread the contract (it applies to the replaced node, not to downstream consumers of the replacement).

---

## H-4 — `read_or_init_var` silent drop on size mismatch

**Source:** R9-2C #1.
**Location:** `crates/strider/src/orchestrator.rs:786`.

**Code:**
```rust
let ty: ir::node::NodeOutputType = vn.size.try_into().ok()?;
```

**Verdict:** **CONFIRMED-LOW.**

The `?` does silently skip varnodes of unsupported byte size. Caller (line 735-739) skips the ret-val:
```rust
for vn in cc.ret_val_regs() {
    if let Some(out) = read_or_init_var(graph, region, initial_var_index, *vn) {
        ctx.ret_val_outputs.push(out);
    }
}
```

In practice all CC presets use sizes ∈ {1, 2, 4, 8, 10, 16, 32, 64} which all map to `NodeOutputType` variants. The function doc explicitly states: "Returns `None` when the varnode's byte size has no matching `NodeOutputType`." This is documented behaviour.

**Severity assessment:** LOW. No real-world CC preset has a 3-byte / 5-byte / 7-byte register. The defensive fix (surface as Err) is principled but unlikely to ever fire.

**Action:** **Defer.** Document the invariant ("all CC presets use NodeOutputType-compatible sizes") in a comment if not already, but don't change the silent-drop behaviour. If a future CC preset introduces an exotic size, that's when it needs revisiting.

---

## H-5 — `build_anchor_calling_context` clobber loop drops unsupported sizes

**Source:** R9-2C #2.
**Location:** `crates/strider/src/orchestrator.rs:728-734`.

**Code:**
```rust
for vn in clobber_iter {
    let Ok(ty) = ir::node::NodeOutputType::try_from(vn.size) else {
        continue;
    };
    ctx.clobbered_kinds.push(ir::node::NodeOutputKind::OutputType(ty));
}
```

**Verdict:** **CONFIRMED-LOW.** Same as H-4 — silent skip, but no real CC has unsupported clobber sizes.

**Action:** **Defer.** Same rationale as H-4.

---

## H-6 — `classify_anchor_with_rom_and_sp` eprintln+None on KB contradiction

**Source:** R9-2C #3.
**Location:** `crates/strider/src/indirect_resolve/classify.rs:49-57`.

**Code:**
```rust
let known = match opt::analyze_known_bits(graph) {
    Ok(k) => k,
    Err(e) => {
        eprintln!(
            "strider: classify_anchor_with_rom_and_sp: analyze_known_bits failed: {e:?}"
        );
        return None;
    }
};
```

**Verdict:** **CONFIRMED.**

`analyze_known_bits` returns `Err` only on `Kb::merge` contradiction (incompatible constants merging at a single output) — a real IR-level bug. The current code prints to stderr and returns `None`, which the orchestrator interprets as "still unresolved at this iteration; try again or surface as `UnresolvedIndirectBranch`." Real bugs masquerade as benign unresolved-branch noise.

**Severity:** MED-HIGH. Diagnostic-only; doesn't change correctness, but obscures real bugs.

**Action:** **Fix.** Surface the error rather than swallowing. Two reasonable shapes:
1. Change return type to `Result<Option<ResolvedTargets>>` and propagate.
2. Convert to a typed strider error (e.g. `KbContradiction { node: NodeOutputId, source: anyhow::Error }`) and return via `Result`.

Need to check call sites to pick the right shape.

---

## H-7 — `wrap_when` leaves dangling graph pointer when `try_borrow` fails

**Source:** Ask-8 R3 ISSUE-1.
**Location:** `crates/strider-py/src/pattern.rs:460-464`.

**Code:**
```rust
// Always invalidate the proxy's graph pointer so any
// subsequent use from Python doesn't deref a stale ptr.
if let Ok(b) = py_proxy.try_borrow(py) {
    b.clear_graph_ptr();
}
```

**Verdict:** **CONFIRMED-LOW.**

The comment says "Always invalidate" but the code only invalidates on `Ok`. `try_borrow` returns `Err` only when there's an active mutable borrow. Since `clear_graph_ptr` is `&self`, the only way `try_borrow` (immutable) fails is if some other code path is holding `&mut PyPartialMatch`. The proxy is `unsendable` and the predicate has finished by line 459, so an active mutable borrow at line 462 is essentially impossible.

The defensive fix would either:
1. Bypass PyO3 borrow check via `Arc<Mutex<Option<*const Graph>>>` outside the pyclass.
2. Use `borrow` (panicking) instead of `try_borrow` so failure is visible.

**Action:** **Defer / minor.** The "Always" in the comment is technically a lie but the failure mode is essentially unreachable in practice. The principled fix (1) is a non-trivial restructuring. Lower-priority than other items.

---

## H-8 — `wrap_when` swallows `KeyboardInterrupt` and `SystemExit`

**Source:** R9-2C #5.
**Location:** `crates/strider-py/src/pattern.rs:475-481`.

**Code:**
```rust
Err(e) => {
    // Surface the predicate's exception to stderr but
    // treat it as "no match" to avoid aborting
    // find_all in the middle of a walk.
    e.print(py);
    false
}
```

**Verdict:** **CONFIRMED.**

The blanket `Err(e)` arm catches `KeyboardInterrupt` and `SystemExit` along with all other exceptions. The comment justifies "no match to avoid aborting find_all" for ordinary predicate bugs, but `KeyboardInterrupt` / `SystemExit` are control-flow exceptions meant to propagate.

**Severity:** MED. Ctrl-C cannot interrupt a slow `find_all` once execution enters a `.when(f)` predicate. This is a real UX regression for interactive Python sessions.

**Action:** **Fix.** Distinguish base exceptions:
```rust
Err(e) => {
    if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
        || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
    {
        e.restore(py);  // re-raise on next GIL acquire
    } else {
        e.print(py);
    }
    false
}
```

---

## H-9 — `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` partial-state

**Source:** R9-2D H1.
**Location:** `crates/ir/src/function.rs:100-126`.

**Code:**
```rust
/// # Contract — caller responsibility
///
/// The returned `BuiltFunctionGraph` has **empty** `variables`,
/// `call_clobbered`, `ret_val_regs`, and `call_other_clobbered`.
/// Callers MUST pass it only to consumers that touch `graph` and
/// `entry`; consulting any other field returns a meaningless
/// empty value silently.  ...
#[must_use]
pub fn from_graph_and_entry_for_rewrite(graph: crate::graph::Graph, entry: NodeId) -> Self {
    Self {
        graph,
        entry,
        variables: PrimaryMap::new(),
        call_clobbered: Box::new([]),
        ret_val_regs: Box::new([]),
        call_other_clobbered: Box::new([]),
    }
}
```

**Verdict:** **BY DESIGN.**

The method is loudly documented with the partial-state contract. Round 8 added `RewriteCtx { graph, entry }` (visible elsewhere in the codebase) as the proper shape. The remaining `from_graph_and_entry_for_rewrite` is a transitional API; the round-9 simplification is to migrate any remaining callers to `RewriteCtx` and delete the partial-state ctor.

**Action:** **Phase C simplification, not a bug fix.** Migrate callers to `RewriteCtx`, delete the constructor. Out of scope for HIGH-severity correctness fixes.

---

## Summary

| # | Verdict | Action |
|---|---------|--------|
| H-1 | FALSE | Skip |
| H-2 | CONFIRMED (MED) | **Fix with TDD** |
| H-3 | PARTIAL / not a bug | Skip |
| H-4 | CONFIRMED-LOW | Defer (document invariant) |
| H-5 | CONFIRMED-LOW | Defer (document invariant) |
| H-6 | CONFIRMED (MED-HIGH) | **Fix** |
| H-7 | CONFIRMED-LOW | Defer |
| H-8 | CONFIRMED (MED) | **Fix** |
| H-9 | BY DESIGN, refactor candidate | Phase C |

**Recommended Phase B fixes (verified worth doing):**
1. **H-2** — multi-node `rewrite_rule` fingerprint propagation. TDD-approachable.
2. **H-6** — KB contradiction surface as Result instead of eprintln+None.
3. **H-8** — KeyboardInterrupt/SystemExit re-raise from `wrap_when`. Small, focused fix.

**Skip / defer:**
- H-1 (false), H-3 (not a bug), H-4/H-5 (low impact), H-7 (low probability), H-9 (refactor not bug).

This narrows Phase B from "9 HIGH items" to "3 verified-real fixes." That's a much more realistic scope.

I'll now also need to verify the IMPORTANT-tier findings before fixing them — they may have similar misinterpretations.
