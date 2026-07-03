# strider-py opts structs + custom pipelines + optimize consolidation — Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Replace `analyze`/`build_cfg` kwargs with `LifterOptions`/`CfgOptions` structs (nested, mirroring Rust), add a per-function pipeline override, move `optimize` onto the `Lifter`, delete `reoptimize`.

**Spec:** `docs/superpowers/specs/2026-07-03-py-opts-pipelines-design.md`.

**Architecture:** Binding-only (`crates/strider-py/src/*.rs` + stubs + tests/examples). Rust `Strider::analyze` already accepts `Option<OptimizerPipeline>` — no core change.

## Global Constraints

- PyO3 0.22 / abi3 / py≥3.9. Build + test from WORKSPACE ROOT: `cd /mnt/c/Users/mikeg/Documents/strider && uv run maturin develop && uv run pytest crates/strider-py/tests/python -q` (building from crates/strider-py leaves the imported .so STALE). Run pytest to COMPLETION in the foreground before reporting.
- Nested opts (exact Rust mirror): `LifterOptions(cfg=CfgOptions(...), ...)`. One `CfgOptions` type, reused by `build_cfg` and nested in `LifterOptions`.
- Per-function pipeline only (in `LifterOptions`/`optimize`), NEVER on `strider.lifter(...)`.
- Mutable-default safety: don't share a mutable default opts instance across calls (None sentinel + fresh construct, or frozen types).
- `.pyi` in lockstep. Clippy clean. Current pytest baseline 874 passed / 2 skipped.

---

## Task 1: `CfgOptions` + `LifterOptions` structs; rewire `build_cfg`/`analyze`

**Files:** `crates/strider-py/src/strider_cls.rs` (Lifter build_cfg/analyze), a new `src/options.rs` (or in strider_cls.rs) for the two pyclasses, `src/lib.rs` (register), `strider/__init__.pyi`, `strider/_api.py` (ElfLifter.analyze), tests/examples using analyze/build_cfg.

**Interfaces (target):**
```python
class CfgOptions:
    def __init__(self, *, function_max_size: int|None = None, allow_code_before_start_addr: bool = False) -> None: ...
class LifterOptions:
    def __init__(self, *, cfg: CfgOptions = ..., compact: bool = True, per_address_ccs: dict|None = None,
                 calls_clobber: bool = False, assume_distinct_sp_bases_disjoint: bool = False,
                 alias_mode: str = "stack_global_disjoint", pipeline: OptimizerPipeline|None = None) -> None: ...
# Lifter.build_cfg(entry, opts: CfgOptions = CfgOptions()) -> Cfg
# Lifter.analyze(entry, cc, opts: LifterOptions = LifterOptions()) -> (Function, list[int])
# ElfLifter.analyze(target, cc=None, opts: LifterOptions = LifterOptions()) -> (Function, list[int])
```

- [ ] **Step 1 (RED):** Add a failing pytest in `tests/python/test_opts_structs.py`:
```python
def test_analyze_takes_lifter_options():
    lift = strider.load_elf_from_segments(FIXTURE)
    g, unresolved = lift.analyze("add", opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=4096)))
    assert g.node_count() > 0
def test_build_cfg_takes_cfg_options():
    lift = strider.load_elf_from_segments(FIXTURE)
    cfg = lift.build_cfg(lift.symbol("add"), strider.CfgOptions(allow_code_before_start_addr=True))
    assert cfg is not None
```
- [ ] **Step 2:** Run — expect failure (`LifterOptions`/`CfgOptions` missing).
- [ ] **Step 3:** Add `PyCfgOptions` (name `CfgOptions`) and `PyLifterOptions` (name `LifterOptions`) pyclasses with the fields above (getters; a `#[new]` with keyword-only signature and the defaults). Register both in `lib.rs`.
- [ ] **Step 4:** Change `Lifter.build_cfg` to `(entry, opts=CfgOptions())` and `Lifter.analyze` / `ElfLifter.analyze` to `(entry|target, cc[, opts=LifterOptions()])`. Internally unpack the struct into the existing `Strider::build_cfg`/`analyze` call (mapping `cfg.function_max_size`→fn_max_size, etc.), and pass `opts.pipeline` as the `Option<OptimizerPipeline>` arg to `Strider::analyze` (the plumbing exists). Remove the old kwargs.
- [ ] **Step 5:** Update `strider/__init__.pyi` (add the two classes; change build_cfg/analyze/ElfLifter.analyze signatures). Migrate `_api.py`'s ElfLifter.analyze to forward `opts`.
- [ ] **Step 6:** Migrate every analyze/build_cfg call site in `tests/python/` + `examples/python/` from kwargs to the structs. (grep `function_max_size=|allow_code_before_start_addr=|per_address_ccs=|calls_clobber=|assume_distinct_sp_bases_disjoint=|alias_mode=|compact=` under those dirs.)
- [ ] **Step 7:** Add a pytest proving the pipeline override runs a custom pipeline (e.g. an empty pipeline leaves more nodes than the default): `analyze(..., opts=LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()))` vs default — assert node counts differ (or a pass that the default folds is absent).
- [ ] **Step 8:** Gate green (ROOT build, foreground pytest). Commit `feat(py): LifterOptions/CfgOptions structs replace analyze/build_cfg kwargs; per-function pipeline`.

