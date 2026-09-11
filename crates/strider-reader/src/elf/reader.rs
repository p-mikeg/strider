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
/// Either way the bytes are file-initial: an unlinked or not-yet-`ld.so`'d
/// image reads zero at each relocation site. A relocated image is a region set
/// from [`super::OwnedElf::regions`], which this reader does not wrap.
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    /// Ascending, disjoint `[start, end)` of every writable mapping the image
    /// declares, the fetchable RWX ones and the ones the fetch filter dropped
    /// alike. The `ReadOnlyMemory` view is the fetch image minus these,
    /// expressed as ranges rather than a second table so the bytes are stored
    /// once.
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
    /// Unreadable segment / section data, a mapping whose `address + length`
    /// exceeds `u64::MAX`, or copies exceeding the loader's amplification
    /// ceiling over the distinct file bytes behind them; this constructor
    /// copies every mapping.
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
    /// owned, windowing into them instead of copying.
    ///
    /// # Errors
    ///
    /// Same as [`from_object`](Self::from_object), plus a file rebuilt since
    /// it was mapped.
    pub fn from_elf(elf: &super::OwnedElf) -> Result<Self> {
        // Building a reader is where an analysis starts reading the mapping,
        // so it is where a file rebuilt under a live handle must surface as an
        // `Err` instead of as bytes from a different program.
        let obj = elf.checked_file()?;
        let layout = ElfSectionLayout::new(&obj);
        Ok(Self::over(elf.regions_with(
            &obj,
            &layout,
            RegionSource::Auto,
            LoadFilter::CodeAndReadOnly,
            false,
        )?))
    }

    fn over(image: super::sections::LoadedImage) -> Self {
        Self {
            lookup: MemRegionsLookupTable::new(image.regions),
            writable: merged(image.writable),
        }
    }

    /// Whether `[addr, addr + len)` touches a writable mapping, i.e. is outside
    /// the immutable image.
    fn touches_writable(&self, addr: u64, len: usize) -> bool {
        if len == 0 {
            return false;
        }
        let end = addr.saturating_add(len as u64);
        // The ranges are disjoint and ascending, so the first one reaching past
        // `addr` is the only candidate.
        let first = self.writable.partition_point(|&(_, hi)| hi <= addr);
        self.writable.get(first).is_some_and(|&(lo, _)| lo < end)
    }

    /// Re-stat the mapping this reader serves. Call it at the top of an
    /// operation that will read through it; the `read` impls themselves are
    /// syscall-free and stay that way.
    ///
    /// # Errors
    ///
    /// When the mapped file changed since it was mapped.
    pub fn check_unchanged(&self) -> Result<()> {
        self.lookup.check_unchanged()
    }
}

/// `ranges` sorted, with everything that overlaps or touches merged.
fn merged(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (lo, hi) in ranges {
        match out.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
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
            let sec = obj
                .sections()
                .find(|s| s.name() == Ok(".text"))
                .expect(".text");
            ElfSectionLayout::new(&obj).section_base(&sec)
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
