# strider-reader

Loads a binary and serves its bytes: the fetch image as a Sleigh instruction
source (`rsleigh::MemReader`), and the runtime-immutable mappings as
`ReadOnlyMemory`, which must not include a mapping the program can write.
Writability is `p_flags` for the segment walk (`sh_flags` and `p_flags` for
the section walk).

## What's here

- `MemRegion` and `MemRegionsLookupTable`: byte regions keyed by start
  address. A read is served by one region, the highest-start one fully
  covering it, else the one serving the most bytes; O(log n).
- `load_elf(path)` / `OwnedElf::parse(bytes)`; `OwnedElf::image(source, mode)`
  gives the fetch and ROM region sets and the writable addresses from one
  parse, sharing one buffer.
- `ElfFileMemReader` (`new(&elf, mode)`) implements both reader traits.
  `RelocationMode::Unapplied` serves file-initial bytes;
  `RelocationMode::Applied { got_is_rom }` serves relocated fields, and with
  `got_is_rom` also `.got`, PPC32 `.got2` and `.data.rel.ro` as ROM.
- Under `elf::`: `RegionSource`, `ElfSectionLayout` (an ET_REL's section
  placement and each undefined symbol's `extern_address`), `OpdTable` (ppc64
  ELFv1 descriptors) and `mapping_symbol_data_ranges` /
  `mapping_symbol_kind` (ARM / AArch64 `$a`, `$d`, `$t`, `$x`).

## Loading is lazy

A region is a window into the mapped file, and relocations are a sorted patch
list applied as a read crosses a site, so loading reads headers and relocated
fields, not whole mappings.

The file must not change on disk while mapped. `check_unchanged` (on
`OwnedElf`, `ElfFileMemReader`, `MemRegion` and `MemRegionsLookupTable`)
compares its size and whole-second mtime against map time, and
`OwnedElf::image` runs it first. A change during a read, or a same-size rewrite
within one second, is not caught; a shortened file or a paging error on a
network mount is a SIGBUS. `STRIDER_NO_MMAP` set to anything but `0` reads the
file into memory instead.

## Layout and relocation

ET_EXEC / ET_DYN load from PT_LOAD headers, everything else from sections.

Relocating an ET_REL stands in for the link: an undefined or `SHN_COMMON`
symbol gets an address in an unmapped range past the image, and a GOT
reference loads from a synthetic slot. The synthetic link binds every symbol
locally, so those slots are ROM whatever `got_is_rom` says. An undefined weak
symbol's address is computed only where it cannot be observed (a branch
displacement, a GOT slot left unmapped), so `if (&w)` folds neither way.

Encodings are applied for x86, ARM, AArch64, MIPS and PowerPC. A relocation
that cannot be computed (TLS, MIPS GP-relative, a branch needing a veneer)
makes its field a hole, so a read across it fails naming the type; in writable
non-executable bytes the file-initial bytes stay. A MIPS `-mabicalls` object,
GCC's default, therefore holes each function's `$gp` setup and GOT loads;
build with `-mno-abicalls` to analyse a `.o`.

An image whose only PT_LOAD is RWX (the MIPS `vmlinux` shape) has an empty ROM
view (`got_is_rom`'s sections aside); supply your own `ReadOnlyMemory` over
its constant data.
