use crate::{MemRegionsLookupTable, Result};

use super::load::{OwnedElf, RelocationMode};
use super::sections::{AddressRanges, ElfSectionLayout, LoadFilter, RegionSource};

/// An [`rsleigh::MemReader`] over an ELF's fetch image, and a
/// [`crate::ReadOnlyMemory`] over that image minus its writable (RWX) mappings.
/// Instruction fetch may reference a mapping that constant-load folding must
/// not, so the two views differ.
///
/// Its regions are windows into the [`OwnedElf`]'s bytes, so it holds the
/// mapping: [`check_unchanged`](Self::check_unchanged) catches a file rebuilt
/// between two operations, and a file truncated under a live reader is a
/// SIGBUS (`STRIDER_NO_MMAP=1` reads the file instead). Once the reader is
/// handed to `Sleigh` or a `Strider`, check through the [`OwnedElf`] that
/// shares its mapping.
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    /// Every writable mapping the image declares, the fetchable RWX ones and
    /// the ones the fetch filter dropped alike. The `ReadOnlyMemory` view is
    /// the fetch image minus these, expressed as ranges rather than a second
    /// table so the bytes are stored once.
    writable: AddressRanges,
}

impl ElfFileMemReader {
    /// Every code + read-only mapping of `elf`, as `mode` leaves them,
    /// kind-dispatched: PT_LOAD program headers for ET_EXEC / ET_DYN,
    /// allocatable sections at their [`ElfSectionLayout`] bases for ET_REL.
    ///
    /// # Errors
    ///
    /// Anything [`OwnedElf::regions`] reports.
    pub fn new(elf: &OwnedElf, mode: RelocationMode) -> Result<Self> {
        let RelocationMode::Unapplied = mode;
        let obj = elf.checked_file()?;
        let image = elf.regions_with(
            &obj,
            &ElfSectionLayout::new(&obj),
            RegionSource::Auto,
            LoadFilter::CodeAndReadOnly,
            false,
        )?;
        Ok(Self {
            lookup: MemRegionsLookupTable::new(image.regions),
            writable: image.writable,
        })
    }

    /// One `stat` of the file the regions window, as
    /// [`OwnedElf::check_unchanged`].
    ///
    /// # Errors
    ///
    /// When the file changed on disk since it was mapped.
    pub fn check_unchanged(&self) -> Result<()> {
        self.lookup.check_unchanged()
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
        if self.writable.touches(addr, buf.len()) {
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
        let elf = OwnedElf::parse(rwx_elf()).expect("parse");
        let obj = elf.checked_file().expect("parse");
        let sec_addr = {
            use object::{Object as _, ObjectSection as _};
            let sec = obj
                .sections()
                .find(|s| s.name() == Ok(".text"))
                .expect(".text");
            ElfSectionLayout::new(&obj).section_base(&sec)
        };
        let reader = ElfFileMemReader::new(&elf, RelocationMode::Unapplied).expect("reader");

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
