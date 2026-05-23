# `strider-reader` — memory readers for binary analysis

Loads ELF binaries and exposes them through two complementary interfaces:
[`rsleigh::MemReader`] (for Sleigh's instruction fetch) and
[`ReadOnlyMemory`](../strider-ir/src/read_only_memory.rs) (for the
`LoadReadOnly` opt pass in `strider-analyze` that folds compile-time-constant
loads).  Same regions, two views.

## Public surface

- `MemRegion` — one contiguous range of bytes loaded at a fixed virtual
  address.  Constructor `MemRegion::new(start_addr, data)` rejects any
  pair that would overflow `u64` so all downstream arithmetic is plain
  `u64`.  Accessors: `start_addr`, `data`, `data_mut`, `end_addr`,
  `contains`, `read`.
- `MemRegionsLookupTable` — `BTreeMap<u64, MemRegion>` keyed by start
  address.  `new(regions)`, `read(addr, out)`.  O(log n) in the common
  non-overlapping case; resolves overlaps by walking candidates from
  highest `start_addr <= addr` downward.
- `ReadOnlyMemory` (re-exported from `strider-ir`) — single method
  `fn read(&self, addr: u64, size: usize) -> Option<u64>` returning up
  to 8 bytes as a little-endian-decoded `u64`, or `None` for unmapped
  addresses / sizes > 8.  Blanket impls for `Arc<T>` and `Box<T>`.
- `MemReadError` — `std::error::Error`-implementing wrapper around
  `anyhow::Error` so reader impls satisfy rsleigh 4.0.0's
  `MemReader::Err` bound while keeping ergonomic `anyhow!` / `?` usage.
  `From<anyhow::Error>` conversion provided.
- `elf::ElfFileMemReader` — owns a `MemRegionsLookupTable`, implements
  `rsleigh::MemReader` and `ReadOnlyMemory`.  Construct via
  `ElfFileMemReader::from_object(&obj)`.
- `elf::load_elf(path)` — parses an ELF into an
  `object::File<'static>` (re-exported at the crate root).
- `elf::elf_load_with_relocations(path)` — convenience that loads an
  ELF and applies its dynamic relocations in one call.
- ELF section helpers (re-exported from `elf::sections`):
  `elf_get_code_and_readonly_sections_as_mem_regions`,
  `elf_get_allocatable_file_backed_sections_as_mem_regions`.
- ELF relocation appliers (re-exported from `elf::relocations`):
  `apply_elf_relocations(regions, &obj)`,
  `apply_elf_relocations_autoload(regions, &obj)` (the latter scans
  the ELF's dynamic relocations, identifies any site addresses not
  yet covered by `regions`, and lazily extends with the section that
  owns each missing site — e.g. `.got.plt` — before applying).
- `RelocationStats` — applied / skipped / failed counters from the
  relocation appliers.
- `Result<T>` alias (`anyhow::Result<T>`).

## Architecture

`src/lib.rs` defines the backend-independent primitives: `MemRegion`,
`MemRegionsLookupTable`, `MemReadError`, plus a re-export of
`strider_ir::ReadOnlyMemory`.  New reader backends (raw blobs, PE,
Mach-O, …) can live alongside `elf` and reuse the same primitives.

`src/elf/` is the ELF backend, split across:

- `load.rs` — parse an `object::File` and the all-in-one
  `elf_load_with_relocations` convenience.
- `reader.rs` — `ElfFileMemReader`, which wraps a
  `MemRegionsLookupTable` and implements both `rsleigh::MemReader` and
  `ReadOnlyMemory`.
- `sections.rs` — section-selection helpers for code + read-only-data
  vs. all-allocatable-file-backed loadouts.
- `relocations.rs` — `apply_elf_relocations` and the autoload variant
  that lazily extends with sections owning relocation sites not yet
  covered (commonly `.got.plt`).

The lookup table indexes by start address so `read` can answer in
O(log n) for the non-overlapping case.

## Key invariants

- `MemRegion::new` rejects any `(start_addr, data)` whose
  `start_addr + data.len()` would overflow `u64`.  After construction,
  `end_addr()` cannot panic.
- `MemRegion::read(addr, out)` returns `None` for `addr < start_addr`
  or `addr >= end_addr` (a zero-byte read at exactly `end_addr`
  returns `None`).  On a hit, the returned `n <= out.len()`;
  `n < out.len()` signals a partial read past the region's end.
- `MemRegionsLookupTable::new` collapses regions sharing a start
  address by overwriting (last write wins).
- `MemReadError` delegates `Display` / `Debug` to its inner
  `anyhow::Error`; its `source()` returns the error chain one level
  below — so consumers chasing `source()` see the real underlying
  error, not the `anyhow::Error` wrapper.

## Tests

Integration tests in `crates/strider-reader/tests/` (the `tempfile`
dev-dep writes fixture ELFs with `object`'s write feature).

```bash
cargo test --package strider-reader
```

## Gotchas

- `apply_elf_relocations` requires the supplied `regions` to cover
  every relocation site.  Use `apply_elf_relocations_autoload` to
  extend the region set automatically — this is what
  `MemoryMap.apply_elf_relocations` (Python) and `strider-py`'s reader
  use by default.
- `ReadOnlyMemory::read` returns a `u64` regardless of `size` — the
  caller is responsible for masking / sign-extending for sizes < 8.
- Two regions overlapping at *different* start addresses are walked
  from highest-start downward; the first containing region wins.
  Worst-case O(n) when many regions stack at the same prefix.
- Depends on `object`, `rsleigh`, `strider-ir`, and `anyhow`.  The
  lookup primitives (`MemRegion`, `MemRegionsLookupTable`,
  `MemReadError`) do not depend on `object` and can be reused by
  non-ELF backends.
