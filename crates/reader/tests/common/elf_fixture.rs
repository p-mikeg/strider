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
use object::write::elf::{FileHeader, SectionHeader, Writer};

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

        debug_assert_eq!(sec_idx.0, 1);
        debug_assert_eq!(shstrtab_idx.0, 2);
    }
    buf
}
