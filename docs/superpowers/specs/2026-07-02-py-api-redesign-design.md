# strider-py API redesign

Redesign the Python surface for one obvious way to do each thing, a single
source of truth for every read, and names that don't fight Python keywords.
Pure binding-layer work except where noted **(Rust)**.

## Goals

- One lift handle, one result shape.
- `cc` is a per-function input, not handle state — and it moves into the
  `Function` (no clone).
- Every read has exactly one home (Graph or Node).
- Match vs build (Template) is explicit.
- No keyword-collision underscores.

---

## 1. Calling convention is per-function; move ownership **(Rust + binding)**

Today the Python handle freezes `cc` at construction and
`FunctionBuilder::new` does `Function::new(cc.clone(), …)`.

- `cc` becomes a parameter of `analyze(entry, cc, …)` (and the ELF
  `analyze(target, cc, …)`).
- Thread `cc` **by value** through the lift and move it into
  `Function.default_cc` at the end (it is only read by-ref during build), so
  the clone is gone. `per_address_ccs` stays a borrowed override map; only the
  default cc is moved in.

## 2. One lift handle: `strider.lifter(arch, mem, rom=None) -> Lifter`

Collapse `strider()`, `run()`, the `Strider` class, and the low-level
`Lifter` class into a single `Lifter` (backed by the Rust `Strider`).

```
class Lifter:
    def build_cfg(self, entry, *, allow_code_before_start_addr=False,
                  function_max_size=None) -> Cfg: ...
    def analyze(self, entry, cc, *, function_max_size=None,
                allow_code_before_start_addr=False, compact=True,
                per_address_ccs=None, calls_clobber=True,
                assume_distinct_sp_bases_disjoint=False,
                alias_mode="...") -> tuple[Function, list[int]]: ...
```

Removed: `strider.strider`, `strider.run`, `strider.Strider`, the old
low-level `Lifter`, `RunResult`, `AnalyzeOutcome`.

Naming note: it optimises + resolves, not merely "lifts". Keeping `Lifter`
per the owner's call; `Analyzer` is the alternative if we reconsider.

## 3. ELF loaders → `ElfLifter(Lifter)`

Replace `load_elf` with two explicit loaders over the two real ELF views:

```
def load_elf_from_segments(path, *, apply_relocations=True,
                           arch=None, cc=None) -> ElfLifter   # PT_LOAD runtime view
def load_elf_from_sections(path, *, apply_relocations=True,
                           arch=None, cc=None) -> ElfLifter   # section link-time view
```

`ElfLifter` **extends** `Lifter` (inherits `build_cfg` / `analyze` over the
wired mem+rom) and **adds only** the ELF surface:

```
class ElfLifter(Lifter):
    def symbols(self) -> dict[str, int]: ...
    def symbol(self, name) -> int: ...
    def symbol_size(self, name) -> int | None: ...
    def entry_point(self) -> int: ...
    def functions(self) -> Iterable[str]: ...
    def analyze(self, target: str | int, cc=None, **opts) -> tuple[Function, list[int]]: ...
```

`analyze(target)` resolves a symbol name → address then delegates to the base;
`cc` defaults to the ELF-derived one when omitted.

**DECISION (was Q3):** keep a thin `load_elf(path, **opts)` that delegates to
`load_elf_from_segments` (the common "just load it" path). Explicit loaders for
when the view matters.

## 4. No result wrappers — return `(graph, unresolved)`

`analyze(...) -> tuple[Function, list[int]]` (the IR graph and the list of
unresolved indirect-branch addresses). `RunResult` / `AnalyzeOutcome` deleted.

**DECISION (was Q4):** anything that needs the `Sleigh` stays on the handle
that owns it (the `Lifter`); anything Sleigh-free stays on the `Function`.
- On `Node`/`Function` (Sleigh-free): addr-only `fingerprint()`, and the raw
  dumps `raw_dot_str` / `raw_html_str` / `to_raw_dot` / `to_raw_html` (they
  render the graph exactly as stored, no register names).
- On `Lifter` (needs `Sleigh`): `fingerprint_pcode(node) -> [(addr, text)]`
  (renders p-code text) and the **pretty** renders `dump_html(function, path,
  style=…)` / `dump_dot(function, path)` / `html_str(function, style=…)` (they
  inline constants and resolve register names via `Sleigh`).

