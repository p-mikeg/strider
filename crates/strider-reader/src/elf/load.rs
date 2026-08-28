use anyhow::Context as _;

use crate::{FileBytes, MemRegion, Result};

use super::relocations::apply_elf_relocations_with;
use super::sections::{ElfSectionLayout, LoadFilter, RegionSource};

/// Maps the file: it must not change on disk while the returned `OwnedElf`
/// lives. A read past a shortened file is filesystem-dependent -- zero-filled
/// on some, SIGBUS on others -- so a truncation is not reliably an error and
/// the analysis can silently lift zeros.
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
/// self-referential; [`file`](Self::file) re-parses instead.
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

impl OwnedElf {
    /// # Errors
    ///
    /// When `bytes` do not parse as a valid ELF.
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        Self::validated(FileBytes::from_vec(bytes))
    }

    /// Maps the file rather than reading it, so only the pages an analysis
    /// touches are ever faulted in.
    ///
    /// # Errors
    ///
    /// When the file cannot be read from disk, or does not parse as ELF.
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        Self::validated(FileBytes::map_path(path)?)
    }

    fn validated(backing: FileBytes) -> Result<Self> {
        // Validate now so `file()` can re-parse the identical bytes infallibly.
        object::File::parse(backing.as_slice()).context("failed to parse ELF")?;
        Ok(Self { backing })
    }

    /// Re-parses each call; see the type docs.
    ///
    /// # Panics
    ///
    /// When the bytes no longer parse as ELF. [`parse`](Self::parse) validated
    /// them, and holding the mapping still is the caller's half of the contract
    /// [`load_elf`] states; rebuilding the file under a live `OwnedElf` breaks
    /// it, as reading with `STRIDER_NO_MMAP=1` does not.
    #[inline]
    pub fn file(&self) -> object::File<'_> {
        object::File::parse(self.backing.as_slice())
            .expect("bytes were validated as ELF at construction")
    }

    /// Whether the ARM `EF_ARM_BE8` flag is set: instructions are stored
    /// little-endian while data stays big-endian.
    ///
    /// `EI_DATA` cannot answer this. A BE8 image and a traditional BE32 one are
    /// both `ELFDATA2MSB`, and decoding either as the other yields byte-swapped
    /// instructions, so the flag is the only thing that separates them. Always
    /// `false` off ARM, where the bit is not defined.
    #[must_use]
    pub fn is_arm_be8(&self) -> bool {
        /// `EF_ARM_BE8`, from the ARM ELF ABI.
        const EF_ARM_BE8: u32 = 0x0080_0000;
        let file = self.file();
        if object::read::Object::architecture(&file) != object::Architecture::Arm {
            return false;
        }
        matches!(
            object::read::Object::flags(&file),
            object::FileFlags::Elf { e_flags, .. } if e_flags & EF_ARM_BE8 != 0
        )
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
    /// exceed `u64::MAX`.
    pub fn regions(
        &self,
        source: RegionSource,
        filter: LoadFilter,
        relocate: bool,
    ) -> Result<Vec<MemRegion>> {
        Ok(self
            .regions_with(
                &ElfSectionLayout::new(&self.file()),
                source,
                filter,
                relocate,
            )?
            .regions)
    }

    /// [`regions`](Self::regions) over a layout the caller already built. It
    /// is a pure function of the bytes, so one built from any parse of them
    /// serves every later parse.
    ///
    /// # Errors
    ///
    /// Same as [`regions`](Self::regions).
    pub(crate) fn regions_with(
        &self,
        layout: &ElfSectionLayout,
        source: RegionSource,
        filter: LoadFilter,
        relocate: bool,
    ) -> Result<super::sections::LoadedImage> {
        let obj = self.file();
        let mut image =
            super::sections::collect_regions(&obj, Some(&self.backing), source, filter, layout)?;
        if relocate {
            apply_elf_relocations_with(&mut image.regions, &obj, filter, layout)?;
        }
        Ok(image)
    }
}
