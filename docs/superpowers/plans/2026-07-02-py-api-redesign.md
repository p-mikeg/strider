# strider-py API Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rework the `strider-py` Python surface to one lift handle, one result shape, single-source-of-truth reads, an explicit match/build split, and keyword-safe descriptive names.

**Architecture:** Mostly binding-layer (`crates/strider-py/src/*.rs` `#[pyclass]`/`#[pyfunction]` + `strider/*.pyi` stubs + `strider/_api.py`). Two tasks touch Rust core: making `cc` a by-value input moved into `Function` (drops a clone), and exposing the existing `strider_pattern::Template` to Python as a distinct type. Each `src/X.rs` owns a `register(py, m)` fn assembled in `src/lib.rs`.

**Tech Stack:** Rust + PyO3 0.22 (abi3-py39) + maturin + uv; pytest for the Python surface; `cargo test` for Rust core.

**Reference:** Spec at `docs/superpowers/specs/2026-07-02-py-api-redesign-design.md`. The current surface is fully described by `crates/strider-py/strider/__init__.pyi` / `pattern.pyi` / `opt.pyi`.

## Global Constraints

- PyO3 0.22, abi3, Python ≥ 3.9. No new Python runtime deps.
- Every Python-exposed name change is done via `#[pyo3(name = "...")]` (or `#[pyclass(name=...)]`), NOT by renaming the Rust type, unless the Rust type is also being restructured.
- The `.pyi` stubs are the authoritative contract — each task updates the relevant stub in lockstep with the binding.
- Breaking change; targets `develop`. Every example under `crates/strider-py/examples/python/` and every test under `crates/strider-py/tests/python/` is updated in the task that changes the API it uses.
- Verification per task: `cd crates/strider-py && uv run maturin develop && uv run pytest -q` green (plus `cargo test -p <crate>` for the two Rust-core tasks). Baselines before starting: `cargo test --workspace` 3245/0, `pytest` 873 passed / 1 skipped.
- Rust worker etiquette: `#[cfg(test)]` code is never production; keep clippy clean (`cargo clippy --workspace`).

---

## Task ordering & dependencies

1. **cc by-value into Function** (Rust core) — foundation for the handle change.
2. **One lift handle `Lifter`** — depends on 1.
3. **ElfLifter** — depends on 2.
4. **Sleigh-owning reads move to Lifter** — depends on 2.
5. **Reads SSoT on Node; Match forwarders** — independent of 2–4, do after 4 to avoid churn.
6. **Remove PartialMatch** — depends on 5 (shared read type).
7. **Template split** (Rust exposure + binding) — independent; can run parallel to 2–6.
8. **Naming renames** — last, mechanical, touches pattern/sleigh/reader/lib.

---

## Task 1: `cc` is a by-value input, moved into `Function` (no clone) — Rust core

**Files:**
- Modify: `crates/strider-ir/src/builder/mod.rs` (`FunctionBuilder::new`, ~line 164 `Function::new(cc.clone(), …)`)
- Modify: `crates/strider-ir/src/function/func.rs` (`Function::new` signature if it takes `&cc`)
- Modify: `crates/strider-lift/src/lift/function_lifter.rs` (`FunctionLifter::new` — takes `cc: &BuiltCallingConvention`; threads to `FunctionBuilder::new`)
- Modify: `crates/strider-lift/src/lift/mod.rs` (`build_ir` / `build_ir_with` — `cc` param)
- Test: existing `cargo test -p strider-ir -p strider-lift`

**Interfaces:**
- Produces: `FunctionBuilder::new(all_vns: Vec<Vn>, cc: BuiltCallingConvention, endianness) -> Result<FunctionBuilder>` (cc **by value**); `Function.default_cc` is the moved-in value.
- Consumes: the container-map build still needs `&cc` during construction — borrow it before the move.

- [ ] **Step 1: Read the current signatures** — `FunctionBuilder::new`, `Function::new`, `FunctionLifter::new`, and every caller (test fixtures pass `&cc`).

