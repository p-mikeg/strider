# Python API reshape: `Program` (loaded ELF) vs `MemoryMap` (raw reader)

> Status: **DESIGN — for review before implementation.** Pre-release branch;
> breaking the Python API is acceptable. No external users to migrate.

## Goal

`strider.load(path)` returns **one object — `Program`** — that *is* the loaded
binary: it wires the code + ROM readers internally and exposes symbols, sizes,
entry point, raw reads, and `analyze()`. `MemoryMap` drops back to what its name
means: a **low-level reader of raw byte regions** for non-ELF / custom-source /
firmware-blob cases. ELF metadata (symbols, entry) no longer lives on
`MemoryMap`.

### Before
```python
s = strider.load("a.out")          # facade, confusingly also named Strider
s.symbol("main")                    # ok, forwarded
size = s.mem.symbol_size("main")    # drop to the low-level MemoryMap
syms = s.mem.symbols()              # ... again
ep   = s.mem.entry_point()          # ... again
```
### After
```python
prog = strider.load("a.out")        # -> Program
prog.symbol("main"); prog.symbol_size("main"); prog.symbols(); prog.entry_point()
prog.read(addr, 16); prog.functions()
a = prog.analyze("main")            # -> Analysis  (readers wired automatically)
# MemoryMap only when you DON'T have an ELF (raw bytes / firmware / custom):
m = strider.MemoryMap(); m.add_region(0x1000, blob); m.read(0x1000, 4)
```

## Layering decision (recommended: **A**)

- **A — `Program` is the Python facade** (rename `_api.Strider` → `Program`),
  holding a Rust `_LoadedElf` for ELF data + a raw `MemoryMap` reader, plus the
  already-working Python arch/cc auto-detection + Thumb logic + `analyze`.
  *Lower Rust churn; keeps the tested Python header/arch logic in place.*
- B — `Program` is a Rust pyclass owning ELF + readers + arch/cc + analyze;
  Python `load` is a thin shim. *Most consolidated, but re-implements the
  arch-detection + Thumb + analyze orchestration in Rust = much more churn.*

Going with **A**.

## Type changes

### `MemoryMap` (Rust `reader.rs`) — slim to a raw reader
KEEP: `new()`, `add_region(start_addr, data)`, `set_endianness(...)`,
`region_count()`, `read(addr, size) -> bytes`, internal `reader_view()` /
`lookup_table()` (the lift consumes these). Fields: `regions`, `table`,
`endianness`.
REMOVE (move to `_LoadedElf`): the `elfs` field, `find_symbol`, `symbol`,
`symbol_size`, `symbol_addr_and_size`, `symbols`, `entry_point`,
`add_region_from_elf`, `apply_elf_relocations`.

### `_LoadedElf` (Rust `reader.rs` or new `program.rs`) — ELF parse + symbols
New `#[pyclass]` (semi-internal; the friendly face is `Program`). Built by a Rust
`load_elf(path, apply_relocations=False) -> _LoadedElf`. Owns the parsed
`object::File`(s) + a `MemoryMap` built from the ELF sections (relocations applied
per flag). Methods:
- `memory_map() -> MemoryMap` (the regions reader, to pass to `run()` as mem/rom)
- `symbol(name) -> int` (raises on miss), `symbol_size(name) -> int | None`,
  `symbols() -> dict[str,int]`, `entry_point() -> int`, `read(addr, size) -> bytes`,
  `endianness() -> str`
- `add_elf(path, apply_relocations=False)` — merge another ELF (shared libs):
  extends the inner `MemoryMap`'s regions + the symbol set (first-loaded wins).

(Keeps the existing `object`-crate symbol/relocation logic; it just moves off
`MemoryMap` onto `_LoadedElf`.)

### `Program` (Python `_api.py`, renamed from `Strider`) — the user-facing object
`strider.load(path) -> Program`. Holds `_LoadedElf` + `arch` + `cc`
(auto-detected, as today). Surface:
- `symbol(name)`, `symbol_size(name)`, `symbols()`, `functions()`,
  `entry_point()`, `read(addr, size)` — delegate to `_LoadedElf`
- `arch`, `cc` properties; `add_elf(path)` for shared libs
- `analyze(name_or_addr, *, function_max_size=…, allow_code_before_start_addr=…,
  rom=…) -> Analysis` — unchanged logic (Thumb-aware), passes
  `_LoadedElf.memory_map()` as both `mem=` and `rom=` to `_ext.run`
- For explicit control (kernel/firmware/custom CC), keep a constructor that takes
  an explicit reader + arch + cc (the current `Strider(mem, arch, cc)` path,
  renamed) — accepts a `MemoryMap` OR a `MemReader` subclass.

## Naming / collision
- The high-level object is now `Program` (distinct name) ⇒ the **two-`Strider`
  collision dissolves**: the low-level Rust lift-driver keeps `Strider(arch,
  sleigh, cc)` with no clash.
- `MemoryMap` keeps its name (now accurate: raw regions).

## Migration (all in this PR)
- **`.pyi`** (`__init__.pyi`): add `Program` + `load()` + `_LoadedElf` (or mark
  internal); slim the `MemoryMap` stub to the raw-reader methods. (This finally
  puts the friendly entry point in the stubs.)
- **Tests**: `test_symbol_size.py`, `test_custom_cc.py`, anything calling
  `MemoryMap.symbol*`/`entry_point`/`add_region_from_elf` → move to `Program`
  (or `_LoadedElf`); keep `MemoryMap` tests for the raw `add_region`/`read` path.
- **Examples** (`examples/python/*.py`): update to `prog = strider.load(...)` +
  `prog.symbol(...)`.
- **README** (`crates/strider-py/README.md` + top-level): update the quickstart +
  the reader section; document `MemoryMap` as the advanced raw reader.
- **CLAUDE.md**: update the strider-py API description (the `load().analyze().find()`
  surface + the `Program`/`MemoryMap` split).
- **Docstring-enforcement test** must stay green (new bindings need docstrings).

## Build sequence (TDD, behavior-checked at each step)
1. Add the Rust `MemoryMap.add_region`-only raw path is already there; add the
   `_LoadedElf` pyclass + `load_elf()` (move the ELF/symbol/reloc logic over),
   keeping `MemoryMap` temporarily dual until step 3. Gate: `cargo test -p strider-py` build.
2. Rename Python facade `Strider` → `Program`; add the delegating
   `symbol_size`/`symbols`/`entry_point`/`read`/`add_elf`; `load()` returns
   `Program` built on `_LoadedElf`. Update `.pyi`. Gate: pytest.
3. Strip ELF methods/`elfs` from `MemoryMap`. Update all call sites/tests/examples.
   Gate: `cargo clippy --workspace`, `cargo test --workspace`, maturin + pytest.
4. Docs: README + CLAUDE.md + examples. Gate: docstring test + `cargo doc -D warnings`.
5. Final review + PR.

## Breaking changes (pre-release; acceptable)
- `MemoryMap` loses `symbol*`/`entry_point`/`add_region_from_elf`/`apply_elf_relocations`.
- The high-level handle is `Program`, not `Strider` (the name `Strider` now refers
  only to the low-level lift-driver).
- `strider.load(path)` return type name changes (`Strider` → `Program`).

## Open question for review
Confirm **Design A** (Python `Program` facade + Rust `_LoadedElf` + slim
`MemoryMap`). If you'd rather a fully-Rust `Program` (Design B), say so — it's a
bigger change but a single self-contained type.
