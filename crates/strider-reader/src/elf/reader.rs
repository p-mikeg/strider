use anyhow::Context as _;

use crate::{MemRegionsLookupTable, Result};

use super::sections::{ElfSectionLayout, LoadFilter, RegionSource, collect_regions};

/// An [`rsleigh::MemReader`] over an ELF's fetch image, and a
/// [`crate::ReadOnlyMemory`] over that image minus its writable (RWX) mappings.
/// Instruction fetch may reference a mapping that constant-load folding must
/// not, so the two views differ.
///
/// Built from an [`super::OwnedElf`] the reader shares that ELF's bytes; built
/// from a parsed `object::File` alone it copies them, borrowing neither the
/// file nor its buffer.
///
/// Every constructor but [`from_elf_relocated`](Self::from_elf_relocated)
/// serves the file-initial bytes: an unlinked or not-yet-`ld.so`'d image reads
/// zero at each relocation site.
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    /// `[start, end)` of the fetch mappings that are writable, i.e. RWX. The
    /// `ReadOnlyMemory` view is the fetch image minus these, expressed as
    /// ranges rather than a second table so the bytes are stored once.
    writable: Vec<(u64, u64)>,
}

impl ElfFileMemReader {
    /// Loads every code + read-only mapping, kind-dispatched: PT_LOAD program
    /// headers for ET_EXEC / ET_DYN, allocatable sections at their
    /// [`ElfSectionLayout`] bases for ET_REL. See
    /// [`super::elf_get_loadable_regions`].
    ///
    /// # Errors
    ///
    /// Unreadable segment / section data, or a mapping whose
    /// `address + length` exceeds `u64::MAX`.
    pub fn from_object(obj: &object::File<'_>) -> Result<Self> {
        let layout = ElfSectionLayout::new(obj);
        Ok(Self::over(collect_regions(
            obj,
            None,
            RegionSource::Auto,
            LoadFilter::CodeAndReadOnly,
            &layout,
        )?))
    }

    /// [`from_object`](Self::from_object) over an ELF whose bytes are already
    /// owned, serving them file-initial.
    ///
    /// # Errors
    ///
    /// Same as [`from_object`](Self::from_object).
    pub fn from_elf(elf: &super::OwnedElf) -> Result<Self> {
        Self::from_elf_maybe_relocated(elf, false)
    }

    /// [`from_elf`](Self::from_elf) with the ELF's relocations applied, so a
    /// site reads what the linker or `ld.so` would have written there rather
    /// than its file-initial zero. See [`super::apply_elf_relocations`] for
    /// which kinds are modelled.
    ///
    /// # Errors
    ///
    /// Same as [`from_object`](Self::from_object), plus an unreadable
    /// relocation table.
    pub fn from_elf_relocated(elf: &super::OwnedElf) -> Result<Self> {
        Self::from_elf_maybe_relocated(elf, true)
    }

    fn from_elf_maybe_relocated(elf: &super::OwnedElf, relocate: bool) -> Result<Self> {
        // Building a reader is where an analysis starts reading the mapping,
        // so it is where a file rebuilt under a live handle must surface as an
        // `Err` instead of as bytes from a different program.
        elf.check_unchanged()?;
        let obj = elf.file();
        let layout = ElfSectionLayout::new(&obj);
        Ok(Self::over(elf.regions_with(
            &layout,
            RegionSource::Auto,
            LoadFilter::CodeAndReadOnly,
            relocate,
        )?))
    }

    fn over(image: super::sections::LoadedImage) -> Self {
        Self {
            lookup: MemRegionsLookupTable::new(image.regions),
            writable: image.writable,
        }
    }

    /// Whether `[addr, addr + len)` touches a writable fetch mapping, i.e. is
    /// outside the immutable image. Usually a scan of an empty list, a normal
    /// ELF's fetch mappings all being read-only.
    fn touches_writable(&self, addr: u64, len: usize) -> bool {
        if len == 0 {
            return false;
        }
        let end = addr.saturating_add(len as u64);
        self.writable.iter().any(|&(lo, hi)| addr < hi && lo < end)
    }

    /// # Errors
    ///
    /// When the bytes do not parse as ELF, plus anything
    /// [`from_object`](Self::from_object) reports.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let obj = object::File::parse(bytes).context("failed to parse ELF")?;
        Self::from_object(&obj)
    }

    /// Maps the file: it must not change on disk while the reader lives, or a
    /// read can observe torn bytes or SIGBUS past a shorter end. Construction
    /// checks the file's `stat` identity, which catches a rebuild between two
    /// operations but not one racing a read.
    ///
    /// # Errors
    ///
    /// When the file cannot be read from disk, plus anything
    /// [`from_elf`](Self::from_elf) reports.
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        Self::from_elf(&super::OwnedElf::open(path)?)
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
        // A writable-but-executable mapping is fetchable and NOT immutable, so
        // it is not ROM even though it is in the fetch table.
        if self.touches_writable(addr, buf.len()) {
            anyhow::bail!("address {addr:#x} is in a writable mapping, not read-only memory");
        }
        self.lookup.read_exact(addr, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReadOnlyMemory;

    /// A single RWX PT_LOAD, the firmware / `ld -N` shape. Its bytes must be
    /// FETCHABLE (it is the only mapping to decode from) yet must NOT answer
    /// a ROM read: `LoadReadOnly` folds a constant-address load without
    /// consulting the memory chain, so a writable mapping there would make a
    /// store-then-reload fold to the file-initial byte.
    fn rwx_elf() -> Vec<u8> {
        use object::write::{Object, StandardSegment};
        use object::{Architecture, BinaryFormat, Endianness, SectionKind};
        let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
        // `Text` is SHF_ALLOC|SHF_EXECINSTR; adding SHF_WRITE makes it RWX.
        let sec = obj.add_section(
            obj.segment_name(StandardSegment::Text).to_vec(),
            b".text".to_vec(),
            SectionKind::Text,
        );
        obj.append_section_data(sec, &[0x90u8; 16], 1);
        obj.section_mut(sec).flags = object::SectionFlags::Elf {
            sh_flags: u64::from(
                object::elf::SHF_ALLOC | object::elf::SHF_EXECINSTR | object::elf::SHF_WRITE,
            ),
        };
        obj.write().expect("write ELF")
    }

    #[test]
    fn an_rwx_mapping_is_fetchable_but_is_not_read_only_memory() {
        let bytes = rwx_elf();
        let obj = object::File::parse(&bytes[..]).expect("parse");
        let sec_addr = {
            use object::{Object as _, ObjectSection as _};
            obj.sections()
                .find(|s| s.name() == Ok(".text"))
                .expect(".text")
                .address()
        };
        let reader = ElfFileMemReader::from_object(&obj).expect("from_object");

        let mut buf = [0u8; 4];
        let n = rsleigh::MemReader::read(
            &reader,
            rsleigh::VnAddr {
                off: sec_addr,
                space: rsleigh::VnSpace::RAM,
            },
            &mut buf,
        )
        .expect("an RWX mapping must be fetchable");
        assert_eq!((n, buf), (4, [0x90; 4]));

        assert!(
            ReadOnlyMemory::read(&reader, sec_addr, &mut buf).is_err(),
            "an RWX mapping must not answer a ROM read"
        );
    }
}