- [ ] **Step 2: Change `FunctionBuilder::new` to take `cc: BuiltCallingConvention` by value.** In the body, do all `&cc` reads (seeding ret/arg regs, container-map queries) first, then `Function::new(cc, endianness, tracked_vns)` — drop `.clone()`. If `Function::new` takes `&BuiltCallingConvention`, change it to own it too and store directly in `default_cc`.

- [ ] **Step 3: Thread by-value through the lifter.** `FunctionLifter::new` currently borrows `cc: &'a BuiltCallingConvention` and passes `cc.clone()` to the builder (function_lifter.rs). Keep the borrow for the container map, but pass an owned clone-at-the-boundary ONLY here — i.e. the single remaining clone is `cc.clone()` at the lifter entry, replacing the one inside `FunctionBuilder::new`. Net: one clone instead of two, and callers that already own a cc can hand ownership in.

  Note: full zero-clone requires `build_ir_with` to take `cc` by value; do that — change `build_ir`/`build_ir_with` to `cc: BuiltCallingConvention` and move it into `FunctionLifter::new`, which moves it into `FunctionBuilder::new`. The container map borrows it before the final move.

- [ ] **Step 4: Fix all callers** — orchestrator `Strider::analyze`, test fixtures (`RegisterSet`, `raw_builder`), strider-py `strider_cls.rs`. Compiler drives this; each call site now passes an owned `BuiltCallingConvention` (they mostly build one fresh already).

- [ ] **Step 5: Run Rust tests**

Run: `cargo test -p strider-ir -p strider-lift -p strider-orchestrator 2>&1 | grep "test result"`
Expected: all `ok`, 0 failed.

- [ ] **Step 6: Confirm the clone is gone**

Run: `grep -rn "cc.clone()" crates/strider-ir/src crates/strider-lift/src`
Expected: at most the single boundary clone in `function_lifter.rs` (or none if `build_ir_with` moved it in).

- [ ] **Step 7: Commit**

```bash
git add crates/strider-ir crates/strider-lift crates/strider-orchestrator
git commit -m "refactor(ir): take calling convention by value, move it into Function"
```

---

## Task 2: One lift handle — `strider.lifter(arch, mem, rom=None) -> Lifter`

**Files:**
- Modify: `crates/strider-py/src/strider_cls.rs` (the `Strider` pyclass → becomes the sole `Lifter`)
- Delete: `crates/strider-py/src/run.rs` (the `run()` fn + `RunResult`) and its `register` call in `src/lib.rs:103`
- Modify: `crates/strider-py/src/cfg.rs` (drop `Lifter` low-level class + `AnalyzeOutcome` if defined here; keep `Cfg`)
- Modify: `crates/strider-py/src/lib.rs` (registration list)
- Modify: `crates/strider-py/strider/__init__.pyi`, `strider/__init__.py`, `strider/_api.py`
- Test: `crates/strider-py/tests/python/` — every test constructing a run handle.

**Interfaces:**
- Produces (target `__init__.pyi`):

```python
def lifter(arch: SleighArch, mem: Any, rom: Optional[Any] = ...) -> Lifter:
    """Build a lift+optimise+resolve handle over a raw code reader
    (`BufferReader`/`MemReader`); `rom` backs `LoadReadOnly` folding."""

class Lifter:
    def build_cfg(self, entry: int, *, allow_code_before_start_addr: bool = ...,
                  function_max_size: Optional[int] = ...) -> Cfg: ...
    def analyze(self, entry: int, cc: CallingConvention, *,
                function_max_size: Optional[int] = ..., allow_code_before_start_addr: bool = ...,
                compact: bool = ..., per_address_ccs: Optional[dict] = ...,
                calls_clobber: bool = ..., assume_distinct_sp_bases_disjoint: bool = ...,
                alias_mode: str = ...) -> Tuple[Function, List[int]]: ...
    def fingerprint_pcode(self, node: Any) -> List[Tuple[int, str]]: ...   # from Task 4
    def dump_html(self, function: Function, path: str, style: Optional[str] = ...) -> None: ...  # Task 4
    def dump_dot(self, function: Function, path: str) -> None: ...          # Task 4
    def html_str(self, function: Function, style: Optional[str] = ...) -> str: ...  # Task 4
```

