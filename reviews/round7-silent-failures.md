# Round 7 — Silent failures & inadequate fallback audit

Scope: production paths in `crates/*/src/**/*.rs` (tests, examples,
benches excluded).  Findings ordered by severity — most-impactful first
in each tier, with file:line and fix proposal.

---

## HIGH severity — fallbacks that silently corrupt analysis or hide errors

### H1 — `PyMemoryMap` ROM reads always little-endian regardless of arch
File: `crates/strider-py/src/reader.rs:567-579`

```rust
impl ReadOnlyMemory for PyMemoryMap {
    fn read(&self, _space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        if size == 0 || size > 8 { return None; }
        let table = self.lookup_table().ok()?;
        let mut buf = [0u8; 8];
        let n = table.read(addr, &mut buf[..size])?;
        if n != size { return None; }
        Some(u64::from_le_bytes(buf))
    }
}
```

Why wrong: `PyMemoryMap` has no endianness field (see struct at line 90-106).
Every consumer that goes through this path on a big-endian target (MIPS BE,
AArch64 BE, ARM BE) gets byte-swapped jump-table entries / .rodata constants
silently.  Compare `reader::ElfFileMemReader::read` at
`crates/reader/src/elf.rs:316-343` which threads `is_little_endian` properly.
A user who builds a `MemoryMap`, populates it from a BE ELF, and uses it as a
ROM for `LoadReadOnly` on a BE target gets wrong constants and therefore
wrong jump-table classifications, wrong constant-folded comparisons, and
wrong indirect-branch resolutions — all without any error.

Fix: thread arch endianness into `PyMemoryMap` (either from the first
`add_region_from_elf` ELF or via an explicit ctor parameter); use
`from_le_bytes` / `from_be_bytes` accordingly.  At minimum, expose a
required `endianness=` kwarg on `MemoryMap.__init__` and store it.

### H2 — `PyReadOnlyMemoryAdapter::read` swallows every Python exception
File: `crates/strider-py/src/reader.rs:499-518`

```rust
impl ReadOnlyMemory for PyReadOnlyMemoryAdapter {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        ...
        Python::with_gil(|py| -> Option<u64> {
            let result = self.py_obj.call_method1(py, "read", (addr, size)).ok()?;
            if result.is_none(py) { return None; }
            result.extract::<u64>(py).ok()
        })
    }
}
```

Why wrong: Two error-eating `.ok()`s.  If the user's Python `read()` raises
*anything* (KeyError, AttributeError, an in-progress refactor's
NotImplementedError) we silently report "no data".  `LoadReadOnly` then
falls back to "leave the Load as-is", which subsequent passes interpret as
"runtime value, can't fold" — silently degrading every match that depended
on the constant.  Worse, `extract::<u64>(py).ok()` silently maps "user
returned a string by mistake" to None.  Compare the symmetric
`PyMemReaderAdapter::read` at line 436-458 which surfaces every error
through `anyhow::anyhow!("PyMemReader.read raised: {e}")` with full
context.

Fix: change the trait `ReadOnlyMemory::read` signature to return
`anyhow::Result<Option<u64>>`, or at minimum convert exceptions to
`e.print(py)` plus a structured Sentry-equivalent log entry — but
preferably raise the Python exception back through PyO3 so the user's
exception lands at the `find_all`/`run` call site.

### H3 — `analyze_known_bits` errors swallowed in classifier shim
Files:
- `crates/opt/src/indirect_branch_resolve/classify.rs:54`
- `crates/opt/src/indirect_branch_resolve/classify.rs:78`
- `crates/strider/src/indirect_resolve/classify.rs:49`

```rust
let known = crate::analyze_known_bits(fg).ok()?;        // opt::classify_anchor
let known = opt::analyze_known_bits(graph).ok()?;       // strider shim
```

Why wrong: `analyze_known_bits` returns `Result<...>` because it bails on
internal contradictions (`Kb::merge` at `known_bits/mod.rs:56-62` errors on
"a bit provably 1 in one source and provably 0 in the other" — a real bug
indicator, per the comment at line 50-55).  These shims convert that error
into "classifier returned None", which the orchestrator interprets as "still
unresolved at this iteration" and silently retries.  At fixed point the
caller sees a generic `UnresolvedIndirectBranch` rather than the actionable
"KB analysis found a contradiction in subgraph X" error.

