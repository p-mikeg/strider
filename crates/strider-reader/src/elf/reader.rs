//! [`ElfFileMemReader`] — an [`rsleigh::MemReader`] +
//! [`crate::ReadOnlyMemory`] impl backed by an ELF file's sections.

use anyhow::Context as _;
use object::Object;

use crate::{MemRegionsLookupTable, Result};

use super::sections::elf_get_loadable_regions;

/// An rsleigh [`rsleigh::MemReader`] backed by an ELF file's sections.
///
/// The reader owns its backing bytes (copied into [`crate::MemRegion`]s at
/// construction) so no lifetime borrow on the source `object::File` or its
/// byte buffer is required. Both the executable sections (for instruction
/// fetch) and the read-only data sections (for compile-time-constant loads)
/// are loaded from the same ELF.
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    endianness: strider_target::Endianness,
}

impl ElfFileMemReader {
    /// Builds a reader from an already-parsed [`object::File`].
    ///
    /// Loads every code + read-only mapping the loader surfaces for the
    /// ELF.  For ET_EXEC / ET_DYN that's the PT_LOAD program headers
    /// (the runtime memory layout); for ET_REL it's the allocatable
    /// sections with first-wins VMA dedup (see
    /// [`elf_get_loadable_regions`] for the full dispatch rules).
    /// The parsed object is not retained — the returned reader is
    /// self-owning.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`elf_get_loadable_regions`]:
    /// `Object` for unreadable segment / section data and
    /// `RegionOverflow` if any included mapping's `address + length`
    /// would exceed `u64::MAX`.
    pub fn from_object(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_loadable_regions(obj)?;
        let endianness = match obj.endianness() {
            object::Endianness::Little => strider_target::Endianness::Little,
            object::Endianness::Big => strider_target::Endianness::Big,
        };
        Ok(Self {
            lookup: MemRegionsLookupTable::new(regions),
            endianness,
        })
    }

    /// Builds a reader by parsing the given ELF bytes.
    ///
    /// The bytes are parsed in-place; no leak is required.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes fail to parse as a valid ELF,
    /// or any error produced by [`from_object`](Self::from_object).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let obj = object::File::parse(bytes).context("failed to parse ELF")?;
        Self::from_object(&obj)
    }

    /// Builds a reader by reading and parsing an ELF file from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read from disk, or
    /// any error produced by [`from_bytes`](Self::from_bytes).
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let bytes = std::fs::read(path).context("failed to read file")?;
        Self::from_bytes(&bytes)
    }
}

impl rsleigh::MemReader for ElfFileMemReader {
    type Err = crate::MemReadError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> std::result::Result<usize, Self::Err> {
        self.lookup
            .read(addr.off, out_buf)
            .ok_or_else(|| crate::MemReadError(anyhow::anyhow!("address {:#x} is not mapped", addr.off)))
    }
}

impl crate::ReadOnlyMemory for ElfFileMemReader {
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        if size == 0 || size > 8 {
            return None;
        }
        // Place the read bytes at the endianness-appropriate end of an 8-byte
        // buffer so `Endianness::read_u64` produces the same numeric value for
        // an N-byte load as the target machine would.  LE: bytes go in the low
        // slots.  BE: bytes go in the high slots so the widened word reads as a
        // big-endian N-byte value.  The byte-order branch itself lives once in
        // `Endianness::read_u64` (the SSoT), mirrored by
        // `strider-py`'s `PyMemoryMapReader`.
        use strider_target::Endianness;
        let mut buf = [0u8; 8];
        let slot = match self.endianness {
            Endianness::Little => &mut buf[..size],
            Endianness::Big => &mut buf[8 - size..],
        };
        if self.lookup.read(addr, slot)? != size {
            return None;
        }
        Some(self.endianness.read_u64(buf))
    }
}