- Removed names: `strider.strider`, `strider.run`, `strider.Strider`, the old low-level `strider.Lifter`, `RunResult`, `AnalyzeOutcome`.

- [ ] **Step 1: Write/point a failing pytest** at the new entry point. In `tests/python/test_low_level.py` (or the existing run test), replace a `strider.strider(arch, cc, mem)` / `strider.run(...)` usage with:

```python
def test_lifter_analyze_returns_graph_and_unresolved():
    lift = strider.lifter(arch, mem)                 # no cc at construction
    graph, unresolved = lift.analyze(entry, cc)      # cc per call, tuple result
    assert graph.node_count() > 0
    assert isinstance(unresolved, list)
```

- [ ] **Step 2: Run it — expect failure** (`strider.lifter` missing / `analyze` arity).

Run: `cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/test_low_level.py -q`
Expected: FAIL.

- [ ] **Step 3: Rework `strider_cls.rs`.** Rename the pyclass to `Lifter` (`#[pyclass(name = "Lifter")]`), constructor `strider.lifter(arch, mem, rom=None)` (a `#[pyfunction(name = "lifter")]`, or `#[new]` on the class + keep the free fn as the documented constructor). Drop the stored `cc`; add `cc` as a required arg to `analyze`. Return `(Py<Function>, Vec<u64>)` instead of any wrapper. Add `build_cfg(entry, ...)` that stops after CFG construction (reuse the native `Lifter::build_cfg`).

- [ ] **Step 4: Delete `run.rs` and the old low-level `Lifter`.** Remove `run::register` from lib.rs; delete `RunResult`/`AnalyzeOutcome` pyclasses; keep `Cfg` in cfg.rs.

- [ ] **Step 5: Update `_api.py` / `__init__.py`** — drop the `strider`/`run` re-exports; export `lifter`, `Lifter`.

- [ ] **Step 6: Update the stub** `__init__.pyi` to the Interfaces block above (minus the Task-4 methods, added in Task 4).

- [ ] **Step 7: Update every affected pytest** to `strider.lifter(...).analyze(entry, cc)` returning a tuple.

- [ ] **Step 8: Run the suite**

Run: `cd crates/strider-py && uv run maturin develop && uv run pytest -q`
Expected: green (only the tests you migrated changed).

- [ ] **Step 9: Commit**

```bash
git add crates/strider-py
git commit -m "feat(py): single Lifter handle; drop strider()/run()/Strider/RunResult"
```

---

## Task 3: ELF loaders → `ElfLifter(Lifter)`

**Files:**
- Modify: `crates/strider-py/src/reader.rs` (ELF loading; expose segment vs section region selection)
- Modify: `crates/strider-py/src/strider_cls.rs` (add `ElfLifter` subclass via `#[pyclass(extends = Lifter)]`)
- Modify: `crates/strider-py/strider/_api.py` (`load_elf_from_segments` / `load_elf_from_sections` / `load_elf`)
- Modify: `crates/strider-py/strider/__init__.pyi`, `__init__.py`
- Test: `tests/python/test_high_level_api.py`, `tests/python/system/`

**Interfaces (target `__init__.pyi`):**

```python
class ElfLifter(Lifter):
    def symbols(self) -> dict[str, int]: ...
    def symbol(self, name: str) -> int: ...
    def symbol_size(self, name: str) -> Optional[int]: ...
    def entry_point(self) -> int: ...
    def functions(self) -> Iterable[str]: ...
    def analyze(self, target: str | int, cc: Optional[CallingConvention] = ..., **opts: Any) -> Tuple[Function, List[int]]: ...

def load_elf_from_segments(path: str, *, apply_relocations: bool = ..., arch: Optional[SleighArch] = ..., cc: Optional[CallingConvention] = ...) -> ElfLifter: ...
def load_elf_from_sections(path: str, *, apply_relocations: bool = ..., arch: Optional[SleighArch] = ..., cc: Optional[CallingConvention] = ...) -> ElfLifter: ...
def load_elf(path: str, **opts: Any) -> ElfLifter:
    """Convenience: delegates to load_elf_from_segments."""
```

