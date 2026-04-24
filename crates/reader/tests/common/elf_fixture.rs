//! Synthetic ELF byte builders used by integration tests.
//!
//! All builders produce a complete ELF byte buffer that `object::File::parse`
//! can consume. Sections are placed at caller-chosen virtual addresses by
//! writing via `object::write::elf::Writer` (low-level API); the high-level
//! `object::write::Object` API always emits `sh_addr: 0`, which is useless
//! for testing a memory reader.

#![allow(dead_code)]

use object::Endianness;
use object::elf;
use object::write::elf::{FileHeader, ProgramHeader, SectionHeader, Writer};

/// Builds a minimal 64-bit little-endian x86-64 ELF with a single
/// `.text` section of `bytes` placed at virtual address `addr`.
///
/// Flags: `SHF_ALLOC | SHF_EXECINSTR`. `sh_type` is `SHT_PROGBITS`.
pub fn simple_text_elf(addr: u64, bytes: &[u8]) -> Vec<u8> {
    build_one_section_elf(OneSectionOpts {
        addr,
        data: bytes,
        endian: Endianness::Little,
        is_64: true,
        e_machine: elf::EM_X86_64,
        name: b".text",
        sh_type: elf::SHT_PROGBITS,
        sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
    })
}

/// Like `simple_text_elf` but lets the caller choose endianness. Used for
/// endianness round-trip tests.
pub fn simple_text_elf_with_endian(
    addr: u64,
    bytes: &[u8],
    endian: Endianness,
) -> Vec<u8> {
    build_one_section_elf(OneSectionOpts {
        addr,
        data: bytes,
        endian,
        is_64: true,
        e_machine: elf::EM_X86_64,
        name: b".text",
        sh_type: elf::SHT_PROGBITS,
        sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
    })
}

struct OneSectionOpts<'a> {
    addr: u64,
    data: &'a [u8],
    endian: Endianness,
    is_64: bool,
    e_machine: u16,
    name: &'a [u8],
    sh_type: u32,
    sh_flags: u64,
}

fn build_one_section_elf(opts: OneSectionOpts<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = Writer::new(opts.endian, opts.is_64, &mut buf);

        // Reserve indices.
        let _null_idx = w.reserve_null_section_index();
        let sec_name = w.add_section_name(opts.name);
        let sec_idx = w.reserve_section_index();
        let shstrtab_idx = w.reserve_shstrtab_section_index();

        // Reserve layout: file header, then section data, then section headers.
        w.reserve_file_header();

        let sec_offset = w.reserve(opts.data.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // Write.
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine: opts.e_machine,
            e_entry: opts.addr,
            e_flags: 0,
        })
        .expect("write file header");

        w.write(opts.data);
        w.write_shstrtab();

        // Section headers: null, our section, shstrtab.
        w.write_null_section_header();

        w.write_section_header(&SectionHeader {
            name: Some(sec_name),
            sh_type: opts.sh_type,
            sh_flags: opts.sh_flags,
            sh_addr: opts.addr,
            sh_offset: sec_offset as u64,
            sh_size: opts.data.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });

        w.write_shstrtab_section_header();

        assert_eq!(sec_idx.0, 1);
        assert_eq!(shstrtab_idx.0, 2);
    }
    buf
}

/// Description of one section in a fixture ELF.
#[derive(Clone, Debug)]
pub struct SectionSpec {
    pub name: &'static [u8],
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool,
    pub writable: bool,
    /// If true, section is `SHT_NOBITS` (no file-backed data). `data` is
    /// ignored except that its length becomes `sh_size`.
    pub nobits: bool,
}

impl SectionSpec {
    pub fn text(addr: u64, data: Vec<u8>) -> Self {
        Self { name: b".text", addr, data, exec: true, writable: false, nobits: false }
    }
    pub fn rodata(addr: u64, data: Vec<u8>) -> Self {
        Self { name: b".rodata", addr, data, exec: false, writable: false, nobits: false }
    }
    pub fn data(addr: u64, data: Vec<u8>) -> Self {
        Self { name: b".data", addr, data, exec: false, writable: true, nobits: false }
    }
    pub fn bss(addr: u64, size: usize) -> Self {
        Self {
            name: b".bss",
            addr,
            data: vec![0; size],
            exec: false,
            writable: true,
            nobits: true,
        }
    }
}

/// Builds a 64-bit little-endian x86-64 ELF with the given sections, in
/// order. Each section lands at its `addr`; the writer emits `SHT_PROGBITS`
/// (or `SHT_NOBITS` if `spec.nobits`) with `SHF_ALLOC` plus `SHF_EXECINSTR`
/// / `SHF_WRITE` per the spec.
///
/// Sections with `nobits == true` contribute nothing to the file on-disk
/// but still have a section header with the right `sh_size` and `sh_type`.
/// This is how `object` models `.bss`.
pub fn build_elf_with_sections(sections: &[SectionSpec]) -> Vec<u8> {
    build_sections_elf(sections, Endianness::Little, true, elf::EM_X86_64)
}

