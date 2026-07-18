# Strider Python Stable API — Design

**Date:** 2026-07-18
**Branch:** `stable/2026-07-18-python-api`
**Status:** design, pending implementation plan

## Goal

Freeze the `strider` Python package into a stable v1 surface. The API has
never been stable, so this is the *first* stabilization, not a break of an
existing promise. That single fact sets the governing rule: we may move,
rename, collapse, and delete freely, updating every internal caller in the
same pass, because no external contract exists yet to honor.

The work has four thrusts:

1. **Reorganize** every symbol into a domain namespace (`strider.ir`,
   `strider.lift`, `strider.cfg`, `strider.sleigh`, `strider.reader`,
   `strider.opt`, `strider.pattern`, `strider.template`).
2. **Clean** the weird/leaked/inconsistent surface (viz method zoo,
   namespace leakage, `Match` builtin-shadowing, the `strider.strider`
   layout leak).
3. **Complete** the pattern DSL so every `NodeKind` is reachable and every
   input/output slot is addressable, retiring the `phi.input(0, anything())`
   workaround.
4. **Expose** the IR walks (`cfg_walk` / `data_walk` / `walk` / `mem_walk`)
   and a join-level user-defined **filter** constraint.

## Governing principles

- **One home per symbol. Zero re-exports.** Each public name is reachable
  by exactly one path. This is the root fix for today's "appears twice"
  smells (`StriderError` ×3, `load_elf_from_*` ×2). The sole top-level
  resident is `StriderError` — the one genuinely cross-cutting type, since
  every namespace raises it.
- **Break freely; no compatibility shims.** Old spellings are deleted, not
  aliased. Internal callers (chiefly the pytest suite) are updated in the
  same change. A shim would be a re-export, which principle one forbids.
- **No O() regressions.** Every new walk/pattern primitive must preserve
  existing traversal complexity.
- **The pytest suite is the spec.** Behavior-preserving changes keep the
  suite green (after mechanical rename updates); new capabilities land with
  new tests, TDD.

## Namespace layout

| Namespace | Contents |
|---|---|
| `strider.ir` | `Function`, `Node` |
| `strider.lift` | `Lifter`, `ElfLifter`, `LifterOptions`, `load_elf` |
| `strider.cfg` | `Cfg`, `CfgOptions` |
| `strider.sleigh` | `SleighArch`, `CallingConvention`, `Sleigh`, `Vn`, `VnSpace` |
| `strider.reader` | `BufferReader`, `MemReader`, `ReadOnlyMemory` |
| `strider.opt` | `OptimizerPipeline` + the pass classes |
| `strider.pattern` | `Pat`, `Capture`, `Match`, builders, `.constraints` |
| `strider.template` | `Template`, builders |
| `strider` (top level) | `StriderError` only |

`load_elf` lives in `strider.lift` because it *returns* an `ElfLifter`.
`Match` lives in `strider.pattern` because it is a query result. `Function`
and `Node` are the IR, so `strider.ir`.

## A. Namespace reorganization

Move each class/function to its domain module as the tabled layout dictates.
No symbol is re-exported at top level (except `StriderError`). The mechanics:

- The native extension module is renamed from `strider` to `strider._strider`
  (`#[pymodule] fn _strider` + maturin `module-name = "strider._strider"`).
  This eliminates the `strider.strider` self-submodule leak (finding #8) as a
  side effect — the cdylib is now reached only via the private `_strider`
  handle used by the package facade.
- Native classes are registered **directly on their domain submodule** on
  the Rust side — each domain module (`ir`, `lift`, `cfg`, `sleigh`,
  `reader`) becomes a native submodule of `_strider`, exactly as `opt` /
  `pattern` / `template` already are, and each `#[pyclass]` is added to its
  home submodule instead of the top-level module. This is a single-home
  *registration*, not a re-export: a class exists on one module, full stop.
- The pure-Python members (`ElfLifter`, `load_elf`, defined in `_api.py`)
  are defined once and bound into `strider.lift` — their one home.
- The package `__init__.py` exposes only `StriderError` and the domain
  submodules; it performs no symbol-level re-exporting.
- `.pyi` stubs are restructured to match the new module tree.

## B. Cleanup items (Q3)

| Item | Change |
|---|---|
| `viz_standalone_js` | Rename `_viz_standalone_js` (internal explorer plumbing). |
| `strider.strider` | Eliminated by the cdylib rename in §A. |
| `StriderError` ×3 | Single home: top-level `strider.StriderError`. Delete the `strider.errors` submodule. |
| `VnSpace.register()` etc. | Replace the four classmethod constructors with constants `VnSpace.REGISTER / RAM / CONST / UNIQUE`. |
| `pass_count` / `post_pass_count` | **Delete.** Add `OptimizerPipeline.passes` / `.post_passes`, each returning `list[str]` of pass names. Count becomes `len(...)`. Requires a new `fn name(&self) -> &'static str` on the `Optimizer` and `PostOptimizer` traits (`strider-opt`), implemented once per pass (~13 one-liners). |
| `load_elf` ×3 | Collapse to `load_elf(path, *, from_segments=True, apply_relocations=True, arch=None, cc=None)`. Delete `load_elf_from_segments` / `load_elf_from_sections`. The Rust `ElfRegionSource {Segments, Sections}` enum stays internal, selected by the `from_segments` flag. |

## C. Visualization / serialization unification

Thirteen renderer method names across `Function` / `Cfg` / `Lifter` collapse
to three verbs. Rationale: pretty-vs-raw is already determined by the
receiver (raw needs no Sleigh → `Function`; pretty needs one →
`Cfg` / `Lifter`), so the name must not re-encode it; and string-vs-file is a
return-type split, expressible with one optional `path` argument.

**Rule:** `to_dot(…, path=None) -> str | None` and
`to_html(…, path=None) -> str | None` — return the string when `path is
None`, else write the file and return `None`. Neighborhood variant is
`neighborhood_dot(…)`.

| Class | Before | After |
|---|---|---|
| `Cfg` | `to_dot(path)` | `to_dot(path=None)` |
| `Cfg` | `to_html(path, style)` + `html_str(style)` | `to_html(path=None, style=None)` |
| `Cfg` | `neighborhood_dot(...)` | unchanged |
| `Cfg` | `raw_neighborhood_dot(...)` | **drop** (use `Function.neighborhood_dot` for raw) |
| `Cfg` | `region_texts()` | `_region_texts()` (explorer-internal) |
| `Function` | `raw_dot_str()` + `to_raw_dot(path)` | `to_dot(path=None)` |
| `Function` | `raw_html_str()` + `to_raw_html(path)` | `to_html(path=None)` |
| `Function` | `raw_neighborhood_dot(...)` | `neighborhood_dot(...)` |
| `Lifter` | `dump_dot(fn, path)` | `to_dot(fn, path=None)` |
| `Lifter` | `dump_html(fn, path, style)` + `html_str(fn, style)` | `to_html(fn, path=None, style=None)` |
| `Lifter` | `neighborhood_dot(fn, ...)` | unchanged |
| `Lifter` | `visualize(target, ...)` | unchanged (distinct: opens a blocking browser server) |

This also fills a real gap: there is currently no way to obtain a *pretty*
DOT string; the unified `to_dot(path=None)` on `Cfg` / `Lifter` provides it.

## D. Namespace hygiene

- `del _importlib, _sys, _ext` at the end of `__init__.py`; define `__all__`
  pinning the intended top-level surface (`StriderError` plus the domain
  submodules).
- Drop the stray `import _api as _api` module-object binding; keep only the
  `from ._api import …` re-exports (which now target domain submodules).
- Leave `_rebuild`, `ElfLifter._arch/_cc/_elf` as-is: an unavoidable PyO3
  `#[pyclass(subclass)]` leak, already correctly underscore-private and
  excluded from `.pyi`.

## E. `Match` reader alignment

Rename `Match.int` / `Match.bool` / `Match.uint` to `Match.const_int` /
`Match.const_bool` / `Match.const_uint`, matching `Node`'s identical readers
and removing the `int` / `bool` builtin-name shadowing.

## F. IR walks

**Home:** the walk primitives live in `strider-ir/src/walk`. The Rust side
already has `cfg_reachable` (control-only) and `walk_graph` (data-in +
control-out, currently `pub(crate)`); the latter is promoted to a public
node-seeded entry point.

**Python surface** (methods on `strider.ir.Function`, returning ordered
`list[Node]`):

| Method | Traversal | Backing |
|---|---|---|
| `cfg_walk()` | control-only reachability (CFG skeleton) | `cfg_reachable` |
| `data_walk()` | data-in + control-out from entry | `walk_graph` |
| `walk(node)` | same, seeded from a given node | `walk_graph` (seeded) |
| `mem_walk()` | memory-token def-use chain | **new primitive — see below** |

**`mem_walk` requires a primitive that does not yet exist in `strider-ir`.**
Memory-chain traversal today lives in `strider-opt`'s `memory_ssa`, and
`strider-ir` cannot depend on `strider-opt` (DAG direction). The `memory_ssa`
file mixes two concerns: the **token-chain walk** (follow
`Store` → `Load` → `MemPhi` def-use edges — pure graph traversal) and the
**alias reasoning** (may-clobber, `SpAnalyzer`, `stack_offsets` — depends on
optimization/stack analysis). `mem_walk` needs only the walk half.

> **Verification spike (blocking, read-only, before any code):** confirm the
> token-chain walk is cleanly separable from the alias reasoning. Expected
> outcome: extract the walk into `strider-ir/src/walk`, leave the alias
> reasoning in `strider-opt` (calling the extracted walk). If they prove
> entangled, `mem_walk` is re-scoped or deferred; the other three walks ship
> regardless.

## G. Pattern DSL completion

Two additions make every node and every slot addressable, retiring the
`phi.input(0, anything().capture(c))` workaround.

### G.1 Generic slot layer on `Pat`

Add to the base pattern (Rust `strider-pattern`, mirrored in Python):

- `input(i, sub)` — operand slot `i` is matched by `sub`.
- `any_input(sub)` — some operand is matched by `sub`.
- `output(j, sub)` — output value at slot `j` is matched/bound by `sub`.
- `any_output(sub)` — some output value is matched/bound by `sub`.

`PhiPat.input` / `any_input` become the general case of this layer.
**Semantics are value-binding, not forward reachability:** `output(j, sub)`
binds/constrains an output `ValueId` the matcher already holds at the node —
it does **not** walk forward to consumers. Forward reachability remains the
job of the join constraints (`dominates`, and any future `reaches`); the slot
layer must not duplicate it. Because outputs are already materialized at a
matched node, indexed and any-slot forms cost the same — both ship.

**Named accessors stay as sugar** (`store.addr`, `if_else.cond`, …) and
compile down to the generic layer. Add named sugar to the new kind roots
where it fits.

**Commutative matching is unchanged.** `any_input` is a *complement*
(bind-one-operand-ignoring-position), not a replacement for commutative
matching (which binds *both* operands trying both orderings). Note
`add.any_input(a).any_input(b)` is unsound (nothing prevents both binding the
same operand) — real commutative ops keep their auto-both-orderings.

### G.2 Kind roots

Add pattern-root builders for the six `NodeKind`s that have none, in both
Rust and Python: `region`, `entry`, `initial_memory`, `segment_op`,
`cpool_ref`, `new`. Once every kind is a root and slots are generic, the
missing "kind filter on a wildcard" is subsumed — `phi.input(0, region())`
expresses "operand 0 is produced by a Region" directly, so no separate
`.of_kind()` primitive is added.

## H. Join-level user-defined filter constraint

Add `strider.pattern.constraints.user(predicate)`: a **filter-only** join
constraint that receives the bound captures and returns `bool`. It sits
beside `dominates` / `negate` / `all_of` / `any_of`. It is a pure filter
(`holds -> Option<bool>` under the existing three-valued eval), so it needs
no binder machinery and cannot introduce the filter/binder confusion the
constraint layer is being kept clear of. Per-match arbitrary logic already
exists via `.when()`; this fills the *relational* (cross-capture) gap.

## I. Runtime-inspection additions (Q4)

- `Node.outputs()` — the missing symmetric counterpart to `Node.inputs()`
  (single-hop forward: the value outputs of this node). Required so the
  runtime inspector can see what the new pattern output-slots match.
- `Function` ordered reachable-node iterator — returns reachable nodes in a
  defined order (pairs with the new walks); today only `node_ids()` exists,
  which is unordered arena contents.

## Out of scope / deferred

- **Programmatic CFG block-edge access** (`Cfg` successors/predecessors) — a
  larger new `Cfg` surface with no current consumer. Deferred until a use
  appears.
- **Forward-reachability output matching** — explicitly *not* built into the
  slot layer; it belongs to join constraints.
- **Per-pass richer introspection** (config, ordering) beyond `name()` — the
  name list is the stable v1 ceiling.

## Testing & rollout

- **Rust:** new unit tests for the promoted/extracted walks, the six kind
  roots, the generic slot layer (input/output, indexed + any), and the
  `constraints.user` filter, TDD. `cargo test --workspace` + `clippy -D
  warnings` + `cargo doc` gate.
- **Python:** every renamed/moved symbol updates its callers across the
  pytest suite (the mechanical half); new capabilities land with new pytest
  cases. **Rebuild via `uv run maturin develop` from the workspace root
  before pytest** (stale-`.so` trap) and verify mtime.
- **Full gate before merge:** `cargo test --workspace`, `clippy`, `fmt`,
  `cargo doc`, and `pytest` all green.

## Implementation ordering (for the plan)

1. Verification spike: `memory_ssa` separability (gates `mem_walk`).
2. Namespace reorg + cdylib rename + `__all__`/hygiene (largest mechanical
   churn; do it first so later work lands in final locations).
3. Q3 cleanups (viz_standalone_js, StriderError, VnSpace constants, load_elf
   collapse, pass enumeration).
4. Viz/serialization unification (§C) + `Match` alignment (§E).
5. Walks (§F) + `Node.outputs()` / ordered iterator (§I).
6. Pattern DSL completion (§G) + `constraints.user` (§H).
7. Full gate.