- [ ] **Step 1: Failing pytest**

```python
def test_load_elf_from_segments_symbol_analyze():
    lift = strider.load_elf_from_segments(FIXTURE)      # ElfLifter
    assert isinstance(lift, strider.Lifter)             # is-a Lifter
    addr = lift.symbol("add")
    graph, unresolved = lift.analyze("add")             # by name, cc defaulted from ELF
    assert graph.node_count() > 0
```

- [ ] **Step 2: Run — expect failure.**

Run: `cd crates/strider-py && uv run pytest tests/python/test_high_level_api.py::test_load_elf_from_segments_symbol_analyze -q`
Expected: FAIL.

- [ ] **Step 3: Reader region selection.** In `reader.rs`, expose two region builders: segments (`PT_LOAD`, the existing `elf_get_loadable_regions_including_writable`-style path) and sections (section headers). Confirm the underlying `strider-reader` crate already has both (`elf_load_with_relocations` vs a section variant); if only one exists, add the section loader in `strider-reader` (out of scope if absent — flag to owner).

- [ ] **Step 4: `ElfLifter` pyclass** `#[pyclass(extends = Lifter, name = "ElfLifter")]` holding the ELF symbol backend; `#[new]`-free (constructed by the loader fns). Its `analyze(target)` resolves `str`→addr via `symbol()` then calls the base `Lifter::analyze`; `int` target passes through. `cc` defaults to the ELF-derived CC when `None`.

- [ ] **Step 5: `_api.py`** — implement the three loader fns; `load_elf` calls `load_elf_from_segments`.

- [ ] **Step 6: Stub + `__init__.py`** exports updated.

- [ ] **Step 7: Migrate `test_high_level_api.py` / `system/`** from `load_elf(...).analyze(name)` (old `Analysis`) to the tuple return; symbol/entry_point calls unchanged.

- [ ] **Step 8: Run suite** — `uv run maturin develop && uv run pytest -q` green.

- [ ] **Step 9: Commit** — `git commit -m "feat(py): ElfLifter(Lifter) + load_elf_from_segments/sections"`.

---

## Task 4: Sleigh-owning reads move to `Lifter`; Sleigh-free reads stay

**Files:**
- Modify: `crates/strider-py/src/function.rs` (drop pretty dumps `to_html`/`to_dot`/`html_str`; keep `raw_*`)
- Modify: `crates/strider-py/src/strider_cls.rs` (add `fingerprint_pcode`, `dump_html`, `dump_dot`, `html_str` taking a `Function`)
- Modify: stubs
- Delete: `Analysis` wrapper (`_api.py`) — its `find`/`fingerprint`/`dump_*` move to `Function`/`Lifter`
- Test: `tests/python/test_dot.py` (or equivalent), fingerprint tests

**Interfaces:** `Function` keeps `raw_dot_str`/`raw_html_str`/`to_raw_dot`/`to_raw_html` (no Sleigh). `Lifter` gains `dump_html(function, path, style=None)`, `dump_dot(function, path)`, `html_str(function, style=None)`, `fingerprint_pcode(node)` (all need Sleigh, which the Lifter owns).

- [ ] **Step 1: Failing pytest**

```python
def test_pretty_dump_on_lifter():
    lift = strider.load_elf_from_segments(FIXTURE)
    graph, _ = lift.analyze("add")
    html = lift.html_str(graph)            # pretty render lives on the Lifter
    assert "add" in html or len(html) > 0
    assert not hasattr(graph, "html_str")  # removed from Function
```

- [ ] **Step 2: Run — expect failure.**

- [ ] **Step 3:** Move pretty-dump methods from `function.rs` to `strider_cls.rs` (they call the native `FunctionDotDumper` which needs `&Sleigh` — the Lifter has it). `fingerprint_pcode(node)` likewise moves from `Analysis` to `Lifter`.

