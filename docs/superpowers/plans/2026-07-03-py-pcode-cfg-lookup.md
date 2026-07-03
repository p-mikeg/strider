# strider-py p-code CFG-lookup + entry-replay — Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** Make `analyze` return `(Cfg, Function, list[int])`, then move p-code rendering to `Cfg.pcode_at`/`Cfg.fingerprint_pcode` (lookup in that CFG) and `Lifter.pcode_at(entry, addr)` (linear decode from entry); remove the fresh-Sleigh free functions and `Lifter.fingerprint_pcode`.

**Spec:** `docs/superpowers/specs/2026-07-03-py-pcode-cfg-lookup-design.md`.

**Architecture:** Binding-only. The `Cfg` holds `RegionInstruction { addr: PcodeInsnAddr, insn: rsleigh::Insn }` per decoded op — the CFG lookup reads those. `Lifter.pcode_at` reuses the owned Sleigh (clone for the sweep to avoid dirtying it).

## Global Constraints

- PyO3 0.22. Build + test from WORKSPACE ROOT: `cd /mnt/c/Users/mikeg/Documents/strider && uv run maturin develop && uv run pytest crates/strider-py/tests/python -q`. Run pytest to COMPLETION in the foreground before reporting. Clippy clean. Current pytest baseline 883 passed / 2 skipped.
- `Cfg.pcode_at`/`fingerprint_pcode` are exact CFG lookups (no re-decode). `Lifter.pcode_at` is a LINEAR sweep from entry (no control-flow following); raise `StriderError` if `addr < entry` or the sweep steps past `addr`. Don't leave the Lifter's shared Sleigh context dirty (clone for the sweep).

---

## Task 1: `analyze` returns the final CFG — `(Cfg, Function, list[int])`

**Files:** `crates/strider-orchestrator/src/lib.rs` (`build_lift`, `analyze`, `AnalyzeResult`), `crates/strider-py/src/strider_cls.rs` (`Lifter.analyze` wraps + prepends the Cfg), `crates/strider-py/src/_api.py`-equivalent `strider/_api.py` (`ElfLifter.analyze`), `strider/__init__.pyi`, tests/examples using `analyze`.

**Interfaces (target):**
```python
# Lifter.analyze(entry, cc, opts=LifterOptions()) -> (Cfg, Function, list[int])
# ElfLifter.analyze(target, cc=None, opts=LifterOptions()) -> (Cfg, Function, list[int])
```
- Consumes: the orchestrator's per-iteration CFG. Produces: the final (resolved) CFG as element 0.

- [ ] **Step 1 (RED):** Failing pytest:
```python
def test_analyze_returns_cfg_first():
    lift = strider.load_elf_from_segments(FIXTURE)
    cfg, function, unresolved = lift.analyze("add")
    assert isinstance(cfg, strider.Cfg)
    assert function.node_count() > 0 and isinstance(unresolved, list)
```
- [ ] **Step 2:** Run — expect failure (2-tuple unpacks wrong / no Cfg).
- [ ] **Step 3 (Rust):** `Strider::build_lift` returns the `strider_cfg::Cfg` it built (thread it out); `Strider::analyze` keeps the FINAL iteration's Cfg and puts it in `AnalyzeResult { cfg, function, unresolved_indirect_branches }` (add the field). Update the in-crate consumers of `AnalyzeResult` (there may be Rust tests — migrate them).
- [ ] **Step 4 (Python):** `Lifter.analyze` (strider_cls.rs) wraps the returned Cfg as `PyCfg` and returns `(cfg, function, unresolved)` (3-tuple, cfg first). `ElfLifter.analyze` in `_api.py` forwards the 3-tuple.
- [ ] **Step 5:** Update `strider/__init__.pyi` (both analyze signatures → `tuple[Cfg, Function, list[int]]`). Migrate EVERY `function, unresolved = ...analyze(...)` call site in `tests/python/` + `examples/python/` to `cfg, function, unresolved = ...` (use `_cfg` where unused). grep `= .*analyze(` and `, _unresolved = ` etc.
- [ ] **Step 6:** Gate green (ROOT, foreground pytest; cargo test --workspace for the Rust change). Commit `feat(py): analyze returns (Cfg, Function, unresolved)`.

---

## Task 2: Cfg.pcode_at + Cfg.fingerprint_pcode + Lifter.pcode_at; remove old; migrate; gate

**Files:** `crates/strider-py/src/cfg.rs` (PyCfg — add pcode_at/fingerprint_pcode; needs access to the underlying `strider_cfg::Cfg` regions), `crates/strider-py/src/strider_cls.rs` (add `Lifter.pcode_at`; remove `Lifter.fingerprint_pcode`), `crates/strider-py/src/pcode.rs` (remove the `#[pyfunction]` `pcode_at`/`pcode_at_addrs`; keep internal helpers if reused), `src/lib.rs` (drop the removed pyfunction registrations), `strider/__init__.pyi`, `_api.py` (ElfLifter.pcode), `README.md`, `CLAUDE.md`, tests/examples.