Note that the parallel call at `indirect_branch_resolve/mod.rs:261`
correctly uses `?` to propagate.  The two `ok()?` callers are inconsistent
with that policy.

Fix: change the classifier signatures to `Result<Option<ResolvedTargets>>`
and propagate via `?`.  The orchestrator already has its own `None`
fallthrough path; reserve `None` for "shape didn't match" (deterministic
classifier output) and `Err` for "analysis itself failed".

### H4 — `wrap_when` predicate exception printed-and-swallowed
File: `crates/strider-py/src/pattern.rs:370-396`

```rust
match result {
    Ok(obj) => obj.extract::<bool>(py).unwrap_or(false),
    Err(e) => {
        e.print(py);                 // prints to stderr
        false                        // treats as "no match"
    }
}
```

Plus line 374-377: `Py::new` failure → `return false` (silent).
Plus line 386: predicate returned non-bool → `unwrap_or(false)` (silent).

Why wrong: A user's `.when(lambda m: m.get_int("x") + 1 == 2)` raising a
TypeError (because `m.get_int("x")` returned None) will print to stderr,
report "no match", and continue.  The user's `find_all` returns an empty
list, and the actual bug — that capture "x" never bound or that the type
was wrong — is invisible.  For interactive use this is the worst possible
failure mode: no stack trace at the call site, the prints can be missed in
notebook output, and the empty result looks plausible.

Fix: capture the first predicate exception, abort the walk, and re-raise
it from `Graph.find_all` / `Graph.match_at`.  An exception in user code
should never be silently demoted to a falsy match result.

### H5 — `run_with_custom_pipeline` discards user-provided ROM
File: `crates/strider-py/src/run.rs:184-213`

```rust
fn run_with_custom_pipeline(
    ...
    rom: Option<RomInput>,
    pipeline: &crate::opt::PyOptimizerPipeline,
    ...
) -> PyResult<PyRunResult> {
    let _ = rom; // custom pipeline owns its own pass list
    ...
    let cfg_obj = Py::new(py, crate::cfg::build_cfg(py, sleigh.clone_ref(py), entry, ...)?, ...)?;
    ...
}
```

Why wrong: The comment justifies dropping the rom because "custom pipeline
owns its own pass list", but the cfg builder *also* uses a rom — for
mini-IR indirect-branch resolution at lift time
(`OptionsBuilder::set_read_only_memory`, used by
`cfg/builder/indirect_resolve.rs`).  Silently discarding the user's rom
means their custom-pipeline run resolves fewer indirect branches than the
default pipeline would, with no diagnostic.  The user thinks "my custom
pipeline is configured", reality: cfg silently lost its rodata view.

Fix: thread `rom` into `cfg::Builder` via `OptionsBuilder::set_read_only_memory`
the same way the default `run()` path does
(`orchestrator.rs:788-790`).  Or, if there's a real reason to drop it, raise
an error on `rom.is_some()` so the user can't silently pass an unused
argument.

### H6 — Single-insn region with one OOB CondBranch successor: in-range edge dropped, region degenerates to `TailCall`
File: `crates/cfg/src/cfg/builder/region_builder.rs:356-385`

```rust
(true, false) | (false, true) => {
    let in_range = if true_oob { next_insn_addr } else { target_addr };
    if self.insns.len() > 1 {
        self.insns.pop();
        let region = self.finish_current_region(RegionTerminator::Branch)?;
        self.builder.work_queue.push((Some((region, RegionEdgeKind::Branch)), in_range));
    } else {
        // self.insns.len() == 1 — only the CondBranch insn in the region.
        self.finish_current_region(RegionTerminator::TailCall {
            target: in_range.machine_addr.addr,
        })?;
    }
}
```

Why wrong: in the `len() > 1` branch the conditional is intentionally
dropped (comment: "The conditional is lost, but the lift completes"); in
the `len() == 1` branch the *in-range edge is replaced* with a TailCall to
the in-range target, **silently destroying the only intra-function edge of
the region**.  The comment dismisses this as "essentially unobserved in
real binaries", but in practice short trampoline functions with a single
conditional branch (kernel `__init` stubs, FreeBSD trap table entries) do
hit this, and the user's CFG ends up with a phantom tail call that doesn't
exist in the source.

