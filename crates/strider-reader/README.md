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
faults in only the pages they read. Bytes handed in from elsewhere
(`MemRegion::new`) stay an owned buffer.

The file must not change on disk while it is mapped. `load_elf` stats the file
it just mapped and holds the fd, so the check follows that inode rather than the
path.

Checked at the top of an operation, one `stat` each: `OwnedElf::regions`,
`OwnedElf::checked_file` and the `ElfFileMemReader` constructors that map a file
(`from_elf`, `from_elf_relocated`, `from_path`) run it themselves --
`from_object` and `from_bytes` serve copied bytes and have nothing to stat --
and `check_unchanged` on `OwnedElf`, `MemRegion`,
`MemRegionsLookupTable` and `ElfFileMemReader` runs it on demand, one `stat`
per mapping rather than per region. A binary rebuilt between two operations is
then an `Err` naming the file, not bytes from a program that is no longer there.
A long-lived handle -- a REPL session -- should call `check_unchanged` at the top
of its own operations.

Not checked, and not checkable:

- Every `read`. They are syscall-free and stay that way, so a change landing
  after an operation's check and before its reads is still a torn read, or a
  SIGBUS past a shortened end that kills the process uncatchably.
- A rewrite in place that preserves the size and lands within the same second:
  the identity is size plus mtime at whole-second granularity (drvfs truncates
  mtime, so a finer comparison reports a change on identical bytes).
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

The `ReadOnlyMemory` view rejects any writable mapping outright, so on an image
whose only PT_LOAD is RWX (the MIPS `vmlinux` shape; x86-64 and arm64 ship
separate RX / R / RW PT_LOADs) every `ReadOnlyMemory::read` fails and nothing
folds. Supply your own `ReadOnlyMemory` over the constant data for such an
image.

Depends only on `object`, `memmap2`, `read-only-memory`, `rsleigh`, and
`anyhow`.
