# strider-reader

Loads a binary into memory and serves its bytes to the rest of the pipeline. The
fetch image is exposed as a Sleigh instruction source (`rsleigh::MemReader`) for
lifting; that image minus its writable mappings is exposed as `ReadOnlyMemory`
for the optimizer's `LoadReadOnly` pass that folds constant loads, which must
not read a mapping the program can write.

## What's here

- `MemRegion` and `MemRegionsLookupTable`: backend-independent byte regions keyed
  by start address. A read one region fully covers is a binary search;
  otherwise it walks down from there, stopping as soon as no lower-start region
  still reaches the address.
- `ElfFileMemReader`: the ELF backend, built with `ElfFileMemReader::from_elf`
  (shares the ELF's bytes) or `::from_object` (copies them); implements both
  reader traits. Those, and `::from_bytes` / `::from_path`, serve the
  file-initial bytes, so an unlinked or not-yet-`ld.so`'d image reads zero at
  every relocation site; `::from_elf_relocated` is the constructor that applies
  the relocations.
- `load_elf(path)`: memory-map an ELF into an `OwnedElf`.
  `OwnedElf::regions(source, filter, relocate)`: one region set cut from those
  bytes; several sets (a fetch image and its ROM subset) share the one buffer.
  `elf_get_loadable_regions(&file)` serves a caller holding only a parsed file,
  copying each mapping.
- Section and relocation helpers under `elf::`.

## Loading is lazy

A region is a window into the mapped file, and relocations are a sorted patch
list applied to the caller's buffer as a read crosses a site, so loading costs
headers rather than bytes: querying a few functions of a large shared object
faults in only the pages they read. The file must not change on disk while it is
mapped. Bytes handed in from elsewhere (`MemRegion::new`) stay an owned buffer.

ET_EXEC / ET_DYN load from PT_LOAD program headers. Everything else, ET_REL
above all, loads from sections, whose pre-link `sh_addr` is typically 0 for all
of them; `elf::ElfSectionLayout` rebases the collisions apart the way a linker
would, and every address a caller sees (region start, relocation site, symbol)
goes through it.

The `ReadOnlyMemory` view rejects any writable mapping outright, so on an image
whose only PT_LOAD is RWX (the MIPS `vmlinux` shape; x86-64 and arm64 ship
separate RX / R / RW PT_LOADs) every `ReadOnlyMemory::read` fails and nothing
folds. Supply your own `ReadOnlyMemory` over the constant data for such an
image.

Depends only on `object`, `memmap2`, `read-only-memory`, `rsleigh`, and
`anyhow`.
