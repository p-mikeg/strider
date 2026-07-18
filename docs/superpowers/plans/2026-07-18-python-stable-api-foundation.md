# Python Stable API — Plan 1: Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganize the `strider` Python package into a fully namespaced, single-home stable surface and clean every weird/inconsistent public name, updating all internal callers in the same pass.

**Architecture:** Native `#[pyclass]`es are registered on per-domain native submodules (`ir`/`lift`/`cfg`/`sleigh`/`reader`, mirroring the existing `opt`/`pattern`/`template` pattern). The cdylib is renamed `strider._strider` to kill the `strider.strider` self-import leak. The pure-Python facade exposes only `StriderError` + the domain submodules. Renames/collapses delete old names outright (no shims). This is the *first* stabilization, so breaking is free.

**Tech Stack:** Rust + PyO3 0.22 (crate `strider-py`), maturin (workspace-root `pyproject.toml`), pyo3-stub-gen for `.pyi`, pytest.

## Global Constraints

- **One home per symbol. Zero re-exports.** Each public name reachable by exactly one path. Sole top-level resident: `StriderError`.
- **Break freely; no compatibility shims/aliases.** Delete old spellings; update callers in the same change.
- **No O() regressions.**
- **Rebuild before every pytest run:** `uv run maturin develop` from the **workspace root** (`/mnt/c/Users/mikeg/Documents/strider`), never from `crates/strider-py` (silently builds the wrong package). Verify the built `.so` mtime is newer than the source before trusting a green run.
- **Namespace layout (target):** `strider.ir` {Function, Node} · `strider.lift` {Lifter, ElfLifter, LifterOptions, load_elf} · `strider.cfg` {Cfg, CfgOptions} · `strider.sleigh` {SleighArch, CallingConvention, Sleigh, Vn, VnSpace} · `strider.reader` {BufferReader, MemReader, ReadOnlyMemory} · `strider.opt` {OptimizerPipeline, passes} · `strider.pattern` {Pat, Capture, Match, builders, .constraints} · `strider.template` {Template, builders} · `strider` {StriderError}.
- **Full gate before merge:** `cargo test --workspace`, `cargo clippy --workspace`, `cargo fmt --check`, `cargo doc`, and `pytest` all green.

---

## File structure

Rust bindings (`crates/strider-py/src/`):
- `lib.rs` — `#[pymodule]` entry; renamed `strider`→`_strider`; builds domain submodules and routes each `register` into its submodule.
- `errors.rs` — `StriderError` registered on the top-level module only (drop `errors` submodule).
- `sleigh.rs` — `VnSpace` space constants; `module="strider.sleigh"`.
- `arch.rs` / `cc.rs` — registered into the `sleigh` submodule.
- `function.rs` / `node.rs` — registered into the `ir` submodule; `node.rs` `fingerprint`→`asm_fingerprint`.
- `options.rs` — split so `CfgOptions`→`cfg`, `LifterOptions`→`lift`.
- `cfg.rs` — registered into `cfg`; `block_at`→`region_at`; viz methods → `to_dot`/`to_html`; `region_texts`→`_region_texts`.
- `strider_cls.rs` — registered into `lift`; `dump_*`/`html_str`→`to_dot`/`to_html`.
- `reader.rs` — registered into `reader`; `load_elf_from_*` pyfunctions collapse to one `load_elf`.
- `opt.rs` — `OptimizerPipeline` into `opt` submodule; `pass_count`/`post_pass_count`→`passes`/`post_passes`.
- `matcher.rs` — `Match` into `pattern` submodule; `int`/`bool`/`uint`→`const_int`/`const_bool`/`const_uint`.

Rust opt crate:
- `crates/strider-opt/src/pipeline.rs` — add default `name()` to `Optimizer`/`PostOptimizer`.

Python facade (`crates/strider-py/strider/`):
- `__init__.py` — expose `StriderError` + domain submodules; `del` leaks; `__all__`.
- `_api.py` — import from `strider._strider`; collapse `load_elf`; bind `ElfLifter`/`load_elf` into `strider.lift`.
- `*.pyi` — regenerated + reconciled to the new tree.

Config:
- workspace-root `pyproject.toml` — `module-name = "strider._strider"`.

Tests: `crates/strider-py/tests/python/` — import/name updates across the suite; new assertions for each behavior change.

---

## Phase A — Rust trait + leaf behavior changes (no namespace moves yet)

These land first because they are pure behavior, independently testable, and unaffected by the later reorg. Tests are Rust unit tests where the change is Rust-only, pytest where it is Python-visible. Rebuild the wheel before any pytest step.

### Task A1: `name()` on the optimizer traits

**Files:**
- Modify: `crates/strider-opt/src/pipeline.rs` (trait `Optimizer` ~152-174, trait `PostOptimizer` ~283-295)
- Test: `crates/strider-opt/src/pipeline.rs` (`#[cfg(test)]` module)

**Interfaces:**
- Produces: `Optimizer::name(&self) -> &'static str` and `PostOptimizer::name(&self) -> &'static str`, defaulting to the concrete type's short name.

- [ ] **Step 1: Write the failing test** — append to the `#[cfg(test)]` module in `pipeline.rs`:

```rust
#[test]
fn optimizer_name_is_the_concrete_struct_name() {
    // ConstantFold implements Optimizer; its default name() is its struct name.
    let p: Box<dyn Optimizer> = Box::new(crate::opt::constant_fold::ConstantFold::default());
    assert_eq!(p.name(), "ConstantFold");
}
```