- [ ] **Step 4:** Delete the `Analysis` class; its `find`/`find_one`/`find_joined` were already `Function` methods, `fingerprint` (addr-only) stays as `Node.fingerprint()`/`Function` (Task 5), `fingerprint_pcode`/`dump_*` are now on `Lifter`.

- [ ] **Step 5:** Update stubs (remove `Analysis`, `Function.to_html/html_str/to_dot`; add `Lifter` dump methods).

- [ ] **Step 6:** Migrate dot/fingerprint tests.

- [ ] **Step 7: Run suite** green. **Commit** — `git commit -m "feat(py): Sleigh-needing renders/provenance on Lifter; drop Analysis"`.

---

## Task 5: Reads are SSoT on `Node`; `Function` loses id-keyed readers; `Match` forwards

**Files:**
- Modify: `crates/strider-py/src/function.rs` (delete `node_kind(id)`, `asm_fingerprint(id)`, `call_other_name(id)`, `wide_const_bytes(id)`)
- Modify: `crates/strider-py/src/node.rs` (ensure `Node` has `kind/inputs/const_int/const_uint/const_bool/fingerprint/wide_const_bytes/call_other_name` + op readers)
- Modify: `crates/strider-py/src/matcher.rs` (`Match`: keep `root`, `node(key)->Node`, `has/[]/in`; make value/op readers thin forwarders to `node(key)`)
- Modify: stubs
- Test: `tests/python/test_node.py`, pattern match tests reading captures

**Interfaces:** `Node` is the single home for per-node facts. `Match.uint(k)` ≡ `Match.node(k).const_uint()`, `Match.int_binary_op(k)` ≡ `Match.node(k).int_binary_op()`, etc. — one implementation on `Node`, forwarders on `Match`.

- [ ] **Step 1: Failing pytest**

```python
def test_reads_are_on_node():
    lift = strider.load_elf_from_segments(FIXTURE)
    g, _ = lift.analyze("add")
    nid = g.node_ids()[0]
    n = g.node(nid)
    assert isinstance(n.kind(), str)
    assert not hasattr(g, "node_kind")     # removed the id-keyed duplicate
```

- [ ] **Step 2: Run — expect failure.**

- [ ] **Step 3:** Delete the four id-keyed reader methods from `function.rs`. Add any missing readers to `node.rs` (`const_uint` alongside `const_int`; op-variant readers `int_binary_op`/`int_cmp_op`/… migrated off `Match`).

- [ ] **Step 4:** In `matcher.rs`, reduce `Match` to `root` + `node(key)` + membership, and implement `uint/int/bool/float_bits/vn/int_binary_op/...` as one-liners delegating to `self.node(key)?.<reader>()`.

- [ ] **Step 5:** Update stubs (`Function` loses the four id readers; `Node` gains `const_uint`, op readers; `Match` documents forwarders).

- [ ] **Step 6:** Migrate tests that used `function.node_kind(id)` → `function.node(id).kind()`.

- [ ] **Step 7: Run suite** green. **Commit** — `git commit -m "refactor(py): reads are single-source on Node; Match forwards"`.

---

## Task 6: Remove `PartialMatch`; `.when` receives a `Match`

**Files:**
- Modify: `crates/strider-py/src/matcher.rs` (delete `PartialMatch`)
- Modify: `crates/strider-py/src/pattern.rs` (`.when` predicate now called with a `Match`)
- Modify: `strider/pattern.pyi`
- Test: pattern tests using `.when(...)`

**Interfaces:** `.when(f: Callable[[Match], bool])`. A `Match` returns `None`/`False` for keys unbound at predicate time (already its semantics), so no partial type is needed.

- [ ] **Step 1: Failing pytest** — a `.when` predicate that reads a bound capture and asserts the callback arg is a `Match`:

```python
def test_when_receives_match():
    seen = {}
    c = pat.Capture()
    p = pat.add(pat.any_int_const(c), pat.anything()).when(lambda m: (seen.setdefault("t", type(m)), True)[1])
    g.find_all(p)
    assert seen["t"].__name__ == "Match"
```

- [ ] **Step 2: Run — expect failure** (callback gets a `PartialMatch`).

