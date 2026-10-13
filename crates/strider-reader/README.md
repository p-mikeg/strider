# strider-reader

Loads a binary into memory and serves its bytes to the rest of the pipeline. The
fetch image is exposed as a Sleigh instruction source (`rsleigh::MemReader`) for
lifting; that image minus its writable mappings is exposed as `ReadOnlyMemory`
for the optimizer's `LoadReadOnly` pass that folds constant loads, which must
not read a mapping the program can write.

## What's here

- `MemRegion` and `MemRegionsLookupTable`: backend-independent byte regions keyed
  by start address. A read is served by exactly one of them, never a per-byte
  merge: the highest-start region fully covering the request, else the region
  serving the most bytes from the address. Both are O(log n) descents of a
  max-end tree, whether the regions are disjoint or nest.
- `ElfFileMemReader`: the ELF backend, built with
  `ElfFileMemReader::from_object`, which copies each mapping; implements both
  reader traits. It serves the file-initial bytes, so an unlinked or
  not-yet-`ld.so`'d image serves each relocation site's unrelocated field (an
  addend, or `ld`'s link-time value), never its target; the relocated
  view of an image is an `OwnedElf::regions(.., relocate)` load.
- `load_elf(path)`: memory-map an ELF into an `OwnedElf`.
  `OwnedElf::regions(source, filter, relocate)`: one region set cut from those
  bytes; several sets (a fetch image and its ROM subset) share the one buffer.
  `elf_get_loadable_regions(&file)` serves a caller holding only a parsed file,
  copying each mapping.
- Section helpers under `elf::`: `RegionSource` / `LoadFilter` pick what a
  region set is cut from; `ImmutableAndRelocatedGot` requires `relocate`, that
  being the claim it makes, and serves only a `.got` / `.data.rel.ro` the
  loader really maps writable. `ElfSectionLayout` lays out an ET_REL's
  colliding sections and mints each undefined symbol's `extern_address`,
  `OpdTable` resolves a ppc64 ELFv1 function symbol through its `.opd`
  descriptor, and
  `mapping_symbol_data_ranges` turns an ARM or AArch64 image's `$d` marks into
  the data ranges a CFG build must not decode. The relocation machinery itself
  is crate-private; it is reached through `OwnedElf::regions(.., relocate)`.

## Loading is lazy

A region is a window into the mapped file, and relocations are a sorted patch
list applied to the caller's buffer as a read crosses a site, so loading costs
headers rather than bytes: querying a few functions of a large shared object
faults in only the pages they read. Bytes handed in from elsewhere
(`MemRegion::new`) stay an owned buffer.

The file must not change on disk while it is mapped. `load_elf` stats the file
it just mapped and holds the fd, so the check follows that inode rather than the
path.

Checked at the top of an operation, one `stat` each: `OwnedElf::regions` and
`OwnedElf::checked_file` run it themselves, and `check_unchanged` on
`OwnedElf`, `MemRegion` and `MemRegionsLookupTable` runs it on demand, one
`stat` per mapping rather than per region. An `ElfFileMemReader` has nothing to
stat, serving copied bytes. A binary rebuilt between two operations is then an
`Err` naming the file, not bytes from a program that is no longer there. A
long-lived handle such as a REPL session should call `check_unchanged` at the
top of its own operations.

Not checked, and not checkable:

- Every `read`. They are syscall-free and stay that way, so a change landing
  after an operation's check and before its reads is still a torn read, or a
  SIGBUS past a shortened end that kills the process uncatchably.
- A rewrite in place that preserves the size and lands within the same second:
  the identity is size plus mtime at whole-second granularity.
- A different file moved onto the path. The mapped inode is untouched, its bytes
  are what was mapped, and reporting it would break analysing a build-system
  temp file that gets replaced or unlinked mid-run.

`STRIDER_NO_MMAP` set to any value but `0` reads the file instead: it costs the
file's size in memory, cannot tear, and needs no check at all.

ET_EXEC / ET_DYN load from PT_LOAD program headers. Everything else, ET_REL
above all, loads from sections, whose pre-link `sh_addr` is typically 0 for all
of them; `elf::ElfSectionLayout` rebases the collisions apart the way a linker
would, from a synthetic image base that leaves address 0 unmapped, and every
address a caller sees (region start, relocation site, symbol) goes through it.

Relocating an ET_REL stands in for the link. A symbol the object does not
define, and a `SHN_COMMON` one, gets a distinct address in an unmapped range
past the image (`ElfSectionLayout::extern_address`), so a call or load through
it has a target but nothing to fold. A GOT reference loads from a synthetic
slot holding the symbol's address. An undefined `STB_WEAK` symbol is the
exception: it resolves to 0 at link or module-load time as readily as to an
address, so `if (&w)` must fold neither way. Its slot stays unmapped and every
field that would hold its address is left uncomputed; only a call or jump
displacement, which reveals no address, takes the synthetic one. Instruction
encodings (branches, page and split address pairs, `MOVW`/`MOVT`, TOC offsets)
are applied for x86, ARM, AArch64, MIPS and PowerPC, in the instruction byte
order, which on AArch64 is little-endian whatever `EI_DATA` says since only
data is big-endian there. The GOT and TOC are this loader's, so their bytes
differ from any real link's while computing the same addresses. A relocation it
cannot compute (TLS, MIPS GP-relative, a branch that would need a veneer) is
never left as its unrelocated field: outside a writable mapping the field is a
hole, so decoding or folding across it fails with the relocation type named. An
`SHT_REL` `R_*_RELATIVE` is not one of those: its field already holds the whole
value, and is served as assembled.

The `ReadOnlyMemory` view rejects any writable mapping outright, so on an image
whose only PT_LOAD is RWX (the MIPS `vmlinux` shape; x86-64 and arm64 ship
separate RX / R / RW PT_LOADs) every `ReadOnlyMemory::read` fails and nothing
folds. Supply your own `ReadOnlyMemory` over the constant data for such an
image.

Depends only on `object`, `memmap2`, `read-only-memory`, `rsleigh`, and
`anyhow`.
