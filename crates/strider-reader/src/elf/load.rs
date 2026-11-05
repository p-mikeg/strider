use anyhow::Context as _;

use crate::{FileBytes, MemRegion, Result};

use super::relocations::apply_elf_relocations_with;
use super::sections::{AddressSet, ElfSectionLayout, LoadFilter, RegionSource};

/// Maps the file, which must not change on disk while the `OwnedElf` or
/// anything built from it lives; see [`OwnedElf::check_unchanged`].
///
/// # Errors
///
/// When the file cannot be read from disk, or its bytes do not parse as ELF.
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<OwnedElf> {
    OwnedElf::open(path)
}

/// An owned ELF's bytes.
pub struct OwnedElf {
    backing: FileBytes,
}

impl std::fmt::Debug for OwnedElf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    /// Applied by this loader; fetch then maps writable sections too.
    /// `got_is_rom` also serves `.got`, PPC32's `.got2` and `.data.rel.ro` as
    /// ROM: an unchecked claim that nothing writes them after the loader (full
    /// RELRO; PPC32 code relocating its own `.got2` breaks it) and that a
    /// symbol the image defines binds to that definition. A slot only the
    /// loader computes stays a hole; `.got.plt` is not served. An object
    /// file's synthetic GOT is ROM either way: its synthetic link binds every
    /// symbol locally.
    Applied { got_is_rom: bool },
}

/// One image's views, from one parse and one layout.
#[derive(Debug)]
pub struct ElfImage {
    /// Code and read-only mappings, plus writable ones when relocated.
    pub fetch: Vec<MemRegion>,
    /// The runtime-immutable mappings, an object file's synthetic GOT, and
    /// `.got`, `.got2` and `.data.rel.ro` under `got_is_rom`.
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

    /// Maps the file, sampling its `stat` identity for
    /// [`check_unchanged`](Self::check_unchanged).
    ///
    /// # Errors
    ///
    /// When the file cannot be read from disk, or does not parse as ELF.
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        Self::validated(FileBytes::map_path(path)?)
    }

    fn validated(backing: FileBytes) -> Result<Self> {
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

    /// Parses the mapping after [`check_unchanged`](Self::check_unchanged):
    /// parsing a shortened mapping is an uncatchable SIGBUS.
    ///
    /// # Errors
    ///
    /// When the file changed on disk since it was mapped, or the mapped bytes
    /// no longer parse.
    pub fn checked_file(&self) -> Result<object::File<'_>> {
        self.check_unchanged()?;
        object::File::parse(self.backing.as_slice())
            .map_err(|e| anyhow::anyhow!("the mapped image no longer parses as ELF: {e}"))
    }

    /// Compares one `stat` of the mapped file with the one [`open`](Self::open)
    /// took; `Ok` for bytes not mapped.
    ///
    /// # Errors
    ///
    /// When the file no longer stats, or its size or modification time
    /// changed. A rewrite preserving both passes.
    pub fn check_unchanged(&self) -> Result<()> {
        self.backing.check_unchanged()
    }

    /// Whether ARM `EF_ARM_BE8` is set: little-endian instructions, big-endian
    /// data. `EI_DATA` is MSB for both BE8 and BE32. `false` off ARM.
    ///
    /// # Errors
    ///
    /// As [`checked_file`](Self::checked_file).
    pub fn is_arm_be8(&self) -> Result<bool> {
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
    /// bytes, with `relocate` carrying the relocations as a patch list.
    ///
    /// # Errors
    ///
    /// As [`image`](Self::image), and when
    /// [`LoadFilter::ImmutableAndRelocatedGot`] meets `relocate == false`.
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
    /// from one parse.
    ///
    /// # Errors
    ///
    /// When a mapping can't be read or ends past `u64::MAX`, the relocation
    /// tables name more entries than the file holds or are unreadable where
    /// the image may relocate code, or [`checked_file`](Self::checked_file)
    /// fails.
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
        let view = if relocate {
            super::dynamic::loader_view(&obj)
        } else {
            super::dynamic::LoaderView::NotDynamic
        };
        let fetch = self.regions_with(&obj, &layout, source, fetch, relocate, &view)?;
        Ok(ElfImage {
            rom: self
                .regions_with(&obj, &layout, source, rom, relocate, &view)?
                .regions,
            fetch: fetch.regions,
            writable: fetch.writable,
        })
    }

    /// The mappings `source` and `filter` select over a
    /// [`checked_file`](Self::checked_file) parse. When `view` is unread, a
    /// relocating load serves no writable, non-executable mapping. Errs when a
    /// mapping cannot be loaded or relocated, or on `ImmutableAndRelocatedGot`
    /// without `relocate`.
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
                "LoadFilter::ImmutableAndRelocatedGot claims relocated `.got` / \
                 `.data.rel.ro`, which relocate=false denies: relocate, or use \
                 LoadFilter::ImmutableOnly"
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
