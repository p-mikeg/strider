use anyhow::Context as _;

use crate::{FileBytes, MemRegion, Result};

use super::relocations::apply_elf_relocations_with;
use super::sections::{AddressSet, ElfSectionLayout, LoadFilter, RegionSource};

/// Maps the file: it must not change on disk while the returned `OwnedElf`,
/// or any region or reader built from it, lives. [`OwnedElf::check_unchanged`]
/// catches a file rebuilt between two operations, and every region build runs
/// it; a change racing a read in progress is still a torn read, or SIGBUS past
/// a shortened end.
///
/// # Errors
///
/// When the file cannot be read from disk, or its bytes do not parse as ELF.
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<OwnedElf> {
    OwnedElf::open(path)
}

/// An owned ELF: the backing file bytes, freed on drop.
///
/// Only the bytes are stored. [`object::File`] is a borrowing view with no
/// owned variant, so holding one alongside its bytes would make this
/// self-referential; [`checked_file`](Self::checked_file) re-parses instead.
pub struct OwnedElf {
    backing: FileBytes,
}

impl std::fmt::Debug for OwnedElf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never dump the backing bytes (an ELF can be hundreds of MB).
        f.debug_struct("OwnedElf")
            .field("backing_len", &self.backing.len())
            .finish_non_exhaustive()
    }
}

/// What an image's relocations are taken to have done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocationMode {
    /// Nothing: each field serves its file-initial bytes, an addend or `ld`'s
    /// link-time value rather than its target.
    Unapplied,
    /// Applied by this loader, which then also maps every writable section for
    /// fetch. `got_is_rom` serves `.got` (PPC32's `.got2` too) and
    /// `.data.rel.ro` as ROM too: an unchecked claim that the dynamic loader
    /// fills those slots before the program runs and nothing writes them
    /// afterwards (a full-RELRO link maps them read-only; PPC32 code that
    /// relocates its own `.got2`, `prom_init`'s `reloc_got2` or
    /// `-mrelocatable` `.fixup`, breaks it), and that a symbol
    /// the image defines binds to that definition. A slot whose value only the
    /// loader computes stays a hole, as `LoadFilter::ImmutableAndRelocatedGot`
    /// lists. `.got.plt` is never served, since lazy binding rewrites it.
    Applied { got_is_rom: bool },
}

/// One image's views, from one parse and one layout.
#[derive(Debug)]
pub struct ElfImage {
    /// Instruction fetch: code and read-only mappings, plus every writable one
    /// once relocated.
    pub fetch: Vec<MemRegion>,
    /// The runtime-immutable mappings, plus `.got`, `.got2` and `.data.rel.ro`
    /// under `got_is_rom`.
    pub rom: Vec<MemRegion>,
    /// Every address the image maps writable, loaded or not.
    pub writable: AddressSet,
}

/// First line of a Git LFS pointer file.
const LFS_POINTER_MAGIC: &[u8] = b"version https://git-lfs.github.com/spec/v1";

