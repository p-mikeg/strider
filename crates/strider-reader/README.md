# `reader` — memory readers for binary analysis

Loads ELF binaries and exposes them through two complementary interfaces:
`rsleigh::MemReader` (for Sleigh's instruction fetch) and `ReadOnlyMemory`
(for the [`opt::LoadReadOnly`](../opt) pass that folds compile-time-constant
loads). Same regions, two views.

## Public surface

- `MemRegion` — one contiguous range of bytes loaded at a fixed virtual
  address. Constructor `MemRegion::new(start_addr, data)` rejects any pair
  that would overflow `u64` so all downstream arithmetic is plain `u64`.
  `start_addr`, `data`, `data_mut`, `end_addr`, `contains`, `read`.
- `MemRegionsLookupTable` — `BTreeMap<u64, MemRegion>` keyed by start
  address. `new(regions)`, `read(addr, out)`. O(log n) in the common
  non-overlapping case; resolves overlaps by walking candidates from
  highest `start_addr <= addr` downward.
- `ReadOnlyMemory` trait — `read(space, addr, size) -> Option<u64>`.
  Blanket impls for `Arc<T>` and `Box<T>` so callers can wrap a shared
  ROM and feed it directly to `LoadReadOnly`.
- `MemReadError` — `std::error::Error`-implementing wrapper around
  `anyhow::Error` so reader impls satisfy rsleigh 4.0.0's `MemReader::Err`
  bound while keeping ergonomic `anyhow!` / `?` usage. `From<anyhow::Error>`
  conversion provided.
- `elf::ElfFileMemReader` — owns a `MemRegionsLookupTable`, implements
  `rsleigh::MemReader` and `ReadOnlyMemory`.
- `elf::load_elf(path)` — parses an ELF into an `object::File<'static>`
  (re-exported at the crate root).
- ELF region helpers (`elf` module): `elf_segment_to_mem_region`,
  `elf_section_to_mem_region`, `elf_segments_to_mem_regions`,
  `elf_sections_to_mem_regions`,
  `elf_get_executable_segments_as_mem_regions`,
  `elf_get_executable_sections_as_mem_regions`,
  `elf_get_code_and_readonly_sections_as_mem_regions`,
  `elf_get_allocatable_file_backed_sections_as_mem_regions`.
- ELF relocation appliers: `apply_elf_relocations(regions, &obj)`,
  `apply_elf_relocations_autoload(regions, &obj)` (the latter scans the
  ELF's dynamic relocations, identifies any site addresses not yet covered
  by `regions`, and lazily extends with the section that owns each missing
  site — e.g. `.got.plt` — before applying), and the convenience
  `elf_load_with_relocations`.
- `RelocationStats` — applied / skipped / failed counters from the
  relocation appliers.
- `Result<T>` alias (`anyhow::Result<T>`).

## Architecture

`src/lib.rs` defines the backend-independent primitives: `MemRegion`,
`MemRegionsLookupTable`, `ReadOnlyMemory`, `MemReadError`. New reader
backends (raw blobs, PE, Mach-O, …) can live alongside `elf` and reuse
the same primitives.

`src/elf.rs` is the ELF backend. It parses an `object::File`, picks out
the relevant segments / sections, builds `MemRegion`s, and stores them in
a `MemRegionsLookupTable` inside an `ElfFileMemReader`. The lookup
table indexes by start address so `read` can answer in O(log n) for the
non-overlapping case.

The ELF helpers are split by intent: segment-vs-section, executable-only
vs. read-only-data vs. all-allocatable-file-backed. This lets callers
build the right region set for the use case (e.g. instruction fetch wants
`PROGBITS+LOAD` segments; `LoadReadOnly` wants `.rodata` + `.text`).

`apply_elf_relocations` rewrites the bytes of in-memory regions per the
ELF's dynamic relocation entries. The `_autoload` variant additionally
scans for relocation sites that fall outside the supplied region set and
extends with the section that owns the missing site before applying — a
common scenario when the caller has loaded only segments and `.got.plt`
isn't backed by any segment but is still a valid relocation target.

## Key invariants

- `MemRegion::new` rejects any `(start_addr, data)` whose
  `start_addr + data.len()` would overflow `u64`. After construction,
  `end_addr()` cannot panic.
- `MemRegion::read(addr, out)` returns `None` for `addr < start_addr` or
  `addr >= end_addr` (a zero-byte read at exactly `end_addr` returns
  `None`). On a hit, the returned `n <= out.len()`; `n < out.len()`
  signals a partial read past the region's end.
- `MemRegionsLookupTable::new` collapses regions sharing a start address
  by overwriting (last write wins).
- `MemReadError` delegates `Display` / `Debug` to its inner
  `anyhow::Error`; its `source()` returns the error chain one level
  below — so consumers chasing `source()` see the real underlying
  error, not the `anyhow::Error` wrapper.

## Tests

Integration tests in `crates/reader/tests/`. The `tempfile` dev-dep is
used to write fixture ELFs with `object`'s write feature.

```
cargo test --package reader
```

## Gotchas

- `apply_elf_relocations` requires the supplied `regions` to cover every
  relocation site. Use `apply_elf_relocations_autoload` to extend the
  region set automatically — this is what
  `MemoryMap.apply_elf_relocations` (Python) and `strider-py`'s reader
  use by default.
- `ReadOnlyMemory::read` returns a `u64` regardless of `size` — the
  caller is responsible for masking / sign-extending for sizes < 8.
- Two regions overlapping at *different* start addresses are walked from
  highest-start downward; the first containing region wins. Worst-case
  O(n) when many regions stack at the same prefix.
- Depends on `object`, `rsleigh`, and `anyhow`. The lookup primitives
  (`MemRegion`, `MemRegionsLookupTable`, `ReadOnlyMemory`, `MemReadError`)
  do not depend on `object` and can be reused by non-ELF backends.