---

## Task 2: `optimize` on `Lifter`; delete `reoptimize`; docs + final gate

**Files:** `crates/strider-py/src/strider_cls.rs` (add optimize), `src/function.rs` (remove optimize/reoptimize), stubs, `CLAUDE.md`, `README.md`, tests/examples using optimize/reoptimize.

**Interfaces:** `Lifter.optimize(function, pipeline=None) -> None` (mutates in place; None=default pipeline). `Function.optimize`/`Function.reoptimize` removed.

- [ ] **Step 1 (RED):** Failing pytest:
```python
def test_optimize_on_lifter_mutates():
    lift = strider.load_elf_from_segments(FIXTURE)
    g, _ = lift.analyze("add", opts=strider.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()))
    before = g.node_count()
    lift.optimize(g)                    # default pipeline, in place
    assert g.node_count() <= before
    assert not hasattr(g, "optimize") and not hasattr(g, "reoptimize")
```
- [ ] **Step 2:** Run — expect failure.
- [ ] **Step 3:** Add `Lifter.optimize(function, pipeline=None)` in `strider_cls.rs` — build the default pipeline when `pipeline is None`, run it in place on the `PyFunction` (reuse the logic currently in `Function.optimize`). Delete `PyFunction::optimize` and `PyFunction::reoptimize` from `function.rs`.
- [ ] **Step 4:** Update `strider/__init__.pyi` (add `Lifter.optimize`; drop `Function.optimize`/`reoptimize`). Migrate every `function.optimize(p)`/`function.reoptimize()` call site → `lifter.optimize(function, p)` / `lifter.optimize(function)`.
- [ ] **Step 5:** Docs: update `crates/strider-py/README.md` and the `CLAUDE.md` strider-py section for the opts structs + optimize-on-Lifter (and any lingering kwargs/reoptimize mentions).
- [ ] **Step 6 (final gate):**
```
cd /mnt/c/Users/mikeg/Documents/strider
cargo test --workspace 2>&1 | grep -E "test result: FAILED|FAILED" || echo cargo-OK
cargo clippy --workspace --all-targets 2>&1 | grep -E "warning:|error" | grep -v generated || echo clippy-clean
uv run maturin develop && uv run pytest crates/strider-py/tests/python -q
for ex in crates/strider-py/examples/python/*.py; do uv run python "$ex" >/dev/null || echo "FAILED: $ex"; done
```
Expected: cargo 0 failed, clippy clean, pytest green (count shifts with new tests), examples all pass.
- [ ] **Step 7:** Commit `feat(py): optimize moves to Lifter; drop reoptimize; docs`.

---

## Self-review

- Spec goals: opts structs (T1), per-function pipeline (T1 Step 7), optimize-on-Lifter + drop reoptimize (T2) — all covered.
- Migration is real (Steps T1.6, T2.4) — kwargs and optimize call sites both swept.
- Rust `Strider::analyze` pipeline arg already exists (verified) — T1.4 just threads it.
