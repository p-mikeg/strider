//! Single-file ELF memory reader implementation (object 0.36 compatible)

use std::collections::BTreeMap;
use std::fmt::Debug;

use object::{Object, ObjectSection, ObjectSegment};

//
// =========================
//  MemRegion
// =========================
//

#[derive(Clone, Debug)]
pub struct MemRegion {
    pub start_addr: u64,
    pub data: Vec<u8>,
}

impl MemRegion {
    pub fn new(start_addr: u64, data: Vec<u8>) -> Self {
        Self { start_addr, data }
    }

    pub fn end_addr(&self) -> u64 {
        self.start_addr + self.data.len() as u64
    }

    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.start_addr && addr < self.end_addr()
    }

    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        if !self.contains(addr) {
            return None;
        }

        let offset = (addr - self.start_addr) as usize;
        let available = self.data.len().saturating_sub(offset);
        let to_copy = available.min(out.len());

        out[..to_copy]
            .copy_from_slice(&self.data[offset..offset + to_copy]);

        Some(to_copy)
    }
}

//
// =========================
//  MemRegionsLookupTable
// =========================
//

#[derive(Debug)]
pub struct MemRegionsLookupTable {
    regions: BTreeMap<u64, MemRegion>,
}

impl MemRegionsLookupTable {
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        let mut map = BTreeMap::new();

        // Last region with same start wins
        for region in regions {
            map.insert(region.start_addr, region);
        }

        Self { regions: map }
    }

    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let region = self
            .regions
            .range(..=addr)
            .next_back()
            .map(|(_, r)| r)?;

        region.read(addr, out)
    }
}

//
// =========================
//  RegionsMemReader
// =========================
//

#[derive(Debug)]
pub struct RegionsMemReader {
    lookup: MemRegionsLookupTable,
}

impl RegionsMemReader {
    pub fn new(lookup: MemRegionsLookupTable) -> Self {
        Self { lookup }
    }

    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        self.lookup.read(addr, out)
    }
}

//
// =========================
//  ELF → MemRegion helpers
// =========================
//

pub fn elf_segment_to_mem_region(
    segment: &object::read::Segment<'_, '_>,
) -> Result<MemRegion, object::Error> {
    let addr = segment.address();
    let data = segment.data()?.to_vec();
    Ok(MemRegion::new(addr, data))
}

pub fn elf_section_to_mem_region(
    section: &object::read::Section<'_, '_>,
) -> Result<MemRegion, object::Error> {
    let addr = section.address();
    let data = section.data()?.to_vec();
    Ok(MemRegion::new(addr, data))
}
pub fn elf_segments_to_mem_regions<F>(
    obj: &object::File,
    filter: Option<F>,
) -> Result<Vec<MemRegion>, object::Error>
where
    F: Fn(&object::read::Segment<'_, '_>) -> bool,
{
    let mut region_by_start = BTreeMap::<u64, MemRegion>::new();

    for segment in obj.segments() {
        // Only real ELF segments
        let Ok(data) = segment.data() else {
            continue;
        };

        if data.is_empty() {
            continue;
        }

        if let Some(ref f) = filter {
            if !f(&segment) {
                continue;
            }
        }

        let region = elf_segment_to_mem_region(&segment)?;
        region_by_start.insert(region.start_addr, region);
    }

    Ok(region_by_start.into_values().collect())
}

pub fn elf_sections_to_mem_regions<F>(
    obj: &object::File,
    filter: Option<F>,
) -> Result<Vec<MemRegion>, object::Error>
where
    F: Fn(&object::read::Section<'_, '_>) -> bool,
{
    let mut regions = Vec::new();

    for section in obj.sections() {
        if let Some(ref f) = filter {
            if !f(&section) {
                continue;
            }
        }

        let region = elf_section_to_mem_region(&section)?;
        regions.push(region);
    }

    Ok(regions)
}

//
// =========================
//  Executable-only helpers
// =========================
//

pub fn elf_get_executable_segments_as_mem_regions(
    obj: &object::File,
) -> Result<Vec<MemRegion>, object::Error> {
    elf_segments_to_mem_regions(
        obj,
        Some(|seg: &object::read::Segment<'_, '_>| {
            match seg.flags() {
                object::read::SegmentFlags::Elf { p_flags } => {
                    p_flags & object::elf::PF_X != 0
                }
                _ => false,
            }
        }),
    )
}

pub fn elf_get_executable_sections_as_mem_regions(
    obj: &object::File,
) -> Result<Vec<MemRegion>, object::Error> {
    elf_sections_to_mem_regions(
        obj,
        Some(|sec: &object::read::Section<'_, '_>| {
            match sec.flags() {
                object::read::SectionFlags::Elf { sh_flags } => {
                    sh_flags & object::elf::SHF_EXECINSTR as u64 != 0
                }
                _ => false,
            }
        }),
    )
}

//
// =========================
//  ElfFileMemReader
// =========================
//

#[derive(Debug)]
pub struct ElfFileMemReader<'a, 'data> {
    pub obj: &'a object::File<'data>,
    pub regions_mem_reader: RegionsMemReader,
}

#[derive(Debug)]
pub enum ElfMemReaderError {
    NotMapped(u64),
    Object(object::Error),
}

impl From<object::Error> for ElfMemReaderError {
    fn from(e: object::Error) -> Self {
        ElfMemReaderError::Object(e)
    }
}

impl<'a, 'data> ElfFileMemReader<'a, 'data> {
    pub fn from_elf_segments(
        obj: &'a object::File<'data>,
    ) -> Result<Self, ElfMemReaderError> {
        let regions = elf_get_executable_segments_as_mem_regions(&obj)?;
        let lookup = MemRegionsLookupTable::new(regions);

        Ok(Self {
            obj,
            regions_mem_reader: RegionsMemReader::new(lookup),
        })
    }

    pub fn from_elf_sections(
        obj: &'a object::File<'data>,
    ) -> Result<Self, ElfMemReaderError> {
        let regions = elf_get_executable_sections_as_mem_regions(&obj)?;
        let lookup = MemRegionsLookupTable::new(regions);

        Ok(Self {
            obj,
            regions_mem_reader: RegionsMemReader::new(lookup),
        })
    }
}

//
// =========================
//  MemReader Implementation
// =========================
//
use std::fs;

pub fn load_elf(path: &str) -> object::File<'static> {
    let data = fs::read(path).expect("failed to read file");

    // Important: object::File borrows the buffer
    // So we must leak or store it somewhere long-lived
    let leaked = Box::leak(data.into_boxed_slice());

    object::File::parse(&*leaked).expect("failed to parse ELF")
}


impl<'a, 'data> rsleigh::MemReader for ElfFileMemReader<'a, 'data> {
    type Err = ElfMemReaderError;

    fn read(
        &self,
        addr: rsleigh::VnAddr,
        out_buf: &mut [u8],
    ) -> Result<usize, Self::Err> {
        let read = self
            .regions_mem_reader
            .read(addr.off, out_buf)
            .ok_or(ElfMemReaderError::NotMapped(addr.off))?;

        Ok(read)
    }
}