Fix: at minimum, plumb a typed warning into the builder
(`RelocationStats`-style breakdown) so the orchestrator / Python layer
can surface "region @0x… degraded conditional to tail call".  For the
`len() == 1` path, prefer to bail with a typed error: a single-insn
region of just a CondBranch is a degenerate function-extent edge case
the caller should be told about, not silently masked.

### H7 — `apply_elf_relocations` mis-buckets malformed-symbol errors
File: `crates/reader/src/elf.rs:507-590` (`.ok()` on lines 513, 517, 561, 565)

```rust
RelocationTarget::Symbol(idx) => {
    let resolved = if let Some(dynsym) = obj.dynamic_symbol_table() {
        dynsym.symbol_by_index(idx).map(|s| (s.address(), s.is_undefined())).ok()
    } else {
        obj.symbol_by_index(idx).map(|s| (s.address(), s.is_undefined())).ok()
    };
    let Some((addr, undef)) = resolved else {
        stats.skipped_unresolved_target += 1;     // covers BOTH cases
        continue;
    };
    ...
}
```

Why wrong: The function-level docstring at line 452-459 says *"Returns an
error only on a malformed ELF (a relocation whose target symbol or section
index doesn't resolve)"* — but in practice a malformed-ELF symbol-index
error from `object` (e.g. index past `.dynsym`) gets bucketed as
`skipped_unresolved_target`, **identical** to the legitimate
"weak/undefined extern" case.  The user has no way to tell "10000 weak
externs" from "10000 corrupt indices".

The same problem repeats at line 577-583 for `RelocationTarget::Section`.

Fix: distinguish.  Either (a) propagate `symbol_by_index`'s `Err` via `?`
since the doc-comment promises this is an error condition, or (b) split
`RelocationStats` to add `skipped_malformed_target`/`error_target`
counters and stop conflating.

---

## MED severity — graceful but information-hiding

### M1 — `find_loadable_section_containing` swallows `sec.data()` errors
File: `crates/reader/src/elf.rs:729`

```rust
if sec.data().map(|d| d.is_empty()).unwrap_or(true) {
    return false;
}
```

`sec.data()` returning `Err` (malformed section table) is treated identically
to `data() == &[]` (SHT_NOBITS).  The autoload helper then leaves the site
uncovered and `apply_elf_relocations` reports the entry as
`skipped_no_region`.  The malformed-ELF signal is lost.

Fix: `sec.data().map_err(...)` → propagate as anyhow context, since the
caller already returns `Result`.

### M2 — `sleigh_regs.name_to_vn` unresolved register fail-closed
The pattern at `crates/strider/src/strider/insn/mod.rs:159-165` is correct
(propagates `anyhow!("user-op {name:?} ABI references unknown register
{n:?}")`).  Calling out for completeness — *this is a positive example* —
but worth checking that the equivalent `BuiltCallingConvention::build`
error path is similarly surfaced.

### M3 — `Kb::from_const` silently zeros on unrepresentable types
File: `crates/opt/src/known_bits/mod.rs:38-46`

```rust
fn from_const(val: u128, ty: NodeOutputType) -> Self {
    let masked = ty.get_unsigned_int(val).unwrap_or(0);
    let type_mask = u64_type_mask(ty).unwrap_or(0);
    let masked_u64 = u64::try_from(masked).unwrap_or(0);
    Kb { ones: masked_u64, zeros: type_mask ^ masked_u64 }
}
```

If called with a U128/U256 type the function returns `Kb { ones: 0, zeros: 0 }`
(== "fully unknown") which is sound as an over-approximation but masks a
"caller should have gated on `fits_u64`" bug — the call site in `analyze`
does check, but a future caller might not.  The two `unwrap_or(0)` calls
on the same line silently turn three independent precondition violations
into one safe-but-meaningless result.

Fix: change to `Option<Kb>` and let the caller assert the precondition.

### M4 — `clear_graph_ptr` silently no-ops on poisoned mutex
File: `crates/strider-py/src/pattern.rs:270-274`

```rust
fn clear_graph_ptr(&self) {
    if let Ok(mut g) = self.graph_ptr.lock() {
        *g = None;
    }
}
```

If a panic poisons this mutex, the cleanup silently fails to clear the
raw pointer.  The companion `with_graph` at line 278-286 then dereferences
through `unsafe { &*ptr }` against a graph that may already have been
dropped.  The SAFETY comment at line 281-283 assumes "the outer Mutex
guard prevents the cleanup from racing this call" — which assumes a
non-poisoned mutex.

Fix: use `lock().unwrap_or_else(|e| e.into_inner())` (matches
`decode_cache.rs:60` policy) so the clear always runs, or upgrade to a
panic on poison since pointer hygiene matters.

### M5 — `PyMemoryMap` ROM ignores `_space` parameter
File: `crates/strider-py/src/reader.rs:568`

```rust
fn read(&self, _space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
```

`_space` is silently ignored; a `Load(VnSpace::REGISTER)` against a
`PyMemoryMap` ROM gets answered with bytes from the RAM-keyed region
table.  Compare `ElfFileMemReader::read` at `elf.rs:317-320` which
explicitly returns `None` for non-RAM spaces.  The `PyReadOnlyMemoryAdapter`
at line 507 also gates on `space != RAM`.

Fix: add the same `if space != rsleigh::VnSpace::RAM { return None; }`
guard to `PyMemoryMap::read`.

---

## LOW severity — benign defaults worth noting

### L1 — `pcode_count` overflow saturates to `u64::MAX`
File: `crates/cfg/src/cfg/builder/region_builder.rs:165`

```rust
let pcode_count = u64::try_from(lift_res.insns.len()).unwrap_or(u64::MAX);
```

On 32/64-bit hosts `usize → u64` is infallible, so the fallback can never
fire.  Cosmetic.

### L2 — `flag_cmp_canonicalize` `cap_b` fallback to lhs
File: `crates/opt/src/flag_cmp_canonicalize/mod.rs:129`

```rust
let b = m.output(rule.cap_b).unwrap_or(a);
```

Fallback to `a` is intentional when `cap_b` isn't bound (per the rule
shape), but a future rule that mistakenly omits the `cap_b` capture would
silently generate wrong RHS.  Consider: `expect("rule must bind cap_b")`
or `?`.

