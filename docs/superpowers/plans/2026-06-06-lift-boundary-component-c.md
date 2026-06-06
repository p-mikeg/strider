# Component C — Python `ElfStrider` / `strider()` API reshape — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Replace the Python high-level API (`load`→`Program`, `Analyzer`, the `_LoadedElf` class) with `strider.load_elf(path) -> ElfStrider` (= run-handle + ELF symbol logic) and `strider.strider(arch, cc, mem, rom=None) -> Strider` (standalone run handle), built on the Rust `Strider` from Component B.

**Architecture:** This is a public-Python-API reshape, NOT behavior-preserving — the tests change to match the new API. `Program` already has the `ElfStrider` shape (symbol/symbols/symbol_size/entry_point/read/functions/pcode/analyze), so C is largely a rename+restructure of `_api.py` plus thin Rust additions, then a pytest/stub/examples migration. pytest staying green (after migration) is the acceptance gate.

**Naming resolution (the one real design decision — pinned here):**
- The **run handle** takes the name **`Strider`** (matches the spec's `strider.strider() -> Strider`). It wraps the Rust `strider_orchestrator::Strider<AnyMemReader>` and exposes `analyze(entry, *, cc=None, **opts) -> Function` (full lift+optimize+resolve).
- The existing low-level **lift handle** (today's `strider.Strider` / Rust `PyStrider`, with `analyze_cfg` + `build_optimizer_pipeline`) is **renamed to `Lifter`** — it keeps its niche "lift one CFG, no resolution" role under a name that no longer collides.
- **`ElfStrider`** = a `Strider` + the ELF symbol table; `strider.load_elf(path) -> ElfStrider`.
- **`Analyzer`** (the configure-once-analyze-many handle) is **removed** — that role is just "hold an `ElfStrider`/`Strider` and call `analyze` repeatedly." Its `pipeline_factory` knob is dropped (the default pipeline is built internally; the custom-`pipeline=` path stays on `strider.run`).
- **`_LoadedElf`** is **no longer a public Python class** — its Rust ELF-parse/symbol backend stays (internal), owned by `ElfStrider`. `strider.load` (old) is removed; `strider.load_elf` is the entry.
- **`strider.run(...)`** (the one-shot pyfunction) stays unchanged.

**Tech Stack:** PyO3 + maturin; `cargo build/clippy --workspace`, `uv run maturin develop && uv run pytest`.

**Gate (every task):** `cargo build --workspace` + `clippy --workspace --all-targets` clean; final task `cargo test --workspace` 0 new failures + `uv run maturin develop && uv run pytest` all pass. Commit per task (trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`); push at the end.

Reference spec: `docs/superpowers/specs/2026-06-06-orchestrator-lift-boundary-design.md` (Component C).

---

## Task C1: Rust — `strider.strider()` + `Strider` pyclass; rename lift handle to `Lifter`

**Files:** `crates/strider-py/src/strider_cls.rs`, `crates/strider-py/src/run.rs` (reuse its `Strider` wiring), `crates/strider-py/src/reader.rs` (`_LoadedElf` → internal), `crates/strider-py/src/lib.rs` (module registration).

- [ ] **Step 1:** Add a Rust pyclass `PyStriderRun` exposed as `#[pyclass(name = "Strider")]` wrapping `strider_orchestrator::Strider<AnyMemReader>` + a constructor pyfunction `strider.strider(arch, cc, mem, rom=None) -> Strider`. Method `analyze(&mut self, entry: u64, *, function_max_size=None, allow_code_before_start_addr=False, compact=True, per_address_ccs=None) -> Function` mapping onto `Strider::analyze(entry, &cc, &LiftOptions{..}, &OptOptions{..})`. The `cc` is fixed at construction (the standalone handle is bound to one default cc; per-address overrides via the arg). Reuse the arg→`LiftOptions`/`OptOptions` mapping already written in `run.rs`'s `run_via_orchestrator`.
- [ ] **Step 2:** Rename the existing `PyStrider` (the `analyze_cfg`/`build_optimizer_pipeline` lift handle) Python name from `"Strider"` to `"Lifter"` (keep the Rust struct name or rename to `PyLifter`; update `lib.rs` `m.add_class`, the `register` fn, and any internal constructor users like `run.rs`'s `_strider_obj` early-CC-resolution check).
- [ ] **Step 3:** Make `_LoadedElf` non-public: remove its `m.add_class::<PyLoadedElf>()` registration (keep the struct + `load_elf` Rust fn for internal use by the Python `ElfStrider`, OR expose a private `strider._load_elf` the Python layer calls). Decide the minimal seam so `_api.py`'s `ElfStrider` can still get symbols/read/entry_point from the Rust ELF backend without a public `_LoadedElf` class.
- [ ] **Step 4:** `cargo build --workspace` + `clippy --workspace --all-targets` clean.
- [ ] **Step 5:** Commit: `feat(strider-py): Strider run pyclass + strider() ctor; rename lift handle to Lifter`.

---

## Task C2: Python — `load_elf -> ElfStrider`; remove `Program`/`Analyzer`/`load`

**Files:** `crates/strider-py/strider/_api.py`, `crates/strider-py/strider/__init__.py`.

- [ ] **Step 1:** Rename `class Program` → `class ElfStrider`. It holds a `Strider` (the run handle from C1, constructed from the ELF's memory + arch + cc) plus the ELF symbol backend. Keep the methods: `arch`/`cc` properties, `functions()`, `symbol(name)`, `symbol_size(name)`, `symbols()`, `entry_point()`, `read(addr,size)`, `add_elf(path)`, `pcode(addr,count)`, and `analyze(fn_name_or_addr, **opts) -> Analysis`. `analyze` resolves a symbol name → addr (or takes an addr) then delegates to the inner `Strider.analyze`.
- [ ] **Step 2:** Rename `def load(...)` → `def load_elf(path, *, apply_relocations=True, arch=None, cc=None) -> ElfStrider`. Remove the old `Analyzer` class and the `analyzer(...)` method/function — fold their role into `ElfStrider`/`Strider` (analyze-many = call `analyze` repeatedly; drop `pipeline_factory`). Add `def strider(arch, cc, mem, rom=None) -> Strider` as a thin Python wrapper over the Rust `strider.strider()` if any Python-side convenience is needed (else re-export the Rust one).
- [ ] **Step 3:** Update `__init__.py` exports: drop `load`/`Program`/`Analyzer`/`analyzer`/`_LoadedElf`; add `load_elf`/`ElfStrider`/`strider`/`Strider`/`Lifter`.
- [ ] **Step 4:** `cargo build --workspace` + the wheel (`uv run maturin develop`) build clean (pytest will fail until C3 — that's expected; just confirm the wheel imports).
- [ ] **Step 5:** Commit: `feat(strider-py): ElfStrider + load_elf; remove Program/Analyzer/load`.

---

## Task C3: Migrate pytest + .pyi + examples + README + public-API snapshot

**Files:** `crates/strider-py/tests/python/**` (~30 files), `crates/strider-py/strider/__init__.pyi`, `crates/strider-py/examples/python/*.py`, `crates/strider-py/README.md`, `crates/strider-py/tests/python/test_public_api_snapshot.py`.

- [ ] **Step 1:** Migrate every pytest using `strider.load`/`Program`/`Analyzer`/`analyzer`/`_LoadedElf`/the old `Strider` to the new surface: `strider.load_elf(...) -> ElfStrider`, `strider.strider(...) -> Strider`, `Lifter` for the low-level `analyze_cfg` tests. Preserve each test's intent. `grep -rln "\.load(\|Program\|Analyzer\|analyzer\|_LoadedElf" tests/python/` to find them all.
- [ ] **Step 2:** Update `__init__.pyi` to the new classes/functions (`ElfStrider`, `Strider`, `Lifter`, `load_elf`, `strider`; drop `Program`/`Analyzer`/`load`/`_LoadedElf`).
- [ ] **Step 3:** Update `test_public_api_snapshot.py` to the new public surface (it pins the exported names — regenerate its expected set).
- [ ] **Step 4:** Update the examples (`examples/python/*.py`) and `README.md` to `load_elf`/`ElfStrider`/`strider`.
- [ ] **Step 5:** Full gate: `cargo test --workspace` 0 new failures; `clippy --workspace --all-targets` clean; `uv run maturin develop && uv run pytest -q` ALL pass.
- [ ] **Step 6:** Commit: `test(strider-py): migrate suite to ElfStrider/strider/Lifter`. Push `feature/orchestrator-lift-boundary`.

---

## Self-review notes
- **Spec coverage:** load_elf→ElfStrider → C2; strider()→Strider + run pyclass → C1; remove _LoadedElf/Program/Analyzer/load → C1/C2; no trait → ElfStrider is a concrete Python class holding a Strider. All covered.
- **Naming:** the `Strider` collision is resolved (run handle = `Strider`; old lift handle = `Lifter`) — pinned above; committed in this plan for visibility.
- **Risk:** C is a public-API reshape; pytest is the only behavioral oracle (tests rewritten to match), so C3's migration must preserve each test's *intent*, not just compile. The `test_public_api_snapshot` regen makes the new surface explicit.
- **Out of scope:** the develop merges (indirect-branch → develop, then orchestrator-lift-boundary → develop) are a separate finishing step after C.
