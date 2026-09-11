use anyhow::Context as _;

use crate::{FileBytes, MemRegion, Result};

use super::relocations::apply_elf_relocations_with;
use super::sections::{ElfSectionLayout, LoadFilter, OpdTable, RegionSource};

/// Maps the file: it must not change on disk while the returned `OwnedElf`
/// lives. [`OwnedElf::check_unchanged`] catches a file rebuilt between two
/// operations, and every region build runs it; a change racing a read in
/// progress is still a torn read, or SIGBUS past a shortened end.
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
        object::File::parse(backing.as_slice()).context("failed to parse ELF")?;
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
    /// long-lived handle -- a REPL session that outlives a rebuild -- to get an
    /// `Err` rather than bytes from a program that is no longer there.
    ///
    /// Always `Ok` for bytes that were read or handed in rather than mapped,
    /// `STRIDER_NO_MMAP=1` included: a copy cannot change underneath.
    ///
    /// # Errors
    ///
    /// When the file no longer stats, or no longer looks like the file that
    /// was mapped: a different size or a different modification time, the only
    /// two fields the recorded identity holds. A rewrite in place that preserves both,
    /// and a different file moved onto the path, both pass -- the inode is
    /// pinned by the held fd, so only the contents can move under it.
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

    /// The code entry at `addr`, following a ppc64 ELFv1 `.opd` descriptor
    /// when `addr` is one; `addr` itself otherwise. See [`OpdTable`].
    ///
    /// One parse per call: a caller resolving many symbols builds an
    /// [`OpdTable`] over its own [`checked_file`](Self::checked_file) instead.
    ///
    /// # Errors
    ///
    /// Anything [`checked_file`](Self::checked_file) reports.
    pub fn function_entry(&self, addr: u64) -> Result<u64> {
        let obj = self.checked_file()?;
        Ok(OpdTable::new(&obj)
            .and_then(|opd| opd.entry_at(addr))
            .unwrap_or(addr))
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
    pub fn regions(
        &self,
        source: RegionSource,
        filter: LoadFilter,
        relocate: bool,
    ) -> Result<Vec<MemRegion>> {
        let obj = self.checked_file()?;
        Ok(self
            .regions_with(&obj, &ElfSectionLayout::new(&obj), source, filter, relocate)?
            .regions)
    }

    /// [`regions`](Self::regions) over a parse and a layout the caller already
    /// holds, both of which must be of these bytes. The layout is a pure
    /// function of them, so one built from any parse serves every later parse.
    ///
    /// Takes the parse rather than making its own, so the
    /// [`checked_file`](Self::checked_file) guard is the caller's and cannot
    /// be skipped here.
    ///
    /// # Errors
    ///
    /// Same as [`regions`](Self::regions).
    pub(crate) fn regions_with(
        &self,
        obj: &object::File<'_>,
        layout: &ElfSectionLayout,
        source: RegionSource,
        filter: LoadFilter,
        relocate: bool,
    ) -> Result<super::sections::LoadedImage> {
        let mut image =
            super::sections::collect_regions(obj, Some(&self.backing), source, filter, layout)?;
        if relocate {
            apply_elf_relocations_with(&mut image.regions, obj, filter, layout)?;
        }
        Ok(image)
    }
}