### L3 — Fingerprint-debug-only `unwrap_or_default` and `unwrap_or_else(|_| format!("{kind:?}"))`
File: `crates/ir/src/dot/label.rs:279, 304, 322`

Visualization-only fallbacks that print debug-formatted node kinds when
`call_other_name` / `pretty_label` are missing.  Cosmetic; no analysis
impact.

### L4 — `i64::try_from(signed).ok()` in `int_const_signed`
File: `crates/opt/src/sp_expr.rs:206, 234`

When the constant overflows `i64`, returns `None`.  Caller short-circuits
correctly.  Sound under-approximation.

### L5 — `worklist::detach_unreachable_nodes` Changed flag dropped
Files: `crates/opt/src/redundant_phis/mod.rs:207`,
`crates/opt/src/function_args/mod.rs:108`

Both have explicit comments saying the discard is intentional ("hygiene-
only post-pass return"); the function returns a `bool`, not a `Result`,
so no error is being eaten.  Documented and fine.

---

## Positive examples worth preserving

Several spots get this right and serve as the model:

- `crates/strider/src/orchestrator.rs:184-191`: iteration cap with clear
  error message and dynamic bound formula.
- `crates/strider/src/orchestrator.rs:380-388`: typed
  `UnresolvedIndirectBranch` error rather than generic anyhow.
- `crates/strider/src/orchestrator.rs:396-404`: stall budget with
  bail-on-zero plus clear context.
- `crates/strider/src/strider/insn/mod.rs:124-127`: typed
  `UnknownCallOtherError` with name surfaced.
- `crates/strider-py/src/reader.rs:436-458` (`PyMemReaderAdapter::read`):
  every Python-side error wrapped with `anyhow::anyhow!("PyMemReader.read
  raised: {e}")` for context.
- `crates/cfg/src/cfg/decode_cache.rs:54-67`: poison-recovery is
  documented and justified.
- `crates/opt/src/known_bits/mod.rs:50-62` (`Kb::merge`): explicitly
  errors on contradictions with full context — *the right pattern that
  classify.rs:54/78 then immediately throws away*.
