# strider-py opts structs + custom pipelines + optimize consolidation

Follow-up to the strider-py API redesign (same branch,
`refactor/2026-07-02-py-api-redesign`). Replace the `analyze`/`build_cfg`
keyword-argument piles with Python opts structs that mirror the Rust
`LiftOptions`/`CfgOptions` (readable single source of truth), add a
per-function optimizer-pipeline override, move `optimize` onto the `Lifter`,
and delete `reoptimize`.

## Goals

- One opts struct per entry point, mirroring the Rust structs exactly (nested).
- Custom optimizer pipeline overridable PER FUNCTION (never on `lifter()`).
- `optimize` lives on the `Lifter` and takes a `Function`; `reoptimize` gone.

## Python opts structs (mirror the Rust)

`CfgOptions` mirrors `strider_cfg::CfgOptions` (user-facing subset — the
orchestrator-internal `known_targets` is NOT exposed):

```python
class CfgOptions:
    def __init__(self, *, function_max_size: int | None = None,
                 allow_code_before_start_addr: bool = False) -> None: ...
```

`LifterOptions` mirrors `strider_lift::LiftOptions` (nested `cfg`) plus the
optimize-side knobs currently flattened into `analyze`, plus the per-function
pipeline override:

```python
class LifterOptions:
    def __init__(self, *, cfg: CfgOptions = CfgOptions(), compact: bool = True,
                 per_address_ccs: dict[int, CallingConvention] | None = None,
                 calls_clobber: bool = False,
                 assume_distinct_sp_bases_disjoint: bool = False,
                 alias_mode: str = "stack_global_disjoint",
                 pipeline: OptimizerPipeline | None = None) -> None: ...
```

Both live in the top-level `strider` namespace (like `SleighArch`), documented
in `strider/__init__.pyi`.

## Entry-point signatures (replace the kwargs)

```python
class Lifter:
    def build_cfg(self, entry: int, opts: CfgOptions = CfgOptions()) -> Cfg: ...
    def analyze(self, entry: int, cc: CallingConvention,
                opts: LifterOptions = LifterOptions()) -> tuple[Function, list[int]]: ...
    def optimize(self, function: Function,
                 pipeline: OptimizerPipeline | None = None) -> None: ...

class ElfLifter(Lifter):
    def analyze(self, target: str | int, cc: CallingConvention | None = None,
                opts: LifterOptions = LifterOptions()) -> tuple[Function, list[int]]: ...
```

- The kwargs pile (`function_max_size=`, `allow_code_before_start_addr=`,
  `compact=`, `per_address_ccs=`, `calls_clobber=`,
  `assume_distinct_sp_bases_disjoint=`, `alias_mode=`) is REMOVED — all of it
  moves into `LifterOptions` / `CfgOptions`. This is a breaking change; every
  call site migrates.
- `opts.pipeline`, when set, makes `analyze` run THAT pipeline (instead of the
  default) for this function only. Default `None` → the built-in default
  pipeline. No pipeline argument exists on `strider.lifter(...)`.
- Mutable default caveat: `CfgOptions()`/`LifterOptions()` as parameter
  defaults must not be shared-mutable Python instances. Use `None` sentinels
  and construct fresh inside, or make the opts types immutable (frozen) —
  implementer picks; the observable default behaviour is a fresh default-opts.

## optimize on the Lifter; drop reoptimize

- Add `Lifter.optimize(function, pipeline=None) -> None`: mutates `function`
  in place (matching the current `Function.optimize` behaviour); `pipeline=None`
  runs the Lifter's default pipeline.
- REMOVE `Function.optimize(pipeline)` and `Function.reoptimize()`. `reoptimize`
  was just "optimize with the default pipeline" — now `Lifter.optimize(function)`.

## Migration

- Every `analyze(entry, cc, function_max_size=…, …)` → `analyze(entry, cc,
  LifterOptions(cfg=CfgOptions(function_max_size=…), …))`.
- Every `build_cfg(entry, allow_code_before_start_addr=…)` →
  `build_cfg(entry, CfgOptions(allow_code_before_start_addr=…))`.
- Every `function.optimize(p)` / `function.reoptimize()` →
  `lifter.optimize(function, p)` / `lifter.optimize(function)`.
- Tests + examples + `.pyi` + README + CLAUDE.md updated in lockstep.

## Out of scope

Entry-replay p-code correctness for mode-switching arches
(`fingerprint_pcode`/`pcode_at` decoding from entry) is a SEPARATE follow-up,
not this one.

## Verification

`cargo test --workspace` (0 failed), `cargo clippy --workspace` clean, and
`uv run maturin develop && uv run pytest` (from workspace root) green, plus all
examples run end-to-end. Same gate as the redesign.