**Interfaces (target):**
```python
class Cfg:
    def pcode_at(self, addr: int) -> str | None: ...            # lookup; None if absent
    def fingerprint_pcode(self, node: Node) -> list[tuple[int,str]]: ...  # per-fingerprint-addr lookup
class Lifter:
    def pcode_at(self, entry: int, addr: int) -> str: ...       # linear from entry; raises if addr<entry
# REMOVED: strider.pcode_at, strider.pcode_at_addrs (free fns), Lifter.fingerprint_pcode
```

- [ ] **Step 1 (RED):** Failing pytest in `tests/python/test_pcode.py` (or new `test_pcode_cfg.py`):
```python
def test_cfg_pcode_at_and_fingerprint_lookup():
    lift = strider.load_elf_from_segments(FIXTURE)
    cfg, g, _ = lift.analyze("add")           # analyze now returns the CFG (Task 1)
    # some reachable node with a fingerprint
    nid = next(i for i in g.node_ids() if g.node(i).fingerprint())
    fp = cfg.fingerprint_pcode(g.node(nid))
    assert fp and all(isinstance(a, int) and isinstance(t, str) for a, t in fp)
    assert cfg.pcode_at(fp[0][0]) is not None
def test_lifter_pcode_at_rejects_addr_before_entry():
    lift = strider.load_elf_from_segments(FIXTURE)
    entry = lift.symbol("add")
    with pytest.raises(strider.errors.StriderError):
        lift.pcode_at(entry, entry - 4)
```
- [ ] **Step 2:** Run — expect failure (methods missing).
- [ ] **Step 3:** `Cfg.pcode_at`: ensure `PyCfg` can reach the inner `strider_cfg::Cfg` regions. Build a `machine_addr -> joined_pcode` map once (iterate regions → each `RegionInstruction`; group by `addr.machine_addr`; join `insn` renderings per machine addr with `"; "`, empty when a machine insn has no p-code), cache it on `PyCfg` (e.g. `OnceCell`). Lookup returns `Some(text)` / `None`.
- [ ] **Step 4:** `Cfg.fingerprint_pcode(node)`: read `node`'s fingerprint machine-addresses (reuse the same accessor `Node.fingerprint` uses), map each via `pcode_at`, return sorted `[(addr, text)]` (skip addresses absent from the CFG OR emit `""` — pick one, document in the docstring).
- [ ] **Step 5:** `Lifter.pcode_at(entry, addr)`: clone the Lifter's Sleigh (as `fingerprint_pcode`'s fix did), lift_one from `entry`, advance by each insn's machine length, until the cursor machine-addr == `addr` → return that insn's p-code (`lift_one_text`); if cursor passes `addr` → `StriderError` (misaligned); if `addr < entry` → `StriderError` up front.
- [ ] **Step 6:** Remove the `#[pyfunction]` `pcode_at`/`pcode_at_addrs` and their `add_function` registrations in `lib.rs`; remove `Lifter.fingerprint_pcode`. Reimplement/adjust `ElfLifter.pcode(addr, count)` in `_api.py` over the new primitives (a small loop of `lifter.pcode_at(entry, ...)` if the entry is known, else drop the `count` form if it has no caller — check `git grep`). 
- [ ] **Step 7:** Update `strider/__init__.pyi` (add the three methods; drop the removed ones). Migrate every test/example using `strider.pcode_at(`/`pcode_at_addrs(`/`.fingerprint_pcode(` (was on Lifter/Analysis) to the new homes.
- [ ] **Step 8 (gate, from ROOT, foreground):**
```
cd /mnt/c/Users/mikeg/Documents/strider
cargo test --workspace 2>&1 | grep -E "test result: FAILED|FAILED" || echo cargo-OK
cargo clippy --workspace --all-targets 2>&1 | grep -E "warning:|error" | grep -v generated || echo clippy-clean
uv run maturin develop && uv run pytest crates/strider-py/tests/python -q
for ex in crates/strider-py/examples/python/*.py; do uv run python "$ex" >/dev/null 2>&1 || echo "FAILED: $ex"; done
```
Expected: cargo 0 failed, clippy clean, pytest green, examples all pass.
- [ ] **Step 9:** Commit `feat(py): p-code via Cfg lookup + Lifter.pcode_at(entry); drop fresh-Sleigh pcode helpers`.

## Self-review

- Spec goals: Cfg.pcode_at (S3), Cfg.fingerprint_pcode (S4), Lifter.pcode_at entry-replay (S5), removals + migration (S6-7), gate (S8) — all covered.
- Sleigh-context-dirty caveat handled (S5 clones). `ElfLifter.pcode` migration explicit (S6).