fn build_sections_elf(
    sections: &[SectionSpec],
    endian: Endianness,
    is_64: bool,
    e_machine: u16,
) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, is_64, &mut buf);

        let _null_idx = w.reserve_null_section_index();

        // Reserve one section name + index per spec, preserving order.
        let mut name_ids = Vec::with_capacity(sections.len());
        let mut sec_indices = Vec::with_capacity(sections.len());
        for spec in sections {
            name_ids.push(w.add_section_name(spec.name));
            sec_indices.push(w.reserve_section_index());
        }
        let _shstrtab_idx = w.reserve_shstrtab_section_index();

        // Reserve layout.
        w.reserve_file_header();

        // Each non-NOBITS section reserves file space equal to its data.
        let mut sec_offsets: Vec<u64> = Vec::with_capacity(sections.len());
        for spec in sections {
            if spec.nobits {
                sec_offsets.push(0);
            } else {
                sec_offsets.push(w.reserve(spec.data.len(), 1) as u64);
            }
        }
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // Write file header (no program headers in this builder — sections only).
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");

        // Section data.
        for spec in sections {
            if !spec.nobits {
                w.write(&spec.data);
            }
        }

        w.write_shstrtab();

        // Section headers.
        w.write_null_section_header();

        for (i, spec) in sections.iter().enumerate() {
            let mut sh_flags = u64::from(elf::SHF_ALLOC);
            if spec.exec {
                sh_flags |= u64::from(elf::SHF_EXECINSTR);
            }
            if spec.writable {
                sh_flags |= u64::from(elf::SHF_WRITE);
            }
            let sh_type = if spec.nobits { elf::SHT_NOBITS } else { elf::SHT_PROGBITS };
            let sh_size = spec.data.len() as u64;
            w.write_section_header(&SectionHeader {
                name: Some(name_ids[i]),
                sh_type,
                sh_flags,
                sh_addr: spec.addr,
                sh_offset: sec_offsets[i],
                sh_size,
                sh_link: 0,
                sh_info: 0,
                sh_addralign: 1,
                sh_entsize: 0,
            });
        }

        w.write_shstrtab_section_header();
    }
    buf
}

/// Description of one PT_LOAD segment in a fixture ELF.
#[derive(Clone, Debug)]
pub struct SegmentSpec {
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool,
}

/// Builds a 64-bit little-endian x86-64 ELF with the given segments, each
/// as a PT_LOAD with `p_vaddr = addr`, `p_flags = PF_R | (PF_X if exec)`.
///
/// A single `.segN`-named section is also emitted per segment so the file
/// also parses via the section view — but the typical consumer is the
/// segment-level readers.
pub fn build_elf_with_segments(segments: &[SegmentSpec]) -> Vec<u8> {
    let endian = Endianness::Little;
    let is_64 = true;

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, is_64, &mut buf);

        // Section index layout: null, [one per segment], shstrtab.
        let _null_idx = w.reserve_null_section_index();
        let mut name_ids = Vec::with_capacity(segments.len());
        let mut sec_indices = Vec::with_capacity(segments.len());
        for i in 0..segments.len() {
            // Writer::add_section_name takes &'a [u8] bound to the writer's
            // lifetime. Leaking per-call is acceptable in test-fixture code
            // that runs a handful of times per test binary.
            let owned: &'static [u8] =
                Box::leak(format!(".seg{i}").into_boxed_str().into_boxed_bytes());
            name_ids.push(w.add_section_name(owned));
            sec_indices.push(w.reserve_section_index());
        }
        let _shstrtab_idx = w.reserve_shstrtab_section_index();

        // Layout.
        w.reserve_file_header();
        w.reserve_program_headers(segments.len() as u32);

        let mut data_offsets: Vec<u64> = Vec::with_capacity(segments.len());
        for spec in segments {
            data_offsets.push(w.reserve(spec.data.len(), 1) as u64);
        }

        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine: elf::EM_X86_64,
            e_entry: segments.first().map_or(0, |s| s.addr),
            e_flags: 0,
        })
        .expect("write file header");

        w.write_align_program_headers();
        for (i, spec) in segments.iter().enumerate() {
            let mut p_flags = elf::PF_R;
            if spec.exec {
                p_flags |= elf::PF_X;
            }
            w.write_program_header(&ProgramHeader {
                p_type: elf::PT_LOAD,
                p_flags,
                p_offset: data_offsets[i],
                p_vaddr: spec.addr,
                p_paddr: spec.addr,
                p_filesz: spec.data.len() as u64,
                p_memsz: spec.data.len() as u64,
                p_align: 1,
            });
        }

        // Segment data.
        for spec in segments {
            w.write(&spec.data);
        }

        w.write_shstrtab();

        // Section headers: one SHT_PROGBITS per segment so the section
        // view is consistent with the segment view.
        w.write_null_section_header();
        for (i, spec) in segments.iter().enumerate() {
            let mut sh_flags = u64::from(elf::SHF_ALLOC);
            if spec.exec {
                sh_flags |= u64::from(elf::SHF_EXECINSTR);
            }
            w.write_section_header(&SectionHeader {
                name: Some(name_ids[i]),
                sh_type: elf::SHT_PROGBITS,
                sh_flags,
                sh_addr: spec.addr,
                sh_offset: data_offsets[i],
                sh_size: spec.data.len() as u64,
                sh_link: 0,
                sh_info: 0,
                sh_addralign: 1,
                sh_entsize: 0,
            });
        }
        w.write_shstrtab_section_header();

        let _ = sec_indices;
    }
    buf
}
