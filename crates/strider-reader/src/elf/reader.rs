use anyhow::Context as _;

use crate::{MemRegionsLookupTable, Result};

use super::sections::elf_get_loadable_regions;

/// An [`rsleigh::MemReader`] + [`crate::ReadOnlyMemory`] backed by an ELF's
/// code and read-only mappings: instruction fetch and constant-load folding
/// both served from the same regions.
///
/// Bytes are copied at construction, so the reader borrows neither the
/// `object::File` nor its buffer.
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
}

impl ElfFileMemReader {
    /// Loads every code + read-only mapping, kind-dispatched: PT_LOAD program
    /// headers for ET_EXEC / ET_DYN, allocatable sections with first-wins VMA
    /// dedup for ET_REL. See [`elf_get_loadable_regions`].
    ///
    /// # Errors
    ///
    /// From [`elf_get_loadable_regions`]: unreadable segment / section data, or
    /// a mapping whose `address + length` exceeds `u64::MAX`.
    pub fn from_object(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_loadable_regions(obj)?;
        Ok(Self {
            lookup: MemRegionsLookupTable::new(regions),
        })
    }

    /// # Errors
    ///
    /// When the bytes do not parse as ELF, plus anything
    /// [`from_object`](Self::from_object) reports.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let obj = object::File::parse(bytes).context("failed to parse ELF")?;
        Self::from_object(&obj)
    }

    /// # Errors
    ///
    /// When the file cannot be read from disk, plus anything
    /// [`from_bytes`](Self::from_bytes) reports.
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let bytes = std::fs::read(path).context("failed to read file")?;
        Self::from_bytes(&bytes)
    }
}

impl rsleigh::MemReader for ElfFileMemReader {
    type Err = crate::MemReadError;

    fn read(
        &self,
        addr: rsleigh::VnAddr,
        out_buf: &mut [u8],
    ) -> std::result::Result<usize, Self::Err> {
        self.lookup.read(addr.off, out_buf).ok_or_else(|| {
            crate::MemReadError(anyhow::anyhow!("address {:#x} is not mapped", addr.off))
        })
    }
}

impl crate::ReadOnlyMemory for ElfFileMemReader {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        // Raw bytes, no endianness swap; a short or unmapped fill errors.
        self.lookup.read_exact(addr, buf)
    }
}
