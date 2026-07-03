# strider-py p-code: CFG lookup + entry-replay (+ analyze returns the CFG)

Follow-up (same branch). Make `analyze` return the CFG alongside the Function
(so the CFG is available on every analysis), and make p-code rendering use the
lifter's own decodes instead of an ad-hoc fresh Sleigh, so it is correct on
context-dependent arches (ARM/Thumb, MIPS16). p-code has two homes only: `Cfg`
(look up the already-built CFG) and `Lifter` (linear decode from entry).

## `analyze` returns the CFG

Change the return of both analyze entry points to a 3-tuple:

```python
class Lifter:
    def analyze(self, entry, cc, opts=LifterOptions()) -> tuple[Cfg, Function, list[int]]: ...
class ElfLifter(Lifter):
    def analyze(self, target, cc=None, opts=LifterOptions()) -> tuple[Cfg, Function, list[int]]: ...
```

Order: `(cfg, function, unresolved_indirect_branches)`. Still a plain tuple (no
`RunResult` wrapper). The returned `cfg` is the **final** iteration's CFG — the
resolved one that matches the returned `function` (the analyze fixed-point loop
rebuilds the CFG as indirect branches resolve; the last rebuild is the SSoT).

**Rust-core change:** `strider_orchestrator::Strider::build_lift` and `analyze`
return the final `strider_cfg::Cfg`; `AnalyzeResult` gains a `cfg` field. The
Python `Lifter.analyze` wraps it as `PyCfg` and prepends it to the tuple.

Every `function, unresolved = analyze(...)` call site migrates to
`cfg, function, unresolved = analyze(...)` (or `_cfg, ...` where the CFG is
unused).

## Motivation

A `Cfg`'s regions already hold every decoded machine instruction
(`strider_cfg::RegionInstruction { addr: PcodeInsnAddr, insn: rsleigh::Insn }`),
decoded in the exact context the real lift used (sequential-within-region decode
from entry). So the audit-trail p-code for a node is a LOOKUP in the CFG, not a
re-decode. The current `fingerprint_pcode` builds a fresh Sleigh with default
context — correct for x86_64 but wrong for a mid-function Thumb switch. The
free-function `pcode_at`/`pcode_at_addrs` have the same fresh-Sleigh flaw.

## New API

### `Cfg`

```python
class Cfg:
    def pcode_at(self, addr: int) -> str | None: ...
    def fingerprint_pcode(self, node: Node) -> list[tuple[int, str]]: ...
```

- `pcode_at(addr)`: collect every `RegionInstruction` whose machine address ==
  `addr` (a machine instruction lifts to ≥1 p-code op — one `RegionInstruction`
  per op, `PcodeInsnAddr.insn_index` 0..N), join their `insn` renderings with
  `"; "` (empty string for a machine insn that lifts to no p-code, e.g.
  `endbr64`). Returns `None` when `addr` is not present in any region.
  Implementation: build a `machine_addr -> joined_pcode` map from the CFG once
  (single pass over regions), then O(1) lookup; cache it on the `PyCfg`.
- `fingerprint_pcode(node)`: read `node.fingerprint()` (the node's sorted machine
  addresses), map each through `pcode_at`, return `[(addr, text), ...]` sorted by
  addr. `text` is `""` for a no-p-code insn; an address not in the CFG is skipped
  (or its text is `""` — implementer picks, but be consistent and document). This
  is the audit trail — correct by construction (the exact lift-time decodes).

### `Lifter`

```python
class Lifter:
    def pcode_at(self, entry: int, addr: int) -> str: ...
```

- `pcode_at(entry, addr)`: decode LINEARLY from `entry`, one machine instruction
  at a time (advancing by each insn's machine length), on a Sleigh, replaying
  context-register state, until the cursor reaches `addr`; return that
  instruction's p-code (ops joined `"; "`). Reuse the Lifter's owned Sleigh so
  GHIDRA's `DisassemblyCache` (warm from the real lift) makes the sweep cheap —
  BUT do not leave the shared Sleigh's context dirtied for later `analyze`
  calls (clone the Sleigh for the sweep, or save/restore, mirroring the
  fingerprint fix). Raise `StriderError` if `addr < entry`, or if the linear
  sweep steps PAST `addr` without landing on it (misaligned target).

## Removals + migration

- DELETE the free functions `strider.pcode_at` / `strider.pcode_at_addrs`
  (`crates/strider-py/src/pcode.rs` — keep only the internal `build_sleigh` /
  `lift_one_text` helpers if the new `Lifter.pcode_at` reuses them).
- DELETE `Lifter.fingerprint_pcode` (moved to `Cfg.fingerprint_pcode`).
- Migrate callers: `ElfLifter.pcode(addr, count)` (if it wraps the old
  `pcode_at`) — reimplement over the new primitives (e.g. iterate
  `Lifter.pcode_at(entry, ...)` or expose a range via the CFG) or drop the
  `count` form if unused; decide in the plan. `test_pcode.py`,
  `test_standalone_strider_by_address`, and any Analysis-era p-code test move to
  `cfg.fingerprint_pcode` / `cfg.pcode_at` / `lifter.pcode_at`.
- `.pyi` (`__init__.pyi`), `README.md`, `CLAUDE.md` updated in lockstep.

## Correctness note

`Cfg.fingerprint_pcode`/`pcode_at` are exact (the CFG's stored decodes).
`Lifter.pcode_at` is a linear sweep from entry — correct when the target is
reachable by the linear instruction stream from entry (the common case and what
the lifter assumes: context fixed per entry, sequential-within-region). It does
NOT follow control flow; a target only reachable via a branch with an
intervening mode switch off the linear path is out of scope (consistent with the
lifter's own fixed-per-entry assumption). Document this boundary.

## Verification

`cargo test --workspace` 0 failed, `cargo clippy --workspace` clean, `pytest`
(from workspace root) green + examples. Add a test that `cfg.fingerprint_pcode(node)`
matches the expected p-code for a known x86_64 fixture node, and that
`lifter.pcode_at(entry, addr)` raises for `addr < entry`.