(If `ConstantFold`'s path differs, adjust the constructor path to the real one — `grep -rn "pub struct ConstantFold" crates/strider-opt/src`. Use whatever concrete zero-arg pass is cheapest to construct.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p strider-opt optimizer_name_is_the_concrete_struct_name`
Expected: FAIL — `no method named name`.

- [ ] **Step 3: Add the default method to both traits**

In `trait Optimizer` (after the `apply` signature, still inside the trait body):

```rust
    /// Human-readable pass name — the concrete struct's short name.
    /// Defaulted via `type_name` so no pass has to implement it; override
    /// only if a pass wants a name that differs from its struct name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
            .rsplit("::")
            .next()
            .unwrap_or("UnknownPass")
    }
```

Add the identical method (doc adjusted to "post-pass") to `trait PostOptimizer`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p strider-opt optimizer_name_is_the_concrete_struct_name`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-opt/src/pipeline.rs
git commit -m "feat(opt): default name() on Optimizer/PostOptimizer traits"
```

### Task A2: `OptimizerPipeline.passes` / `.post_passes`, delete the counts

**Files:**
- Modify: `crates/strider-py/src/opt.rs` (`#[pymethods] impl PyOptimizerPipeline` ~250-297)
- Test: `crates/strider-py/tests/python/test_optimizer_pipeline.py`

**Interfaces:**
- Consumes: `Optimizer::name` / `PostOptimizer::name` (Task A1).
- Produces: `OptimizerPipeline.passes -> list[str]`, `OptimizerPipeline.post_passes -> list[str]`. `pass_count`/`post_pass_count` removed.

- [ ] **Step 1: Update the failing test.** In `test_optimizer_pipeline.py`, replace every `pass_count()` / `post_pass_count()` assertion. Where it read `assert p.pass_count() == 10`, write:

```python
def test_default_pipeline_pass_names():
    p = strider.opt.OptimizerPipeline.default()
    names = p.passes
    assert isinstance(names, list) and all(isinstance(n, str) for n in names)
    assert len(names) == 10
    assert "ConstantFold" in names
    assert len(p.post_passes) == 3
```

(Keep the existing count numbers — 10 passes, 3 post-passes — as `len()` assertions. Adjust `strider.opt.OptimizerPipeline` to the pre-reorg path `strider.OptimizerPipeline` if running before Phase B; this test is re-pathed in Task D1 anyway.)

- [ ] **Step 2: Rebuild + run to verify it fails**

```bash
uv run maturin develop    # from workspace root
uv run pytest crates/strider-py/tests/python/test_optimizer_pipeline.py::test_default_pipeline_pass_names -q
```
Expected: FAIL — `AttributeError: 'OptimizerPipeline' object has no attribute 'passes'`.

- [ ] **Step 3: Implement `passes`/`post_passes`, delete the counts.** In `opt.rs`, replace the `pass_count`/`post_pass_count` methods with:

```rust
    /// Names of the fixed-point passes currently registered, in order.
    #[getter]
    fn passes(&self) -> PyResult<Vec<String>> {
        let state = self.lock_state()?;
        Ok(state.passes.iter().map(|p| p.name().to_string()).collect())
    }

    /// Names of the post-passes currently registered, in order.
    #[getter]
    fn post_passes(&self) -> PyResult<Vec<String>> {
        let state = self.lock_state()?;
        Ok(state.post_passes.iter().map(|p| p.name().to_string()).collect())
    }
```

(`#[getter]` makes them attributes — `p.passes`, not `p.passes()`. `name()` is in scope via the `Optimizer`/`PostOptimizer` traits already imported as `strider_orchestrator::opt::…` in the `ErasedPass` aliases; add `use strider_orchestrator::opt::{Optimizer, PostOptimizer};` at the top of `opt.rs` if the trait methods are not resolvable.)

- [ ] **Step 4: Rebuild + run to verify it passes**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python/test_optimizer_pipeline.py -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/opt.rs crates/strider-py/tests/python/test_optimizer_pipeline.py
git commit -m "feat(py): OptimizerPipeline.passes/.post_passes replace the _count methods"
```

### Task A3: `VnSpace` space constants

**Files:**
- Modify: `crates/strider-py/src/sleigh.rs` (`#[pymethods] impl PyVnSpace` ~117-185)
- Test: `crates/strider-py/tests/python/test_sleigh.py`

**Interfaces:**
- Produces: `VnSpace.REGISTER`, `VnSpace.RAM`, `VnSpace.CONST`, `VnSpace.UNIQUE` as class-level `VnSpace` constants (no call). `name()` retained. The `ram()`/`register()`/`const()`/`unique()` classmethods removed.

- [ ] **Step 1: Update the test.** In `test_sleigh.py`, change every `VnSpace.register()` call site to `VnSpace.REGISTER` (and `ram()`→`RAM`, `const()`→`CONST`, `unique()`→`UNIQUE`). Add:

```python
def test_vnspace_constants_are_instances_not_callables():
    from strider.sleigh import VnSpace   # pre-reorg: `from strider import VnSpace`
    assert isinstance(VnSpace.REGISTER, VnSpace)
    assert VnSpace.REGISTER == VnSpace.REGISTER
    assert VnSpace.REGISTER != VnSpace.RAM
    assert VnSpace.REGISTER.name() == "REGISTER"
```

- [ ] **Step 2: Rebuild + run to verify it fails**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python/test_sleigh.py::test_vnspace_constants_are_instances_not_callables -q
```
Expected: FAIL — `AttributeError: type object 'VnSpace' has no attribute 'REGISTER'`.

- [ ] **Step 3: Replace the classmethod constructors with class constants.** Delete the four `#[classmethod]` `ram`/`register`/`const_`/`unique` methods. Add them back as module/class constants by registering them after class creation. PyO3 has no `#[classattr]` returning `Self` at class-body time cleanly for a frozen type built from Rust consts, so use `#[classattr]`:

```rust
    #[classattr]
    #[allow(non_snake_case)]
    fn REGISTER() -> Self {
        Self { inner: rsleigh::VnSpace::REGISTER }
    }
    #[classattr]
    #[allow(non_snake_case)]
    fn RAM() -> Self {
        Self { inner: rsleigh::VnSpace::RAM }
    }
    #[classattr]
    #[allow(non_snake_case)]
    fn CONST() -> Self {
        Self { inner: rsleigh::VnSpace::CONST }
    }
    #[classattr]
    #[allow(non_snake_case)]
    fn UNIQUE() -> Self {
        Self { inner: rsleigh::VnSpace::UNIQUE }
    }
```

(`#[classattr]` on a zero-arg fn evaluates once at class init and binds the result as a class attribute — `VnSpace.REGISTER` is a `VnSpace` instance, no call. Keep `name`, `__repr__`, `__eq__`, `__hash__` unchanged.)

- [ ] **Step 4: Rebuild + run to verify it passes**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python/test_sleigh.py -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/sleigh.rs crates/strider-py/tests/python/test_sleigh.py
git commit -m "feat(py): VnSpace.REGISTER/RAM/CONST/UNIQUE constants replace constructors"
```

### Task A4: `Node.fingerprint` → `Node.asm_fingerprint`

**Files:**
- Modify: `crates/strider-py/src/node.rs` (`fingerprint` ~293-316), and its two callers `crates/strider-py/src/matcher.rs` + `crates/strider-py/src/cfg.rs` (they call `node.fingerprint(py)` internally).
- Test: `crates/strider-py/tests/python/` (whichever test reads `Node.fingerprint` — `grep -rln "\.fingerprint(" crates/strider-py/tests/python`).

**Interfaces:**
- Produces: `Node.asm_fingerprint` (Python), same signature/behavior as the old `fingerprint`.

- [ ] **Step 1: Update the test.** Change the Python `node.fingerprint()` call sites in the test(s) to `node.asm_fingerprint()`. Add if none exists:

```python
def test_node_asm_fingerprint_name():
    # any lifted function; the first node with a fingerprint
    ...  # obtain a Node `n`
    assert isinstance(n.asm_fingerprint(), list)
    assert not hasattr(n, "fingerprint")
```

- [ ] **Step 2: Rebuild + run to verify it fails**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python -k asm_fingerprint -q
```
Expected: FAIL — no attribute `asm_fingerprint`.

- [ ] **Step 3: Rename.** In `node.rs`, the method is `pub(crate) fn fingerprint(&self, py)` with no `#[pyo3]` attr — it is exposed under some public name elsewhere. Rename the Rust fn to `asm_fingerprint` and ensure it is Python-exposed as `asm_fingerprint` (add `#[pyo3(name = "asm_fingerprint")]` if the method is in the `#[pymethods]` public set; keep `pub(crate)` visibility for the internal callers). Update the two internal call sites in `matcher.rs` (`node.fingerprint(py)` → `node.asm_fingerprint(py)`) and `cfg.rs` (`fingerprint_pcode` body) to the new Rust name. `Cfg.fingerprint_pcode` and `Match.asm_fingerprint` public names are unchanged.

- [ ] **Step 4: Rebuild + run to verify it passes**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/node.rs crates/strider-py/src/matcher.rs crates/strider-py/src/cfg.rs crates/strider-py/tests/python
git commit -m "refactor(py): Node.fingerprint -> Node.asm_fingerprint"
```

### Task A5: `Match.int/bool/uint` → `const_int/const_bool/const_uint`

**Files:**
- Modify: `crates/strider-py/src/matcher.rs` (`uint`/`int_`/`bool_` ~251-290)
- Test: `crates/strider-py/tests/python/test_pattern_match.py` (and any test using `m.int(c)` / `m.bool(c)` / `m.uint(c)` — `grep -rln "\.int(\|\.bool(\|\.uint(" crates/strider-py/tests/python`)

**Interfaces:**
- Produces: `Match.const_int`, `Match.const_bool`, `Match.const_uint` (aligning with `Node.const_int/bool/uint`). Old `int`/`bool`/`uint` removed.

- [ ] **Step 1: Update the tests** — replace `m.int(c)`→`m.const_int(c)`, `m.bool(c)`→`m.const_bool(c)`, `m.uint(c)`→`m.const_uint(c)`. Add:

```python
def test_match_const_readers_align_with_node():
    # from the existing bool-const test: m[c] path already covered elsewhere
    ...  # obtain a Match `m` and Capture `c` bound to an I1 const
    assert m.const_bool(c) is True or m.const_bool(c) is False
    assert isinstance(m.const_uint(c), int)
    assert not hasattr(m, "int") or callable(getattr(type(m), "int", None)) is False
```

- [ ] **Step 2: Rebuild + run to verify it fails**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python/test_pattern_match.py -q
```
Expected: FAIL — no attribute `const_bool` on Match.

- [ ] **Step 3: Rename.** In `matcher.rs`: rename `uint`→`const_uint` (no `#[pyo3]` needed, Rust name = Python name); change `#[pyo3(name = "int")]` to `#[pyo3(name = "const_int")]` (keep Rust fn `int_` or rename to `const_int`); change `#[pyo3(name = "bool")]` to `#[pyo3(name = "const_bool")]`. Update docstrings ("as an unsigned `int`" phrasing stays).

- [ ] **Step 4: Rebuild + run to verify it passes**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/matcher.rs crates/strider-py/tests/python
git commit -m "refactor(py): Match.const_int/const_bool/const_uint align with Node"
```

---

## Phase B — Viz/serialization unification

Collapse the 13-name renderer zoo to `to_dot(path=None)` / `to_html(path=None)` / `neighborhood_dot`. Each `to_*` returns the string when `path is None`, else writes the file and returns `None`.

### Task B1: `Cfg` renderers

**Files:**
- Modify: `crates/strider-py/src/cfg.rs` (`to_html`/`to_dot`/`html_str` ~166-191, `raw_neighborhood_dot` ~273-286, `region_texts` ~288-316; the `CfgDotOp`/`CfgDotResult`/`dispatch_dot` machinery ~79-153 stays)
- Test: `crates/strider-py/tests/python/` (Cfg render tests — `grep -rln "\.to_dot(\|\.to_html(\|\.html_str(\|raw_neighborhood_dot\|region_texts" crates/strider-py/tests/python`)

**Interfaces:**
- Produces: `Cfg.to_dot(path=None) -> str|None`, `Cfg.to_html(path=None, style=None) -> str|None`, `Cfg.neighborhood_dot(...)` (unchanged). Removed: `html_str`, `raw_neighborhood_dot`. `region_texts`→`_region_texts`.

- [ ] **Step 1: Write/adjust the test:**

```python
def test_cfg_to_dot_str_and_file(tmp_path):
    cfg = ...  # from analyze
    s = cfg.to_dot()                 # path=None -> string
    assert isinstance(s, str) and "digraph" in s.lower()
    out = tmp_path / "c.dot"
    assert cfg.to_dot(str(out)) is None
    assert out.read_text()
    assert isinstance(cfg.to_html(), str)     # pretty HTML string now exists
    assert not hasattr(cfg, "html_str")
    assert not hasattr(cfg, "raw_neighborhood_dot")
```

- [ ] **Step 2: Rebuild + run to verify it fails**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python -k cfg_to_dot -q
```
Expected: FAIL — `to_dot()` requires `path`.

- [ ] **Step 3: Implement.** Replace the three methods with two `path`-optional ones; delete `html_str` and `raw_neighborhood_dot`; rename `region_texts`→`_region_texts` (add a leading `#[pyo3(name = "_region_texts")]` or rename the Rust fn):

```rust
    /// Render the CFG to DOT. Returns the DOT string when `path` is None,
    /// otherwise writes it to `path` and returns None.
    #[pyo3(signature = (path=None))]
    fn to_dot(&self, py: Python<'_>, path: Option<&str>) -> PyResult<Option<String>> {
        match path {
            Some(p) => self.dispatch_dot(py, "dark_cfg", CfgDotOp::ToDot(p)).map(|_| None),
            None => match self.dispatch_dot(py, "dark_cfg", CfgDotOp::DotStr)? {
                CfgDotResult::Dot(s) => Ok(Some(s)),
                _ => Ok(None),
            },
        }
    }

    /// Render the CFG to a standalone HTML page. Returns the HTML string when
    /// `path` is None, otherwise writes it and returns None. `style` selects
    /// the dot theme (default "dark_cfg").
    #[pyo3(signature = (path=None, style=None))]
    fn to_html(&self, py: Python<'_>, path: Option<&str>, style: Option<&str>) -> PyResult<Option<String>> {
        let style = style.unwrap_or("dark_cfg");
        match path {
            Some(p) => self.dispatch_dot(py, style, CfgDotOp::ToHtml(p)).map(|_| None),
            None => match self.dispatch_dot(py, style, CfgDotOp::HtmlStr)? {
                CfgDotResult::Html(s) => Ok(Some(s)),
                _ => Ok(None),
            },
        }
    }
```

Add a `DotStr` arm to `CfgDotOp` and a `Dot(String)` arm to `CfgDotResult`, and handle it in `dispatch_dot` (mirror the `HtmlStr` arm using `d.as_dot()` — locate the DOT-string method on `dot::GraphDot`; if only `dump_as_dot(path)` exists, add an in-memory variant or render to a temp `String` via the dumper's `to_string`; confirm the exact `GraphDot` API with `grep -n "pub fn" crates/dot/src/*.rs`):

```rust
enum CfgDotOp<'a> { ToHtml(&'a str), ToDot(&'a str), HtmlStr, DotStr }
enum CfgDotResult { Unit, Html(String), Dot(String) }
```

- [ ] **Step 4: Rebuild + run to verify it passes**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/cfg.rs crates/strider-py/tests/python
git commit -m "feat(py): unify Cfg renderers on to_dot/to_html(path=None)"
```

### Task B2: `Function` renderers

**Files:**
- Modify: `crates/strider-py/src/function.rs` (`raw_dot_str`/`raw_html_str`/`to_raw_dot`/`to_raw_html` ~147-176, `raw_neighborhood_dot` ~192-214; `write_to` helper ~121-125 stays)
- Test: `crates/strider-py/tests/python/`

**Interfaces:**
- Produces: `Function.to_dot(path=None) -> str|None`, `Function.to_html(path=None) -> str|None`, `Function.neighborhood_dot(...)`. Removed: `raw_dot_str`, `raw_html_str`, `to_raw_dot`, `to_raw_html`, `raw_neighborhood_dot` (renamed).

- [ ] **Step 1: Test:**

```python
def test_function_to_dot_str_and_file(tmp_path):
    fn = ...  # from analyze
    assert isinstance(fn.to_dot(), str)
    out = tmp_path / "f.dot"
    assert fn.to_dot(str(out)) is None and out.read_text()
    assert isinstance(fn.to_html(), str)
    assert isinstance(fn.neighborhood_dot(fn.entry_node().id), str)
    for gone in ("raw_dot_str", "raw_html_str", "to_raw_dot", "to_raw_html", "raw_neighborhood_dot"):
        assert not hasattr(fn, gone)
```

- [ ] **Step 2: Rebuild + run to verify it fails**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python -k function_to_dot -q
```
Expected: FAIL.

- [ ] **Step 3: Implement.** Replace the four raw methods with two, and rename `raw_neighborhood_dot`→`neighborhood_dot`:

```rust
    /// Render the graph exactly as stored (no Sleigh) to DOT. Returns the
    /// string when `path` is None, else writes it and returns None.
    #[pyo3(signature = (path=None))]
    fn to_dot(&self, path: Option<&str>) -> PyResult<Option<String>> {
        let s = self.with_read_value(strider_ir::Function::raw_dot)?
            .map_err(crate::errors::into_strider_err)?;
        match path {
            Some(p) => { write_to(p, s)?; Ok(None) }
            None => Ok(Some(s)),
        }
    }

    /// Like `to_dot` but wraps the DOT in a self-contained HTML page.
    #[pyo3(signature = (path=None))]
    fn to_html(&self, path: Option<&str>) -> PyResult<Option<String>> {
        let s = self.with_read_value(strider_ir::Function::raw_html)?
            .map_err(crate::errors::into_strider_err)?;
        match path {
            Some(p) => { write_to(p, s)?; Ok(None) }
            None => Ok(Some(s)),
        }
    }
```

Rename the `raw_neighborhood_dot` fn to `neighborhood_dot` (keep its body and `#[pyo3(signature = ...)]` unchanged).

- [ ] **Step 4: Rebuild + run to verify it passes**

```bash
uv run maturin develop && uv run pytest crates/strider-py/tests/python -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/function.rs crates/strider-py/tests/python
git commit -m "feat(py): unify Function renderers on to_dot/to_html(path=None)"
```

### Task B3: `Lifter` renderers

**Files:**
- Modify: `crates/strider-py/src/strider_cls.rs` (`dump_html`/`dump_dot`/`html_str` ~577-607; `DotOp`/`DotResult`/`dispatch_dot` ~246-289 stay; `neighborhood_dot`/`visualize` unchanged)
- Test: `crates/strider-py/tests/python/`

**Interfaces:**
- Produces: `Lifter.to_dot(function, path=None) -> str|None`, `Lifter.to_html(function, path=None, style=None) -> str|None`. `neighborhood_dot`/`visualize` unchanged. Removed: `dump_dot`, `dump_html`, `html_str`.

- [ ] **Step 1: Test** (mirror B1 with the `function` first arg; `ElfLifter` inherits these, so also assert `not hasattr(lifter, "dump_dot")`).

- [ ] **Step 2: Rebuild + run to verify it fails** — `uv run maturin develop && uv run pytest -k lifter_to_dot -q`. Expected: FAIL.

- [ ] **Step 3: Implement.** Replace `dump_dot`/`dump_html`/`html_str` with `to_dot`/`to_html` mirroring B1's shape but threading `function`, and add a `DotStr` arm to `DotOp`/`DotResult`:

```rust
    #[pyo3(signature = (function, path=None))]
    fn to_dot(&self, function: &PyFunction, path: Option<&str>) -> PyResult<Option<String>> {
        match path {
            Some(p) => self.dispatch_dot(function, None, DotOp::DumpDot(p)).map(|_| None),
            None => match self.dispatch_dot(function, None, DotOp::DotStr)? {
                DotResult::Dot(s) => Ok(Some(s)),
                _ => Ok(None),
            },
        }
    }

    #[pyo3(signature = (function, path=None, style=None))]
    fn to_html(&self, function: &PyFunction, path: Option<&str>, style: Option<&str>) -> PyResult<Option<String>> {
        match path {
            Some(p) => self.dispatch_dot(function, style, DotOp::DumpHtml(p)).map(|_| None),
            None => match self.dispatch_dot(function, style, DotOp::HtmlStr)? {
                DotResult::Html(s) => Ok(Some(s)),
                _ => Ok(None),
            },
        }
    }
```

Add `DotStr` to `DotOp` and `Dot(String)` to `DotResult`, handled in `dispatch_dot` (mirror the `HtmlStr` arm with the DOT-string render).

- [ ] **Step 4: Rebuild + run to verify it passes** — `uv run maturin develop && uv run pytest crates/strider-py/tests/python -q`. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/strider_cls.rs crates/strider-py/tests/python
git commit -m "feat(py): unify Lifter renderers on to_dot/to_html(path=None)"
```

### Task B4: `Cfg.block_at` → `Cfg.region_at` + "block" docstrings

**Files:**
- Modify: `crates/strider-py/src/cfg.rs` (`block_at` ~318-337; docstrings at ~254-255 `neighborhood_dot`, ~288 `region_texts`), `crates/strider-py/strider/explore.py` (~339-348 call site + comments)
- Test: `crates/strider-py/tests/python/`

**Interfaces:**
- Produces: `Cfg.region_at(addr) -> int|None`. `block_at` removed.

- [ ] **Step 1: Test:**

```python
def test_cfg_region_at():
    cfg = ...  # from analyze; entry addr known
    assert isinstance(cfg.region_at(entry_addr), int)
    assert not hasattr(cfg, "block_at")
```

- [ ] **Step 2: Rebuild + run to verify it fails.** Expected: FAIL — no attribute `region_at`.

- [ ] **Step 3: Rename** the Rust fn `block_at`→`region_at`; change the "predecessor+successor blocks" phrasing (neighborhood_dot doc) → "regions" and "per-block text" (region_texts doc) → "per-region text"; update `explore.py:340` `block_at(`→`region_at(` and the two comments ("containing block"→"containing region", "block-start addresses"→"region-start addresses").

- [ ] **Step 4: Rebuild + run to verify it passes.** Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/cfg.rs crates/strider-py/strider/explore.py crates/strider-py/tests/python
git commit -m "refactor(py): Cfg.block_at -> region_at (no block type exists)"
```

---

## Phase C — `load_elf` collapse + `StriderError` single home

### Task C1: collapse `load_elf` to one function

**Files:**
- Modify: `crates/strider-py/src/reader.rs` (drop one of the two `#[pyfunction]`s or repoint; ~482-501), `crates/strider-py/strider/_api.py` (`load_elf`/`load_elf_from_segments`/`load_elf_from_sections` ~241-327)
- Test: `crates/strider-py/tests/python/test_high_level_api.py`

**Interfaces:**
- Produces: `strider.lift.load_elf(path, *, from_segments=True, apply_relocations=True, arch=None, cc=None) -> ElfLifter`. Removed: `load_elf_from_segments`, `load_elf_from_sections` (Python names).

- [ ] **Step 1: Update tests** — replace `load_elf_from_segments(p)`→`load_elf(p)` and `load_elf_from_sections(p)`→`load_elf(p, from_segments=False)`. Add:

```python
def test_load_elf_flag_selects_strategy(elf_path):
    a = strider.load_elf(elf_path)                      # segments (default)
    b = strider.load_elf(elf_path, from_segments=False) # sections
    assert isinstance(a, strider.lift.ElfLifter)
    assert isinstance(b, strider.lift.ElfLifter)
    import strider
    assert not hasattr(strider, "load_elf_from_segments")
    assert not hasattr(strider, "load_elf_from_sections")
```

(Pre-reorg, `strider.lift.ElfLifter` is `strider.ElfLifter`; this test is re-pathed in Phase D.)

- [ ] **Step 2: Rebuild + run to verify it fails.** Expected: FAIL — unexpected keyword `from_segments`.

- [ ] **Step 3: Implement.** In `_api.py`, collapse the three functions into one. Keep the shared `_load_elf_with`; select the Rust loader by the flag:

```python
def load_elf(
    path: StrPath,
    *,
    from_segments: bool = True,
    apply_relocations: bool = True,
    arch: Optional[SleighArch] = None,
    cc: Optional[CallingConvention] = None,
) -> "ElfLifter":
    """Load an ELF into an ElfLifter. `from_segments=True` (default) walks
    PT_LOAD program headers; `from_segments=False` forces the section-header
    walk (for objects without usable segments)."""
    loader = _ext.load_elf_from_segments if from_segments else _ext.load_elf_from_sections
    return _load_elf_with(
        loader, path, apply_relocations=apply_relocations, arch=arch, cc=cc
    )
```

Delete the `load_elf_from_segments` / `load_elf_from_sections` Python functions. Keep the two Rust `#[pyfunction]`s (they are the internal `_ext.load_elf_from_*` loaders `_load_elf_with` calls) but they no longer need to be top-level Python API — they stay on `_strider` only, not re-exported. Update `__init__.py`'s `from ._api import (...)` to import only `ElfLifter, load_elf`.

- [ ] **Step 4: Rebuild + run to verify it passes.** Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/reader.rs crates/strider-py/strider/_api.py crates/strider-py/strider/__init__.py crates/strider-py/tests/python
git commit -m "feat(py): collapse load_elf_from_* into load_elf(from_segments=)"
```

### Task C2: `StriderError` single home

**Files:**
- Modify: `crates/strider-py/src/errors.rs` (`register` ~40-50)
- Test: `crates/strider-py/tests/python/`

**Interfaces:**
- Produces: `strider.StriderError` only. `strider.errors` submodule removed.

- [ ] **Step 1: Test:**

```python
def test_strider_error_single_home():
    import strider
    assert hasattr(strider, "StriderError")
    import importlib
    try:
        importlib.import_module("strider.errors")
        assert False, "strider.errors should not exist"
    except ModuleNotFoundError:
        pass
```

- [ ] **Step 2: Rebuild + run to verify it fails.** Expected: FAIL — `strider.errors` still importable.

- [ ] **Step 3: Implement.** Rewrite `errors::register` to add `StriderError` only to the top-level module and drop the submodule + `sys.modules` insert:

```rust
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    Ok(())
}
```

Change the `create_exception!` first arg from `strider.errors` to `strider` (sets `__module__` to `strider`). Remove the `strider.errors` hoist in `__init__.py` (the `if hasattr(_ext, "errors")` block).

- [ ] **Step 4: Rebuild + run to verify it passes.** Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/errors.rs crates/strider-py/strider/__init__.py crates/strider-py/tests/python
git commit -m "refactor(py): StriderError single home at strider.StriderError"
```

---

## Phase D — Namespace reorg + cdylib rename + hygiene

This phase moves every class into its domain submodule and renames the cdylib. Do it after Phases A–C so the renamed methods travel with their classes.

### Task D1: rename the cdylib to `strider._strider`

**Files:**
- Modify: `crates/strider-py/src/lib.rs` (`#[pymodule] fn strider` ~97), workspace-root `pyproject.toml` (`module-name` line ~48), `crates/strider-py/strider/__init__.py` (~13,15), `crates/strider-py/strider/_api.py` (~60-61)
- Test: `crates/strider-py/tests/python/test_low_level.py` (references the old `strider.strider` path — update it)

**Interfaces:**
- Produces: the extension importable as `strider._strider`; `strider.strider` no longer exists.

- [ ] **Step 1: Test:**

```python
def test_cdylib_is_private():
    import strider, importlib
    importlib.import_module("strider._strider")   # exists
    assert not hasattr(strider, "strider")        # self-submodule gone
```

- [ ] **Step 2: Change config + imports.** In `pyproject.toml`: `module-name = "strider._strider"`. In `lib.rs`: `#[pymodule] fn _strider(...)`. In `__init__.py`: `_ext = _importlib.import_module("strider._strider")` and `from ._strider import *`. In `_api.py`: `import_module("strider._strider")` and `from ._strider import (...)`. Update `test_low_level.py`'s `strider.strider` reference to `strider._strider`.

- [ ] **Step 3: Rebuild + run to verify it passes.** (This is a rename; the test asserts the new path.)

```bash
uv run maturin develop && uv run pytest crates/strider-py/tests/python/test_low_level.py -q
```
Expected: PASS; confirm `crates/strider-py/strider/_strider.abi3.so` now exists and the old `strider.abi3.so` is gone (delete a stale one if present).

- [ ] **Step 4: Commit**

```bash
git add pyproject.toml crates/strider-py/src/lib.rs crates/strider-py/strider/__init__.py crates/strider-py/strider/_api.py crates/strider-py/tests/python/test_low_level.py
git commit -m "refactor(py): rename cdylib strider->strider._strider, kill self-submodule"
```

### Task D2: domain submodules — register each class on its home

**Files:**
- Modify: `crates/strider-py/src/lib.rs` (the `#[pymodule]` body ~98-116), and the `register` fns in `sleigh.rs`/`arch.rs`/`cc.rs`/`function.rs`/`node.rs`/`options.rs`/`cfg.rs`/`strider_cls.rs`/`reader.rs`/`opt.rs`/`matcher.rs` (only where they add to the parent vs a submodule), plus every `#[pyclass(module="strider")]` attribute → its domain module.
- Test: `crates/strider-py/tests/python/test_namespaces.py` (new)

**Interfaces:**
- Produces: the full target layout — `strider.ir.Function`, `strider.sleigh.Sleigh`, etc.

- [ ] **Step 1: Write the namespace test** (`test_namespaces.py`):

```python
import strider

EXPECTED = {
    "ir": ["Function", "Node"],
    "lift": ["Lifter", "ElfLifter", "LifterOptions", "load_elf"],
    "cfg": ["Cfg", "CfgOptions"],
    "sleigh": ["SleighArch", "CallingConvention", "Sleigh", "Vn", "VnSpace"],
    "reader": ["BufferReader", "MemReader", "ReadOnlyMemory"],
    "opt": ["OptimizerPipeline"],
    "pattern": ["Pat", "Capture", "Match"],
    "template": ["Template"],
}

def test_each_symbol_has_exactly_one_home():
    for mod, names in EXPECTED.items():
        m = getattr(strider, mod)
        for n in names:
            assert hasattr(m, n), f"strider.{mod}.{n} missing"

def test_top_level_is_just_error_and_submodules():
    # nothing but StriderError + submodules leaks at top level
    assert hasattr(strider, "StriderError")
    for gone in ("Sleigh", "Cfg", "Function", "Node", "Lifter", "load_elf",
                 "OptimizerPipeline", "Vn", "VnSpace"):
        assert not hasattr(strider, gone), f"strider.{gone} should have moved"
```

- [ ] **Step 2: Rebuild + run to verify it fails.** Expected: FAIL — classes still top-level.

- [ ] **Step 3: Restructure registration.** Rewrite the `#[pymodule]` body in `lib.rs` to build submodules and route each `register` into the correct one (each `register` fn already does `m.add_class`/`add_function` on the module it is handed, so passing a submodule is the change):

```rust
#[pymodule]
fn _strider(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    force_anyhow_backtrace_capture();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(pyo3::wrap_pyfunction!(viz_standalone_js, m)?)?; // renamed in D4
    errors::register(py, m)?;                 // StriderError -> top level

    let sleigh = PyModule::new_bound(py, "sleigh")?;
    sleigh::register(py, &sleigh)?;
    arch::register(py, &sleigh)?;
    cc::register(py, &sleigh)?;
    m.add_submodule(&sleigh)?;

    let reader = PyModule::new_bound(py, "reader")?;
    reader::register(py, &reader)?;
    m.add_submodule(&reader)?;

    let cfg = PyModule::new_bound(py, "cfg")?;
    cfg::register(py, &cfg)?;
    options::register_cfg(py, &cfg)?;         // CfgOptions only (see split below)
    m.add_submodule(&cfg)?;

    let ir = PyModule::new_bound(py, "ir")?;
    function::register(py, &ir)?;
    node::register(py, &ir)?;
    m.add_submodule(&ir)?;

    let lift = PyModule::new_bound(py, "lift")?;
    strider_cls::register(py, &lift)?;        // Lifter + `lifter` fn
    options::register_lift(py, &lift)?;       // LifterOptions only
    m.add_submodule(&lift)?;

    let opt = PyModule::new_bound(py, "opt")?;
    opt::register(py, &opt)?;                 // OptimizerPipeline INTO opt (see below)
    m.add_submodule(&opt)?;

    let pattern = PyModule::new_bound(py, "pattern")?;
    pattern::register(py, &pattern)?;
    matcher::register(py, &pattern)?;         // Match INTO pattern
    m.add_submodule(&pattern)?;

    let template = PyModule::new_bound(py, "template")?;
    template::register(py, &template)?;
    m.add_submodule(&template)?;
    Ok(())
}
```

Required adjustments to the `register` fns:
- `options.rs`: split `register` into `register_cfg` (adds `CfgOptions`) and `register_lift` (adds `LifterOptions`). Confirm which classes it currently adds with `grep -n "add_class" crates/strider-py/src/options.rs`.
- `opt.rs`: `register` currently does `parent.add_class::<PyOptimizerPipeline>()` then builds its own `"opt"` submodule for passes. Change it to add `PyOptimizerPipeline` to the passed module `m` (now the `opt` submodule) and add the pass classes to that same `m` directly (drop the inner `PyModule::new_bound(py, "opt")` / `add_submodule` — lib.rs now owns the submodule).
- `pattern.rs`: if `register` builds its own `"pattern"` submodule, change it to add to the passed `m` (lib.rs owns the submodule now). Confirm with `grep -n "PyModule::new_bound\|add_submodule\|add_class\|add_function" crates/strider-py/src/pattern.rs`.
- `template.rs`: same check as pattern.
- `matcher.rs`: adds `PyMatch` (and its `predicate`/etc.) — ensure it adds to the passed `m` (the `pattern` submodule).
- Every `#[pyclass(... module = "strider" ...)]` attribute → its domain module string: `PySleigh`/`PyVn`/`PyVnSpace`→`"strider.sleigh"`, `PyCfg`→`"strider.cfg"`, `PyLifter`→`"strider.lift"`, `PyFunction`/`PyNode`→`"strider.ir"`, `PyOptimizerPipeline`→`"strider.opt"`, `PyMatch`→`"strider.pattern"`, options→`"strider.cfg"`/`"strider.lift"`, arch/cc→`"strider.sleigh"`, reader classes→`"strider.reader"`. Find them all: `grep -rn 'module = "strider"' crates/strider-py/src`.

- [ ] **Step 4: Rebuild + run to verify it passes.**

```bash
uv run maturin develop && uv run pytest crates/strider-py/tests/python/test_namespaces.py -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src crates/strider-py/tests/python/test_namespaces.py
git commit -m "feat(py): register every class on its domain submodule"
```

### Task D3: rewrite `__init__.py` — expose only StriderError + submodules, `del` leaks, `__all__`

**Files:**
- Modify: `crates/strider-py/strider/__init__.py`
- Test: `crates/strider-py/tests/python/test_namespaces.py` (extend)

**Interfaces:**
- Produces: `dir(strider)` = `StriderError` + `{ir,lift,cfg,sleigh,reader,opt,pattern,template}` + dunders; no `_importlib`/`_sys`/`_ext`/`_api`/`strider`.

- [ ] **Step 1: Extend the test:**

```python
def test_no_import_leaks_at_top_level():
    import strider
    public = {n for n in dir(strider) if not n.startswith("__")}
    allowed = {"StriderError", "ir", "lift", "cfg", "sleigh", "reader",
               "opt", "pattern", "template"}
    assert public <= allowed, f"unexpected top-level names: {public - allowed}"
```

- [ ] **Step 2: Rebuild + run to verify it fails.** Expected: FAIL — `_importlib`, `_sys`, `_ext`, `_api`, etc. leak.

- [ ] **Step 3: Rewrite `__init__.py`:**

```python
"""strider — Python bindings for the Strider binary analysis pipeline.

The native extension is loaded as `strider._strider`; the public API is the
domain submodules (`strider.ir`, `strider.lift`, `strider.cfg`,
`strider.sleigh`, `strider.reader`, `strider.opt`, `strider.pattern`,
`strider.template`) plus the top-level `StriderError`.
"""

import importlib as _importlib
import sys as _sys

_ext = _importlib.import_module("strider._strider")

# Publish the native domain submodules under their public dotted names.
for _name in ("ir", "lift", "cfg", "sleigh", "reader", "opt", "pattern", "template"):
    _sub = getattr(_ext, _name)
    _sys.modules[f"strider.{_name}"] = _sub
    globals()[_name] = _sub

# The one cross-cutting top-level symbol.
StriderError = _ext.StriderError
__version__ = _ext.__version__

# Bind the pure-Python facade members into their home submodule (strider.lift).
from . import _api as _mod_api  # noqa: E402
lift.ElfLifter = _mod_api.ElfLifter          # type: ignore[attr-defined]
lift.load_elf = _mod_api.load_elf            # type: ignore[attr-defined]

__all__ = ["StriderError", "ir", "lift", "cfg", "sleigh", "reader",
           "opt", "pattern", "template"]

# Drop import machinery from the public namespace.
del _importlib, _sys, _name, _sub, _mod_api
```

(If `pattern` needs its `constraints` subpackage hoisted into `sys.modules` too, add `strider.pattern.constraints` alongside — confirm with `grep -rn "constraints" crates/strider-py/strider/__init__.py crates/strider-py/src/pattern.rs`.)

- [ ] **Step 4: Rebuild + run to verify it passes.**

```bash
uv run maturin develop && uv run pytest crates/strider-py/tests/python/test_namespaces.py -q
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/strider/__init__.py crates/strider-py/tests/python/test_namespaces.py
git commit -m "refactor(py): __init__ exposes only StriderError + domain submodules"
```

### Task D4: hide `viz_standalone_js` and `region_texts`

**Files:**
- Modify: `crates/strider-py/src/lib.rs` (`viz_standalone_js` ~92-95,101), `crates/strider-py/strider/explore.py` (the call site), `crates/strider-py/src/cfg.rs` (`region_texts` if not already renamed in B1)
- Test: `crates/strider-py/tests/python/test_namespaces.py`

**Interfaces:**
- Produces: `_viz_standalone_js` (underscore, internal); `Cfg._region_texts`.

- [ ] **Step 1: Test:**

```python
def test_internal_helpers_are_private():
    import strider
    assert not hasattr(strider, "viz_standalone_js")
    cfg = ...  # any cfg
    assert not hasattr(cfg, "region_texts")
    assert hasattr(cfg, "_region_texts")
```

- [ ] **Step 2: Rebuild + run to verify it fails.** Expected: FAIL.

- [ ] **Step 3: Rename.** In `lib.rs`, `#[pyo3(name = "_viz_standalone_js")]` on the `viz_standalone_js` fn (or rename the fn). Update `explore.py`'s `strider.viz_standalone_js()` call → `strider._strider._viz_standalone_js()` (the explorer reaches the private handle). Ensure `region_texts`→`_region_texts` from B1 landed (if B1 deferred it, do it here).

- [ ] **Step 4: Rebuild + run to verify it passes.** Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/lib.rs crates/strider-py/strider/explore.py crates/strider-py/src/cfg.rs crates/strider-py/tests/python
git commit -m "refactor(py): hide viz_standalone_js and region_texts as internal"
```

---

## Phase E — Sweep callers, regenerate stubs, full gate

### Task E1: update the rest of the pytest suite to the new surface

**Files:**
- Modify: all of `crates/strider-py/tests/python/` still using old paths/names.

- [ ] **Step 1: Find every stale reference.** Run each and fix the hits:

```bash
cd /mnt/c/Users/mikeg/Documents/strider
grep -rln "strider\.Sleigh\|strider\.Cfg\|strider\.Function\|strider\.Node\|strider\.Lifter\|strider\.Vn\|strider\.VnSpace\|strider\.OptimizerPipeline\|strider\.CallingConvention\|strider\.SleighArch\|strider\.CfgOptions\|strider\.LifterOptions\|strider\.BufferReader\|strider\.MemReader\|strider\.ReadOnlyMemory" crates/strider-py/tests/python
grep -rln "load_elf_from_segments\|load_elf_from_sections\|\.block_at(\|\.pass_count(\|\.post_pass_count(\|\.dump_dot(\|\.dump_html(\|\.html_str(\|raw_dot_str\|raw_html_str\|to_raw_dot\|to_raw_html\|raw_neighborhood_dot\|region_texts\|VnSpace\.register(\|VnSpace\.ram(\|VnSpace\.const(\|VnSpace\.unique(" crates/strider-py/tests/python
grep -rln "from strider import errors\|strider\.errors\|strider\.strider\|\.fingerprint(" crates/strider-py/tests/python
```

Rewrite each hit to the new spelling (e.g. `strider.Sleigh`→`strider.sleigh.Sleigh`, `strider.OptimizerPipeline`→`strider.opt.OptimizerPipeline`, `m.int(c)`→`m.const_int(c)`). Prefer top-of-file `from strider import sleigh, ir, lift, cfg, opt, pattern` imports and short names where a test uses many symbols.

- [ ] **Step 2: Rebuild + run the whole suite**

```bash
uv run maturin develop && uv run pytest crates/strider-py/tests/python -q
```
Expected: PASS, 0 failures, 0 errors. (Skips: expect 0 — the suite was made skip-free earlier.)

- [ ] **Step 3: Commit**

```bash
git add crates/strider-py/tests/python
git commit -m "test(py): update suite to the reorganized stable surface"
```

### Task E2: regenerate `.pyi` stubs to the new module tree

**Files:**
- Modify: `crates/strider-py/strider/*.pyi`, `crates/strider-py/strider/pattern/*.pyi`, and any stub-gen driver.

- [ ] **Step 1: Locate the stub generator.** `grep -rn "define_stub_info_gatherer\|gen_stub" crates/strider-py/src/lib.rs` (present at lib.rs:88) and find the driver: `ls crates/strider-py/examples/ 2>/dev/null; grep -rln "stub_info\|StubInfo" crates/strider-py`. Run it (typical invocation — confirm the exact example/bin name first):

```bash
grep -rn "fn main" crates/strider-py/examples/*.rs 2>/dev/null
# then, using the real example name:
cargo run -p strider-py --example <stub_gen_example>
```

- [ ] **Step 2: Reconcile hand-written stub parts.** The generator covers `#[gen_stub_*]` types; the pure-Python facade (`ElfLifter`, `load_elf` in `_api.py`) and the module tree layout may need hand edits so the `.pyi` files sit at `strider/ir.pyi`, `strider/lift.pyi`, etc., matching the new submodules. Update `pyproject.toml`'s `[tool.maturin] include` globs if stub file locations changed. Ensure `strider/__init__.pyi` declares only `StriderError` + the submodules.

- [ ] **Step 3: Verify stubs import-check.** If the repo has a stub check (`grep -rn "pyright\|mypy\|stubtest" crates/strider-py pyproject.toml`), run it. Otherwise at minimum: `python -c "import strider; import strider.ir, strider.lift, strider.cfg, strider.sleigh, strider.reader, strider.opt, strider.pattern, strider.template"` returns cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/strider pyproject.toml
git commit -m "docs(py): regenerate .pyi stubs for the reorganized surface"
```

### Task E3: full gate

- [ ] **Step 1: Rust workspace**

```bash
cargo test --workspace 2>&1 | tail -20    # gate on the real exit code, not the tail text
cargo clippy --workspace 2>&1 | tail -5
cargo fmt --check
cargo doc --workspace --no-deps 2>&1 | tail -5
```
Expected: all succeed (exit 0). Fix any fallout.

- [ ] **Step 2: Python**

```bash
uv run maturin develop
uv run pytest crates/strider-py/tests/python -q
```
Expected: all pass, 0 skips.

- [ ] **Step 3: Final commit / push**

```bash
git push origin stable/2026-07-18-python-api
```

---

## Self-review notes

- **Spec coverage:** §A (D1–D4, C2), §B Q3 (A2/A3/C1/C2/D4), §C viz (B1–B3), §D hygiene (D3/D4), §E Match (A5), §H.1 vocab (A4, B*), pass enumeration (A1–A2). Walks (§F), pattern completion (§G), `constraints.user` (§H), `Node.outputs()`/ordered iterator (§I) are **Plan 2/3**, intentionally out of this plan.
- **Open confirmations carried from the spec:** the `dot::GraphDot` DOT-string API (Tasks B1/B3 Step 3 assume a string-render arm exists; if only file-write exists, add the in-memory render or render-to-temp) — verify with `grep -n "pub fn" crates/dot/src/*.rs` before B1. The stub-gen example name (E2 Step 1) — confirm before running.
- **Type consistency:** `passes`/`post_passes` are `#[getter]` (attributes) everywhere; `to_dot`/`to_html` are `path=None -> Option<String>` on all three classes; `const_int`/`const_bool`/`const_uint` names match between `Node` and `Match`.