`Cfg` / `Sleigh` are reached via the `Lifter`, not bundled into a result
object.

## 5. Split match vs build: `strider.pattern` and `strider.template`

Rust already separates `Pattern` (match) from `Template` (build); Python fuses
them (one `PyPat` compiles to either). Make the boundary explicit:

- `strider.pattern` — the match DSL (all current constructors), producing
  `Pat`. Match-only affordances (`.when`, commutative matching, `.ordered`)
  live here.
- `strider.template` — the build DSL, producing a distinct `Template`. Only the
  build-valid subset (no predicates, no commutativity).
- `rewrite(find: Pat, replace: Template)` and `rewrite_all([(Pat, Template)])`
  become type-checked.

**DECISION (was Q5):** ship the **typed-`Template` middle ground** first — one
shared set of leaf constructors, but a distinct `Template` type and a
`strider.template` namespace for the build side, so the type signatures carry
the distinction without a full 2× duplication of every builder. Revisit a full
module split if the shared leaves prove confusing.

## 6. All reads on Graph / Node (single source of truth)

Delete the id-keyed reader methods on `Function`
(`node_kind(id)`, `asm_fingerprint(id)`, `call_other_name(id)`,
`wide_const_bytes(id)`). `Function` keeps only graph-level surface:

```
class Function:  # the graph
    def node(self, id) -> Node
    def node_ids(self) -> list[int]
    def node_count(self) -> int
    def count_regions(self) -> int
    # queries + mutation + Sleigh-free raw dumps + clone + optimize/validate/compact
    find_all / find_one / find_joined / rewrite / rewrite_all / clone
    raw_dot_str / raw_html_str / to_raw_dot / to_raw_html
    optimize / reoptimize / validate / compact
```

Pretty (register-named) dumps move to the `Lifter` — see #4.

All per-node facts live on `Node`: `kind()`, `inputs()`, `const_uint()`,
`const_int()`, `const_bool()`, `fingerprint()`, `wide_const_bytes()`,
`call_other_name()`, plus the op-variant readers (`int_binary_op()`,
`int_cmp_op()`, …) moved off `Match`.

**DECISION (was Q6):** the **logic** lives on `Node` (SSoT). `Match` keeps thin
sugar *forwarders* (`m.uint("x")` → `m.node("x").const_uint()`) so ergonomics
survive, but there is one implementation. `Match` itself is: `root`,
`node(key) -> Node | None`, `has(key)` / `m[key]` / `key in m`, plus the
forwarders.

## 7. Remove `PartialMatch`

It exists only as the `.when(predicate)` argument (captures bound so far) and
duplicates `Match`'s accessors. `Match` already returns `None`/`False` for
unbound keys, so `.when(f: Callable[[Match], bool])` subsumes it. Delete
`PartialMatch`; predicates receive a `Match`.

## 8. Naming — descriptive, no keyword underscores

Integer ops join the existing `int_eq`/`int_lt` family:

| old | new |
|-----|-----|
| `and_` | `int_and` |
| `or_` | `int_or` |
| `xor` | `int_xor` |
| `not_` / `bit_not` | `int_not` (consolidated `~x`; `bit_not` removed) |
| `if_` | `if_else` |
| `VnSpace.const_` | `VnSpace.const` |
| `any_()` | `anything()` |
| `strider.strider()` | `strider.lifter()` (from #2) |

Boolean ops unchanged (`bool_and` / `bool_or` / `bool_xor` / `bool_not`).
Split is explicit: `int_not` = bitwise `~x` (any width); `bool_not` = logical
`xor(x, 1:I1)`.

---

## Impact / sequencing

- **Rust:** #1 (cc by-value → moved into `Function`); #5 (expose `Template` /
  build-side to Python distinctly). Everything else is binding-layer.
- **Binding + stubs:** rewrite `src/*.rs` `#[pyclass]`/`#[pyfunction]` surface,
  regenerate `.pyi`, update every example and pytest in lockstep (breaking).
- **Verification gate:** `cargo test --workspace`, `cargo clippy`, and the full
  `pytest` suite green; the examples in `crates/strider-py/examples/` updated
  and runnable.

This is a breaking API change; it targets `develop`, not a patch release.