- [ ] **Step 3:** In `matcher.rs`/`pattern.rs`, pass the in-progress `Match` to the predicate; delete the `PartialMatch` pyclass and its registration.

- [ ] **Step 4:** Update `pattern.pyi` (remove `PartialMatch`; `.when` signatures reference `Match`).

- [ ] **Step 5:** Migrate `.when` tests.

- [ ] **Step 6: Run suite** green. **Commit** — `git commit -m "refactor(py): drop PartialMatch; .when predicate receives Match"`.

---

## Task 7: Explicit match/build split — `strider.template` + typed `Template`

**Files:**
- Modify: `crates/strider-py/src/pattern.rs` (add a `Template` pyclass wrapping `DynTemplate`; a `strider.template` submodule registration)
- Modify: `crates/strider-py/src/function.rs` (`rewrite(find, replace)` accepts `Template` for `replace`; `rewrite_all` pairs)
- Modify: `crates/strider-py/src/lib.rs` (register `template` submodule)
- Create: `crates/strider-py/strider/template.pyi`
- Modify: `strider/pattern.pyi`, `strider/__init__.py` (hoist `template` like `pattern`)
- Test: `tests/python/test_function_rewrite.py`

**Interfaces:** `strider.template` exposes build-side constructors returning `Template` (the build-valid subset: node/op/const builders + `var(capture)`, NO `.when`/commutativity). `Function.rewrite(find: PatLike, replace: Template) -> int`. Keep back-compat acceptance of a `Pat` as `replace` during migration ONLY if trivial; otherwise require `Template`.

- [ ] **Step 1: Failing pytest**

```python
from strider import pattern as pat, template as tpl
def test_rewrite_takes_template():
    c = pat.Capture()
    n = g.rewrite(find=pat.add(pat.var(c), pat.int_const(0)), replace=tpl.var(c))
    assert isinstance(n, int)
```

- [ ] **Step 2: Run — expect failure** (`strider.template` missing).

- [ ] **Step 3:** Add a `Template` pyclass in `pattern.rs` wrapping the existing `DynTemplate` compile path (`compile_operand_template`). Add `template` module builders mirroring the build-valid constructors (reuse the `DynTemplate` factories already there). Register a `template` submodule in `lib.rs` and hoist it in `__init__.py` (same pattern as `pattern`/`opt`).

- [ ] **Step 4:** `rewrite`/`rewrite_all` type the `replace` arg as `Template`.

- [ ] **Step 5:** Write `template.pyi` (the build constructors + `Template`); trim build-only affordances from `pattern.pyi`'s notion of "replace".

- [ ] **Step 6:** Migrate `test_function_rewrite.py` to `tpl.*` on the replace side.

- [ ] **Step 7: Run suite** green. **Commit** — `git commit -m "feat(py): explicit strider.template build DSL; typed rewrite(replace: Template)"`.

---

## Task 8: Descriptive renames (no keyword underscores)

**Files:**
- Modify: `crates/strider-py/src/pattern.rs` (`#[pyo3(name = ...)]` on the op constructors)
- Modify: `crates/strider-py/src/sleigh.rs` or `reader.rs` (`VnSpace.const_` → `const`)
- Modify: `crates/strider-py/src/lib.rs` (already `strider.lifter` from Task 2)
- Modify: `strider/pattern.pyi`, `strider/__init__.pyi`
- Test: every pattern test using the renamed constructors; grep-driven codemod of examples/tests

**Rename table (exact):**

| old Python name | new Python name |
|---|---|
| `and_` | `int_and` |
| `or_` | `int_or` |
| `xor` | `int_xor` |
| `not_` and `bit_not` | `int_not` (single constructor; `~x`) |
| `if_` | `if_else` |
| `VnSpace.const_` | `VnSpace.const` |
| `any_` | `anything` |
| `strider.strider` | `strider.lifter` (done in Task 2) |

`bool_and`/`bool_or`/`bool_xor`/`bool_not` unchanged.

- [ ] **Step 1: Failing pytest** asserting the new names exist and the old are gone:

