use crate::{MemRegionsLookupTable, Result};

use super::load::{OwnedElf, RelocationMode};
use super::sections::RegionSource;

/// An [`rsleigh::MemReader`] over an ELF's fetch image and a
/// [`crate::ReadOnlyMemory`] over its runtime-immutable mappings (plus `.got`,
/// `.got2` and `.data.rel.ro` under `got_is_rom`): instruction fetch may
/// reference a mapping that constant-load folding must not, so the two views
/// differ.
///
/// Its regions are windows into the [`OwnedElf`]'s bytes, so over a mapped
/// file it holds the mapping: [`check_unchanged`](Self::check_unchanged)
/// catches a file rebuilt between two operations, and a read past the end of
/// a file truncated under a live reader, or a paging error on a network or 9p
/// mount, is a SIGBUS (`STRIDER_NO_MMAP=1` reads the file instead; an
/// [`OwnedElf::parse`] image owns its bytes). Once the reader is handed to
/// `Sleigh` or a `Strider`, check through the [`OwnedElf`] that shares its
/// mapping.
#[derive(Debug)]
pub struct ElfFileMemReader {
    fetch: MemRegionsLookupTable,
    rom: MemRegionsLookupTable,
}

impl ElfFileMemReader {
    /// `elf`'s views under `mode`, kind-dispatched: PT_LOAD program headers
    /// for ET_EXEC / ET_DYN, allocatable sections at their
    /// [`ElfSectionLayout`](super::ElfSectionLayout) bases otherwise. See
    /// [`OwnedElf::image`].
    ///
    /// # Errors
    ///
    /// Anything [`OwnedElf::image`] reports.
    pub fn new(elf: &OwnedElf, mode: RelocationMode) -> Result<Self> {
        let image = elf.image(RegionSource::Auto, mode)?;
        Ok(Self {
            fetch: MemRegionsLookupTable::new(image.fetch),
            rom: MemRegionsLookupTable::new(image.rom),
        })
    }

    /// One `stat` of the file the regions window, as
    /// [`OwnedElf::check_unchanged`]. Both tables window the one mapping, so
    /// the fetch table answers for both.
    ///
    /// # Errors
    ///
    /// When the file changed on disk since it was mapped.
    pub fn check_unchanged(&self) -> Result<()> {
        self.fetch.check_unchanged()
    }
}

impl rsleigh::MemReader for ElfFileMemReader {
    type Err = crate::MemReadError;

    fn read(
        &self,
        addr: rsleigh::VnAddr,
        out_buf: &mut [u8],
    ) -> std::result::Result<usize, Self::Err> {
        self.fetch.read(addr.off, out_buf).ok_or_else(|| {
            crate::MemReadError(anyhow::anyhow!("address {:#x} is not mapped", addr.off))
        })
    }
}

impl crate::ReadOnlyMemory for ElfFileMemReader {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.rom.read_exact(addr, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReadOnlyMemory;
    use crate::elf::ElfSectionLayout;

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

        let err = ReadOnlyMemory::read(&reader, sec_addr, &mut buf)
            .expect_err("an RWX mapping must not answer a ROM read");
        assert!(
            err.to_string().contains("is not mapped"),
            "the ROM view holds no writable mapping at all, got: {err}"
        );
    }
}
