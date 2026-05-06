# Bug 1: Custom optimizer pipeline silently ignores `per_address_ccs`

**Status**: design — awaiting user review.
**Date**: 2026-05-06.
**Scope**: `strider` (`Strider::analyze_cfg*` API + `IrStrider`), `strider-py` (`strider.run` dispatcher).
**Out of scope**: Bug 2 (PT_LOAD-driven loader / writability tracking); deferred to a separate spec.

## Motivation

`strider.run(...)` accepts a `per_address_ccs` map that overrides the calling
convention for `Call` nodes whose target matches a user-supplied address.
The override is what lets a Linux-kernel reproducer treat `__fentry__` as a
no-clobber hook so `function_arg(0)` patterns survive the prologue.

Today the override silently drops on the floor when the caller passes a
custom `pipeline=` argument:

```python
res_default = strider.run(..., per_address_ccs=per_addr, pipeline=None)        # 1 hit
res_custom  = strider.run(..., per_address_ccs=per_addr, pipeline=custom_pipe) # 0 hits
```

The bug is in the strider-py dispatcher
([`crates/strider-py/src/run.rs:70-95`](../../crates/strider-py/src/run.rs)):
the `Some(pipeline)` branch routes to `run_with_custom_pipeline`, which
doesn't accept `per_address_ccs` at all, so the dispatcher discards it.
Inside `run_with_custom_pipeline`, the lift call
[`strider_borrow.inner.analyze_cfg(&cfg)`](../../crates/strider-py/src/run.rs#L214)
hits `Strider::analyze_cfg`, which threads no overrides — by the time the
user's pipeline runs, every `Call` node already carries the function-default
CC. No post-lift pass can retroactively rebuild calls.

The fix has to plumb `per_address_ccs` into `run_with_custom_pipeline` and
switch the lift call to the override-aware variant.

## Why this is also an API smell, and not just a missing parameter

The override-aware variant exists, but it sits behind a method-name expansion:

```rust
analyze_cfg(cfg)
analyze_cfg_with_vns(cfg, vns)
analyze_cfg_with_vns_and_overrides(cfg, vns, &per_addr_ccs)
```

Three methods, each adding one more argument by name suffix. The
strider-py custom-pipeline path called the first form because it was the
shortest — exactly the surface that made it possible to write a wrong
caller. A new per-call knob (attribution flags, validation toggles) would
multiply the matrix again.

`IrStrider` carries the override as `Option<&'a HashMap<u64,
BuiltCallingConvention>>` and exposes a setter
(`IrStrider::set_per_address_ccs`) that is invoked from
`analyze_cfg_with_vns_and_overrides` only when the map is non-empty. The
"if non-empty" guard is purely cosmetic — `HashMap::get` on an empty map
costs nothing — and the `Option<&'a …>` shape exists only to express
"sometimes I have a borrow."

The fix is to:

1. **Plumb the override through** so the bug stops happening.
2. **Collapse the analyze-method matrix into one + an options bag** so it
   stops being possible to write the same kind of bug for the next knob.
3. **Drop `set_per_address_ccs` and `Option<&'a …>`** so `IrStrider`
   always borrows a real (possibly empty) map and the lookup site loses a
   `.and_then`.

## Design

### `Strider::analyze_cfg*` collapses to two methods

`crates/strider/src/strider/pipeline.rs`:

```rust
/// Per-call lift configuration.  Empty defaults match the old
/// `analyze_cfg(cfg)` behaviour.
#[derive(Default)]
pub struct AnalyzeOptions<'a> {
    /// Pre-computed varnode set.  When `None`, `Strider` calls
    /// `find_all_unique_vns(cfg)` itself.  When `Some`, must be sorted
    /// by `pcode_lift::vn_sort_key` and must include every varnode any
    /// instruction in `cfg` references — under-tracking drops pcode reads.
    /// The orchestrator passes `Some(cached_vns)` so it shares one vn
    /// table across rebuild iterations.
    pub all_vns: Option<Vec<rsleigh::Vn>>,

    /// Per-target-address CC override map (already resolved against the
    /// Sleigh register table the function-default CC was built against).
    /// Empty by default — every direct `Call` uses the function-default CC.
    pub per_address_ccs: &'a HashMap<u64, BuiltCallingConvention>,
}

impl Strider {
    /// Translate a CFG with default options.
    pub fn analyze_cfg<R: rsleigh::MemReader>(&self, cfg: &cfg::Cfg<R>)
        -> Result<AnalyzeOutcome>
    {
        self.analyze_cfg_with(cfg, AnalyzeOptions::default())
    }

    /// Translate a CFG with caller-supplied `AnalyzeOptions`.
    pub fn analyze_cfg_with<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
        opts: AnalyzeOptions<'_>,
    ) -> Result<AnalyzeOutcome>;
}
```

The default `&'a HashMap` borrows a process-wide
`std::sync::OnceLock<HashMap<u64, BuiltCallingConvention>>` (or a `static`
`LazyLock`) so `AnalyzeOptions::default()` is cheap and lifetime-clean.

`analyze_cfg_with_vns` and `analyze_cfg_with_vns_and_overrides` are
**removed**. Only one external Rust caller exists
(`crates/strider/src/orchestrator.rs:827`), and it migrates to
`analyze_cfg_with`. Tests in `crates/strider/tests/` only call
`analyze_cfg` and stay green.

### `IrStrider::per_address_ccs` always borrows

`crates/strider/src/strider/mod.rs`:

```rust
// Before
pub(crate) per_address_ccs:
    Option<&'a std::collections::HashMap<u64, target::BuiltCallingConvention>>,
pub(crate) fn set_per_address_ccs(...) { ... }   // removed

// After
pub(crate) per_address_ccs:
    &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
```

`IrStrider::new` takes the borrow as a constructor parameter, defaulting to
the empty static map when callers don't care.

`crates/strider/src/strider/insn/control.rs:246` and `:281`:

```rust
// Before
let override_cc = self.per_address_ccs.and_then(|m| m.get(&target_addr));
// After
let override_cc = self.per_address_ccs.get(&target_addr);
```

`set_per_address_ccs` is deleted along with its caller in pipeline.rs.

### `strider-py` custom-pipeline path picks up the override

`crates/strider-py/src/run.rs`:

1. The `run` dispatcher forwards `per_address_ccs.unwrap_or_default()` to
   **both** branches.
2. `run_with_custom_pipeline` gains a
   `per_address_ccs_py: HashMap<u64, PyCallingConvention>` parameter.
3. Before lifting:
   ```rust
   let regs = sleigh.borrow(py).regs.clone();
   let built: HashMap<u64, target::BuiltCallingConvention> = per_address_ccs_py
       .into_iter()
       .map(|(addr, py_cc)| {
           py_cc.inner.build(&regs)
               .map(|b| (addr, b))
               .map_err(|e| into_lift_err(anyhow!(
                   "per-address CC at {addr:#x} unresolved: {e:?}")))
       })
       .collect::<PyResult<_>>()?;
   ```
4. The lift call becomes:
   ```rust
   let outcome = strider_borrow.inner.analyze_cfg_with(
       &cfg_obj.borrow(py).inner,
       AnalyzeOptions { per_address_ccs: &built, ..Default::default() },
   )?;
   ```

The orchestrator path is unchanged externally — it migrates internally
from `analyze_cfg_with_vns_and_overrides` to `analyze_cfg_with` with both
options set.

### Public docstring on `strider.run`

Update the `per_address_ccs` doc to state explicitly that it applies on
both pipeline paths. Drop the implicit "orchestrator-only" framing.

## Tests (TDD — write failing first, then fix)

### 1. Strider-py — both pipeline paths honour the override

Extend `crates/strider-py/tests/python/test_per_address_cc.py` with a new
test case that uses the same `mem.add_region(addr, bytes)` fixture
pattern already in that file (a minimal x86-64 byte sequence; no ELF
needed).

The new test asserts a 2×2 matrix on a fixture that exercises an
arg-clobbering call:

| pipeline       | per_address_ccs    | expected hits |
| -------------- | ------------------ | ------------- |
| `None`         | `{}`               | 0             |
| `None`         | `{hook: all_pres}` | 1             |
| `<custom>`     | `{}`               | 0             |
| `<custom>`     | `{hook: all_pres}` | 1             |

Where `<custom>` is an `OptimizerPipeline` populated with the
default-equivalent passes (mirroring the user's reproducer).

Concrete fixture (x86-64, single function at `0x1000`):

```
0x1000  e8 fb 0f 00 00     call 0x2000      ; "hook" (clobbers rdi by default)
0x1005  e8 f6 1f 00 00     call 0x3000      ; "sink" — pattern asserts on arg0
0x100a  c3                 ret
```

The pattern is `call().at(0x3000).arg(0, function_arg(0))`. By SystemV
ABI rdi enters the function carrying arg0; the hook clobbers it under
the default CC; the sink's arg0 reflects the clobber. With the override
mapping the hook to `x86_64_all_preserving`, rdi flows through and the
sink's arg0 is the original `function_arg(0)`.

On master, the bottom-right cell is 0 (the bug). After the fix it's 1.
The other three cells confirm the override is meaningful (top row) and
the custom pipeline is functionally equivalent to default when the
override isn't needed (left column).

### 2. Rust — `analyze_cfg_with` applies the override directly

`crates/strider/tests/analyze_cfg_with_overrides.rs` (new):

Smoke test that calls `Strider::analyze_cfg_with` with a non-empty
`per_address_ccs` and verifies the resulting Call node carries the
override CC's footprint. Mirrors `crates/strider/tests/per_address_cc.rs`
but exercises the per-call API directly (without the orchestrator).

### 3. Existing tests stay green

- `crates/strider/tests/per_address_cc.rs` (orchestrator path) — unchanged.
- `crates/strider/tests/per_address_cc_indirect.rs` (indirect-tail-call
  apply path) — unchanged.
- All other `analyze_cfg(cfg)` callers in tests/ — unchanged signature.

## Migration

External Rust callers of `analyze_cfg_with_vns` / `_with_vns_and_overrides`:

- `crates/strider/src/orchestrator.rs:827` — only call site. Migrates to
  `analyze_cfg_with`.

External callers of `analyze_cfg` (the simple form): unchanged. No deprecation
window needed because the workspace owns every caller.

## Implementation order

1. **TDD red**: write failing tests
   (`test_run_per_address_cc.py` + `analyze_cfg_with_overrides.rs`).
   Run `cargo test --workspace` and `uv run pytest`; both new tests must
   fail with the expected wrong-hit-count signal.
2. Add `AnalyzeOptions` and `analyze_cfg_with` in
   `crates/strider/src/strider/pipeline.rs`.
3. Migrate `IrStrider`: remove `Option`, drop setter, add constructor
   parameter. Update `crates/strider/src/strider/insn/control.rs`
   call sites.
4. Migrate `crates/strider/src/orchestrator.rs:827` to `analyze_cfg_with`.
5. Delete `analyze_cfg_with_vns` and `analyze_cfg_with_vns_and_overrides`.
6. Run `cargo test --workspace` — all tests stay green except the new
   `analyze_cfg_with_overrides.rs` (still failing if (3)–(4) regress; should
   pass).
7. Update `crates/strider-py/src/run.rs`: extend
   `run_with_custom_pipeline`, update the dispatcher.
8. Run `uv run pytest` — `test_run_per_address_cc.py` flips green.
9. Update `strider.run` docstring.
10. Code review (feature-dev:code-reviewer).
11. `cargo clippy --workspace`.

## Risks

- **Static empty map lifetime**: `AnalyzeOptions::default()` needs an
  `&'static HashMap`. The workspace targets Rust edition 2024 (Cargo.toml),
  which implies Rust ≥1.85, so `std::sync::LazyLock<HashMap<…>>` is
  available and is the right tool. The static lives in `pipeline.rs`.
- **Public API churn**: removing `analyze_cfg_with_vns` is breaking for any
  external caller. Internal grep confirms only one workspace caller, plus
  one removed-by-this-spec strider-py call and tests that use
  `analyze_cfg`. No external Rust crate depends on `strider`.
- **Test-fixture availability**: bug 2 (`.rodata` filter) means kernel
  vmlinux can't ground the strider-py test directly. The test uses a
  hand-rolled minimal ELF / a fixture already present under
  `crates/strider-py/tests/fixtures/`, so the test does not depend on
  Bug 2 being fixed first.

## Cost

- ~120 LOC across `strider` (refactor) + ~30 LOC in `strider-py` (bug fix)
  + ~80 LOC tests = ~230 LOC total.
- Single PR.
