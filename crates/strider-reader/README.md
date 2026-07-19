# strider-reader

Loads a binary into memory and serves its bytes to the rest of the pipeline. The
same regions are exposed two ways: as a Sleigh instruction source
(`rsleigh::MemReader`) for lifting, and as `ReadOnlyMemory` for the optimizer's
`LoadReadOnly` pass that folds constant loads.

## What's here

- `MemRegion` and `MemRegionsLookupTable`: backend-independent byte regions keyed
  by start address, with O(log n) reads in the common non-overlapping case.
- `ElfFileMemReader`: the ELF backend, built with `ElfFileMemReader::from_object`;
  implements both reader traits.
- `load_elf(path)` and `elf_load_with_relocations(path)`: parse an ELF, and
  optionally apply its relocations.
- Section and relocation helpers under `elf::`.

New backends (raw blobs, PE, Mach-O) can reuse the region primitives without
touching the ELF code. Depends only on `object`, `rsleigh`, `strider-ir`, and
`anyhow`. The source is the reference for the full surface.
