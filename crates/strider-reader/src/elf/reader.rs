use crate::{MemRegionsLookupTable, Result};

use super::load::{OwnedElf, RelocationMode};
use super::sections::RegionSource;

/// An [`rsleigh::MemReader`] over an ELF's fetch image and a
/// [`crate::ReadOnlyMemory`] over its ROM ([`OwnedElf::image`]).
///
/// Its regions window the [`OwnedElf`]'s mapping, so a file truncated under it
/// SIGBUSes a read past the new end.
#[derive(Debug)]
pub struct ElfFileMemReader {
    fetch: MemRegionsLookupTable,
    rom: MemRegionsLookupTable,
}

impl ElfFileMemReader {
    /// `elf`'s views under `mode`, from [`RegionSource::Auto`].
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

    /// [`OwnedElf::check_unchanged`] for the mapping both views window.
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
        rsleigh::MemReader::read(&self.fetch, addr, out_buf)
    }
}

impl crate::ReadOnlyMemory for ElfFileMemReader {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.rom.read_exact(addr, buf)
    }

    fn try_read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.rom.read(addr, buf) == Some(buf.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReadOnlyMemory;
    use crate::elf::ElfSectionLayout;

    /// A single RWX mapping, the firmware / `ld -N` shape.
    fn rwx_elf() -> Vec<u8> {
        text_elf(true)
    }

    /// 16 bytes of `.text`, with `writable` also SHF_WRITE.
    fn text_elf(writable: bool) -> Vec<u8> {
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
                object::elf::SHF_ALLOC
                    | object::elf::SHF_EXECINSTR
                    | if writable { object::elf::SHF_WRITE } else { 0 },
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

    #[test]
    fn try_read_answers_as_read_does() {
        let elf = OwnedElf::parse(text_elf(false)).expect("parse");
        let obj = elf.checked_file().expect("parse");
        let base = {
            use object::{Object as _, ObjectSection as _};
            let sec = obj
                .sections()
                .find(|s| s.name() == Ok(".text"))
                .expect(".text");
            ElfSectionLayout::new(&obj).section_base(&sec)
        };
        let reader = ElfFileMemReader::new(&elf, RelocationMode::Unapplied).expect("reader");
        let mut served = 0;
        for (addr, len) in [
            (base, 16),
            (base + 12, 4),
            (base + 12, 8),
            (base + 16, 1),
            (base, 0),
        ] {
            let (mut a, mut b) = (vec![0u8; len], vec![0u8; len]);
            let read = ReadOnlyMemory::read(&reader, addr, &mut a).is_ok();
            assert_eq!(reader.try_read(addr, &mut b), read, "{addr:#x}+{len}");
            if read {
                assert_eq!(a, b);
                served += 1;
            }
        }
        assert_eq!(served, 3);
    }
}