```python
def test_renamed_constructors():
    from strider import pattern as pat
    assert hasattr(pat, "int_and") and hasattr(pat, "int_not") and hasattr(pat, "if_else") and hasattr(pat, "anything")
    for gone in ("and_", "or_", "xor", "not_", "bit_not", "if_", "any_"):
        assert not hasattr(pat, gone), gone
```

- [ ] **Step 2: Run — expect failure.**

- [ ] **Step 3:** Change the `#[pyo3(name=...)]` on each constructor in `pattern.rs` per the table (the Rust fn names may stay; only the exported name changes). Collapse `not_`+`bit_not` into one `int_not` (delete the alias). Rename `VnSpace.const_` exported method to `const`. Rename `any_` → `anything`.

- [ ] **Step 4: Codemod examples + tests.** Run a workspace-wide replace over `crates/strider-py/tests/python/` and `crates/strider-py/examples/python/`:

```bash
cd crates/strider-py
grep -rl -E "\b(and_|or_|xor|not_|bit_not|if_|any_)\b" tests examples | while read f; do
  sed -i -E 's/\band_\b/int_and/g; s/\bor_\b/int_or/g; s/\bxor\b/int_xor/g; s/\b(not_|bit_not)\b/int_not/g; s/\bif_\b/if_else/g; s/\bany_\b/anything/g' "$f"
done
```
Then hand-audit: `xor`/`and_` must not have clobbered unrelated identifiers (e.g. Python `or`/`and` keywords are safe — different token; `xor` as a bare word only appears as the constructor). Review the diff.

- [ ] **Step 5:** Update `pattern.pyi` / `__init__.pyi` names.

- [ ] **Step 6: Run suite** green. **Commit** — `git commit -m "refactor(py): descriptive constructor names (int_and/or/xor/not, if_else, anything, const)"`.

---

## Task 9: Regenerate stubs, run examples, final gate

**Files:**
- Modify: all `strider/*.pyi` (final consistency pass)
- Modify: `crates/strider-py/examples/python/*.py` (any missed by codemods)
- Modify: `crates/strider-py/README.md` if it shows the old API

- [ ] **Step 1:** Grep the whole crate for any surviving old names:

```bash
cd crates/strider-py
grep -rn -E "\b(strider\.strider|strider\.run|RunResult|AnalyzeOutcome|PartialMatch|\.node_kind\(|load_elf\b\()" strider examples tests README.md | grep -v "load_elf_from_"
```
Expected: only intended `load_elf(` convenience calls remain; everything else gone.

- [ ] **Step 2:** Run each example end-to-end (they read fixtures under `fixtures/out/`):

```bash
cd crates/strider-py && for ex in examples/python/*.py; do echo "== $ex =="; uv run python "$ex" || break; done
```
Expected: each runs clean.

- [ ] **Step 3: Full gate**

```bash
cd /mnt/c/Users/mikeg/Documents/strider
cargo test --workspace 2>&1 | grep -E "test result: FAILED|FAILED" || echo "cargo OK"
cargo clippy --workspace 2>&1 | grep -E "warning:|error" | grep -v generated || echo "clippy clean"
cd crates/strider-py && uv run maturin develop && uv run pytest -q
```
Expected: cargo 0 failed, clippy clean, pytest green.

- [ ] **Step 4: Commit** — `git commit -m "docs(py): stubs/examples/README to the redesigned API; final gate"`.

---

## Self-review notes (author)

- **Spec coverage:** #1→T1, #2→T2, #3→T3, #4→T4, #5→T7, #6→T5, #7→T6, #8→T8, stubs/examples→T9. All eight covered.
- **Rust-core risk:** T1 and the section-loader in T3 (`strider-reader`) may need core changes; T3 Step 3 flags the case where a section loader doesn't yet exist.
- **Ordering:** T5 before T6 (shared read type); T2 before T3/T4 (handle exists first); T7 independent.
- **Migration reality:** most "tests" are edits of existing pytest to the new surface — each task migrates only the tests for the API it changes; T9 is the safety net grep + example run.