impl OwnedElf {
    /// # Errors
    ///
    /// When `bytes` do not parse as a valid ELF.
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        Self::validated(FileBytes::from_vec(bytes))
    }

    /// Maps the file rather than reading it, so only the pages an analysis
    /// touches are ever faulted in. The file's `stat` identity is sampled here
    /// for [`check_unchanged`](Self::check_unchanged).
    ///
    /// # Errors
    ///
    /// When the file cannot be read from disk, or does not parse as ELF.
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        Self::validated(FileBytes::map_path(path)?)
    }

    fn validated(backing: FileBytes) -> Result<Self> {
        // A clone without git-lfs leaves pointer text where the image should
        // be, and "failed to parse ELF" sends the reader hunting a corrupt
        // binary instead of running `git lfs pull`.
        anyhow::ensure!(
            !backing.as_slice().starts_with(LFS_POINTER_MAGIC),
            "a Git LFS pointer, not an ELF: run `git lfs pull`"
        );
        let file = object::File::parse(backing.as_slice()).context("failed to parse ELF")?;
        anyhow::ensure!(
            matches!(file, object::File::Elf32(_) | object::File::Elf64(_)),
            "a {:?} object, not an ELF",
            file.format()
        );
        Ok(Self { backing })
    }

    /// Re-parses the mapping, behind
    /// [`check_unchanged`](Self::check_unchanged): the only way in for
    /// anything that parses the image (regions, entry point, symbol table,
    /// header flags), since an unguarded parse of a shortened mapping is a
    /// SIGBUS no caller can catch.
    ///
    /// # Errors
    ///
    /// When the file changed on disk since it was mapped, or when the mapped
    /// bytes no longer parse. The guard reads a truncated mtime and a size, so
    /// a same-length rewrite within one second passes it; parsing fallibly
    /// here is what keeps that case an error rather than a panic.
    pub fn checked_file(&self) -> Result<object::File<'_>> {
        self.check_unchanged()?;
        object::File::parse(self.backing.as_slice())
            .map_err(|e| anyhow::anyhow!("the mapped image no longer parses as ELF: {e}"))
    }

    /// One `stat` of the mapped file, comparing it against what it was when
    /// [`open`](Self::open) mapped it. Call it at the top of an operation on a
    /// long-lived handle (a REPL session that outlives a rebuild) to get an
    /// `Err` rather than bytes from a program that is no longer there.
    ///
    /// Always `Ok` for bytes that were read or handed in rather than mapped,
    /// `STRIDER_NO_MMAP=1` included: a copy cannot change underneath.
    ///
    /// # Errors
    ///
    /// When the file no longer stats, or no longer looks like the file that
    /// was mapped: a different size or a different modification time, the only
    /// two fields the recorded identity holds. A rewrite in place that
    /// preserves both passes, and so does a different file moved onto the
    /// path: the held fd pins the inode, so only the contents can move under
    /// it.
    pub fn check_unchanged(&self) -> Result<()> {
        self.backing.check_unchanged()
    }

    /// Whether the ARM `EF_ARM_BE8` flag is set: instructions are stored
    /// little-endian while data stays big-endian.
    ///
    /// `EI_DATA` cannot answer this. A BE8 image and a traditional BE32 one are
    /// both `ELFDATA2MSB`, and decoding either as the other yields byte-swapped
    /// instructions, so the flag is the only thing that separates them. Always
    /// `false` off ARM, where the bit is not defined.
    ///
    /// Parses the mapping, so it is guarded: a `bool` return could not report a
    /// file rebuilt under a live handle, and reading a shortened mapping is a
    /// SIGBUS no caller can catch.
    ///
    /// # Errors
    ///
    /// When the file changed on disk since it was mapped, or the mapped bytes
    /// no longer parse.
    pub fn is_arm_be8(&self) -> Result<bool> {
        /// `EF_ARM_BE8`, from the ARM ELF ABI.
        const EF_ARM_BE8: u32 = 0x0080_0000;
        let file = self.checked_file()?;
        if object::read::Object::architecture(&file) != object::Architecture::Arm {
            return Ok(false);
        }
        Ok(matches!(
            object::read::Object::flags(&file),
            object::FileFlags::Elf { e_flags, .. } if e_flags & EF_ARM_BE8 != 0
        ))
    }

    /// The mappings `source` and `filter` select, as windows into this ELF's
    /// bytes: no copy, and with `relocate` the relocations land as a patch list
    /// rather than as writes into a materialised image.
    ///
    /// Two region sets built from one [`OwnedElf`] (a fetch image and its ROM
    /// subset) share the single backing buffer.
    ///
    /// # Errors
    ///
    /// When a mapping's data can't be read, or its `address + length` would
    /// exceed `u64::MAX`, plus anything
    /// [`checked_file`](Self::checked_file) reports.
    ///
    /// Also when [`LoadFilter::ImmutableAndRelocatedGot`] meets
    /// `relocate == false`: the filter's claim is that the loader has
    /// relocated those slots, which `relocate == false` denies.
    #[cfg(feature = "test-util")]
    pub fn regions(
        &self,
        source: RegionSource,
        filter: LoadFilter,
        relocate: bool,
    ) -> Result<Vec<MemRegion>> {
        let obj = self.checked_file()?;
        let view = super::dynamic::loader_view(&obj);
        Ok(self
            .regions_with(
                &obj,
                &ElfSectionLayout::new(&obj),
                source,
                filter,
                relocate,
                &view,
            )?
            .regions)
    }

    /// The fetch and ROM region sets `mode` picks, and the writable addresses,
    /// from one parse: fetch is the code and read-only mappings unrelocated,
    /// every allocatable one relocated; ROM is the immutable mappings, plus
    /// `.got` / `.got2` / `.data.rel.ro` under `got_is_rom`.
    ///
    /// # Errors
    ///
    /// When a mapping's data can't be read, or its `address + length` would
    /// exceed `u64::MAX`; when a relocating `mode`'s relocation section
    /// headers name more than twice the entries the file holds at its
    /// smallest entry, or a table the dynamic tags place that cannot be read
    /// or decoded may relocate code (`DT_TEXTREL`, or a writable and
    /// executable mapping); plus anything
    /// [`checked_file`](Self::checked_file) reports.
    pub fn image(&self, source: RegionSource, mode: RelocationMode) -> Result<ElfImage> {
        let obj = self.checked_file()?;
        let layout = ElfSectionLayout::new(&obj);
        let (fetch, rom, relocate) = match mode {
            RelocationMode::Unapplied => (
                LoadFilter::CodeAndReadOnly,
                LoadFilter::ImmutableOnly,
                false,
            ),
            RelocationMode::Applied { got_is_rom } => (
                LoadFilter::AllAllocatable,
                if got_is_rom {
                    LoadFilter::ImmutableAndRelocatedGot
                } else {
                    LoadFilter::ImmutableOnly
                },
                true,
            ),
        };
        let view = super::dynamic::loader_view(&obj);
        let fetch = self.regions_with(&obj, &layout, source, fetch, relocate, &view)?;
        Ok(ElfImage {
            rom: self
                .regions_with(&obj, &layout, source, rom, relocate, &view)?
                .regions,
            fetch: fetch.regions,
            writable: fetch.writable,
        })
    }

    /// The mappings `source` and `filter` select, over a parse and a layout the
    /// caller already holds, both of which must be of these bytes. The layout is
    /// a pure function of them, so one built from any parse serves every later
    /// parse.
    ///
    /// Takes the parse rather than making its own, so the
    /// [`checked_file`](Self::checked_file) guard is the caller's and cannot
    /// be skipped here.
    ///
    /// # Errors
    ///
    /// When a mapping's data can't be read or its `address + length` would
    /// exceed `u64::MAX`, and when `filter` is `ImmutableAndRelocatedGot` with
    /// `relocate == false`.
    ///
    /// `view` is [`loader_view`](super::dynamic::loader_view) of `obj`. When
    /// it cannot read what the tags relocate, a relocating load serves no
    /// writable, non-executable mapping at all: those are where it would
    /// land.
    pub(crate) fn regions_with(
        &self,
        obj: &object::File<'_>,
        layout: &ElfSectionLayout,
        source: RegionSource,
        filter: LoadFilter,
        relocate: bool,
        view: &super::dynamic::LoaderView,
    ) -> Result<super::sections::LoadedImage> {
        if matches!(filter, LoadFilter::ImmutableAndRelocatedGot) && !relocate {
            anyhow::bail!(
                "LoadFilter::ImmutableAndRelocatedGot claims the loader has relocated \
                 `.got` / `.data.rel.ro`, which relocate=false denies: the slots would \
                 be served as their on-disk bytes, 0 on a PIE where the address lives \
                 in the relocation addend, and a load through one folds to that 0. \
                 Relocate, or use LoadFilter::ImmutableOnly"
            );
        }
        let mut image = super::sections::collect_regions(
            obj,
            &self.backing,
            source,
            filter,
            layout,
            view.is_unread(),
        )?;
        if relocate {
            if view.is_unread() {
                let data = AddressSet::without(&image.writable, &image.executable);
                image.regions = crate::collapse_equal_starts(super::sections::without_ranges(
                    std::mem::take(&mut image.regions),
                    &data,
                ));
            }
            let got = apply_elf_relocations_with(
                &mut image.regions,
                obj,
                &image.owners,
                layout,
                &image.writable,
                &image.executable,
                view,
            )?;
            image.regions.extend(got);
        }
        Ok(image)
    }
}
