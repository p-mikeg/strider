# strider-reader

Loads a binary and serves its bytes: the fetch image as a Sleigh instruction
source (`rsleigh::MemReader`), and the runtime-immutable mappings as
`ReadOnlyMemory` for the optimizer's `LoadReadOnly`, which must not read a
mapping the program can write. `OwnedElf::image` builds both, and the writable
addresses, from one parse.

## What's here

- `MemRegion` and `MemRegionsLookupTable`: byte regions keyed by start address.
  A read is served by one region, never merged per byte: the highest-start
  region fully covering it, else the one serving the most bytes. O(log n),
  disjoint or nested.
- `load_elf(path)` maps an ELF into an `OwnedElf`; `OwnedElf::parse` takes
  bytes. `OwnedElf::image(source, mode)` gives the fetch and ROM region sets,
  sharing one buffer.
- `ElfFileMemReader::new(&elf, mode)` implements both reader traits over that
  image. `RelocationMode::Unapplied` serves file-initial bytes (an addend or
  `ld`'s link-time value at each relocation site);
  `RelocationMode::Applied { got_is_rom }` serves the fields this loader
  relocates, and with `got_is_rom` also `.got` / `.data.rel.ro` as ROM.
- Under `elf::`: `RegionSource` picks segments or sections; `ElfSectionLayout`
  rebases an ET_REL's sections and gives each undefined symbol an
  `extern_address`; `OpdTable` resolves a ppc64 ELFv1 descriptor;
  `mapping_symbol_data_ranges` turns ARM / AArch64 `$d` marks into data ranges
  a CFG build must not decode.
- `OwnedElf::regions` and `elf::LoadFilter` are test-only (`test-util`).

## Loading is lazy

A region is a window into the mapped file, and relocations are a sorted patch
list applied as a read crosses a site, so loading touches headers, not bytes.

The file must not change on disk while mapped. `OwnedElf::image` and
`OwnedElf::checked_file` stat it (through the held fd) against its size and
whole-second mtime, and `check_unchanged` on `OwnedElf`, `ElfFileMemReader`,
`MemRegion` and `MemRegionsLookupTable` does so on demand, so a rebuild between
operations is an `Err`. Not caught: a change during a read (a torn read, or an
uncatchable SIGBUS past a shortened end), a same-size rewrite within one
second, and a paging error on a network or 9p mount (also SIGBUS).
`STRIDER_NO_MMAP` set to anything but `0` reads the file into memory instead.

## Layout and relocation

ET_EXEC / ET_DYN load from PT_LOAD headers; everything else from sections,
which `ElfSectionLayout` rebases apart from a base that leaves 0 unmapped.

Relocating an ET_REL stands in for the link. An undefined or `SHN_COMMON`
symbol gets an address in an unmapped range past the image, and a GOT reference
loads from a synthetic slot. An undefined `STB_WEAK` symbol's address is left
uncomputed so `if (&w)` folds neither way; only a branch displacement takes the
synthetic address. Instruction encodings are applied for x86, ARM, AArch64,
MIPS and PowerPC, in instruction byte order (little-endian on AArch64 whatever
`EI_DATA` says). A relocation that cannot be computed (TLS, MIPS GP-relative, a
branch needing a veneer, one reading `S` of STN_UNDEF) makes its field a hole,
so decoding or folding across it fails naming the type; the rule is
`Ctx::unmodelled` in `src/elf/relocations/mod.rs`.

On an image whose only PT_LOAD is RWX (the MIPS `vmlinux` shape) the ROM view
is empty (under `got_is_rom`, its `.got` / `.data.rel.ro` aside); supply your
own `ReadOnlyMemory` over its constant data.
