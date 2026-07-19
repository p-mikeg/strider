//! Synthetic ELF byte builders, each producing a complete buffer
//! `object::File::parse` accepts.
//!
//! Everything goes through the low-level `object::write::elf::Writer` because
//! the high-level `object::write::Object` API always emits `sh_addr: 0`, which
//! is useless for testing a memory reader.

#![allow(dead_code)]

use object::write::elf::{FileHeader, ProgramHeader, SectionHeader, Writer};
use object::{Endianness, elf};

/// A big-endian MIPS32 ELF whose `.data.rel.ro` slot at `slot_addr` carries a
/// `REL` relocation of type `R_MIPS_REL32` against a defined symbol at
/// `sym_addr`.
pub(crate) struct Mips32Rel32Fixture {
    pub bytes: Vec<u8>,
    /// Virtual address of the 4-byte relocation site (in `.data.rel.ro`).
    pub slot_addr: u64,
    /// Virtual address (`st_value`) of the defined target symbol.
    pub sym_addr: u64,
}

pub(crate) fn build_mips32be_rel32_elf() -> Mips32Rel32Fixture {
    build_mips32be_rel32_elf_with(/* defined_symbol */ true)
}

/// Layout:
/// - `.text`: one dummy instruction word at `0x1000`, where `func` is defined.
/// - `.data.rel.ro`: one 4-byte slot at `slot_addr`, initially 0.
/// - `.dynsym`: null symbol plus the defined `func`.
/// - `.dynstr`: string table for `.dynsym`.
/// - `.rel.dyn`: one `Elf32_Rel { r_offset = slot_addr,
///   r_info = (sym_index << 8) | R_MIPS_REL32 }`, `sh_link = .dynsym`.
///
/// `defined_symbol` picks `r_sym` 1 (a `RelocationTarget::Symbol`) or 0 /
/// STN_UNDEF (a `RelocationTarget::Absolute`, the addend-only path).
///
/// `object::dynamic_relocations()` only iterates `SHT_REL` sections whose
/// `sh_link` is the `SHT_DYNSYM` section, so that wiring is what makes the
/// reloc visible at all.
pub(crate) fn build_mips32be_rel32_elf_with(defined_symbol: bool) -> Mips32Rel32Fixture {
    let endian = Endianness::Big;
    let sym_addr: u64 = 0x1000; // `.text` / `func`
    let slot_addr: u64 = 0x2000; // `.data.rel.ro` slot

    let text = vec![0u8, 0, 0, 0]; // one dummy MIPS word
    let slot = vec![0u8, 0, 0, 0]; // REL site, starts zeroed

    // `.dynstr`: index 0 is the empty string; "func" follows.
    let mut dynstr = vec![0u8];
    let func_name_off = dynstr.len() as u32;
    dynstr.extend_from_slice(b"func\0");

    // Symbol 0 is the reserved null entry, symbol 1 the defined `func`.
    // Elf32_Sym is 16 bytes: name(4) value(4) size(4) info(1) other(1) shndx(2).
    let sym_index: u32 = 1;
    let text_shndx: u16 = 1; // `.text` is section index 1 (see below)
    let mut dynsym = vec![0u8; 16]; // null symbol
    let mut func_sym = Vec::with_capacity(16);
    func_sym.extend_from_slice(&func_name_off.to_be_bytes()); // st_name
    func_sym.extend_from_slice(&(sym_addr as u32).to_be_bytes()); // st_value
    func_sym.extend_from_slice(&0u32.to_be_bytes()); // st_size
    // st_info: STB_GLOBAL << 4 | STT_FUNC
    func_sym.push((elf::STB_GLOBAL << 4) | elf::STT_FUNC);
    func_sym.push(0); // st_other
    func_sym.extend_from_slice(&text_shndx.to_be_bytes()); // st_shndx
    dynsym.extend_from_slice(&func_sym);

    // One Elf32_Rel, 8 bytes: r_offset(4) r_info(4), where
    // r_info = (sym << 8) | type for ELF32.
    let r_sym = if defined_symbol { sym_index } else { 0 };
    let r_info: u32 = (r_sym << 8) | u32::from(elf::R_MIPS_REL32 as u8);
    let mut reldyn = Vec::with_capacity(8);
    reldyn.extend_from_slice(&(slot_addr as u32).to_be_bytes());
    reldyn.extend_from_slice(&r_info.to_be_bytes());

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, /* is_64 */ false, &mut buf);

        // Must match `text_shndx` and the dynsym link: 0 = null, 1 = .text,
        // 2 = .data.rel.ro, 3 = .dynsym, 4 = .dynstr, 5 = .rel.dyn,
        // 6 = .shstrtab.
        let _null = w.reserve_null_section_index();
        let text_name = w.add_section_name(b".text");
        let text_idx = w.reserve_section_index();
        let slot_name = w.add_section_name(b".data.rel.ro");
        let _slot_idx = w.reserve_section_index();
        let dynsym_name = w.add_section_name(b".dynsym");
        let dynsym_idx = w.reserve_section_index();
        let dynstr_name = w.add_section_name(b".dynstr");
        let dynstr_idx = w.reserve_section_index();
        let reldyn_name = w.add_section_name(b".rel.dyn");
        let _reldyn_idx = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        assert_eq!(text_idx.0, u32::from(text_shndx));
        assert_eq!(dynsym_idx.0, 3);

        // The PT_LOADs are required: ET_DYN dispatches to program headers, so
        // without them the region map is empty and the REL32 site has nothing
        // to patch.
        w.reserve_file_header();
        w.reserve_program_headers(2);
        // Align 1 throughout so reserved offsets match where `w.write` lands,
        // with no implicit padding the plain `write` calls below wouldn't
        // reproduce. File-offset alignment doesn't affect the in-memory
        // addresses the applier patches.
        let text_off = w.reserve(text.len(), 1);
        let slot_off = w.reserve(slot.len(), 1);
        let dynsym_off = w.reserve(dynsym.len(), 1);
        let dynstr_off = w.reserve(dynstr.len(), 1);
        let reldyn_off = w.reserve(reldyn.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // ET_DYN, the shape `apply_elf_relocations` targets.
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_DYN,
            e_machine: elf::EM_MIPS,
            e_entry: sym_addr,
            e_flags: 0,
        })
        .expect("write file header");

        // These match the sections' `sh_addr`, so the segment and section
        // walkers would produce equivalent regions; ET_DYN consults only the
        // former.
        w.write_align_program_headers();
        w.write_program_header(&ProgramHeader {
            p_type: elf::PT_LOAD,
            p_flags: elf::PF_R | elf::PF_X,
            p_offset: text_off as u64,
            p_vaddr: sym_addr,
            p_paddr: sym_addr,
            p_filesz: text.len() as u64,
            p_memsz: text.len() as u64,
            p_align: 1,
        });
        w.write_program_header(&ProgramHeader {
            p_type: elf::PT_LOAD,
            p_flags: elf::PF_R | elf::PF_W,
            p_offset: slot_off as u64,
            p_vaddr: slot_addr,
            p_paddr: slot_addr,
            p_filesz: slot.len() as u64,
            p_memsz: slot.len() as u64,
            p_align: 1,
        });

        w.write(&text);
        w.write(&slot);
        w.write(&dynsym);
        w.write(&dynstr);
        w.write(&reldyn);
        w.write_shstrtab();

        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(text_name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
            sh_addr: sym_addr,
            sh_offset: text_off as u64,
            sh_size: text.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 4,
            sh_entsize: 0,
        });
        // The relocation site.
        w.write_section_header(&SectionHeader {
            name: Some(slot_name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_WRITE),
            sh_addr: slot_addr,
            sh_offset: slot_off as u64,
            sh_size: slot.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 4,
            sh_entsize: 0,
        });
        // sh_info is the index of the first non-local symbol, 1 here since
        // symbol 0 is the null entry.
        w.write_section_header(&SectionHeader {
            name: Some(dynsym_name),
            sh_type: elf::SHT_DYNSYM,
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0,
            sh_offset: dynsym_off as u64,
            sh_size: dynsym.len() as u64,
            sh_link: dynstr_idx.0,
            sh_info: 1,
            sh_addralign: 4,
            sh_entsize: 16,
        });
        w.write_section_header(&SectionHeader {
            name: Some(dynstr_name),
            sh_type: elf::SHT_STRTAB,
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0,
            sh_offset: dynstr_off as u64,
            sh_size: dynstr.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        // sh_link = .dynsym is what makes `dynamic_relocations()` pick this up.
        w.write_section_header(&SectionHeader {
            name: Some(reldyn_name),
            sh_type: elf::SHT_REL,
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0,
            sh_offset: reldyn_off as u64,
            sh_size: reldyn.len() as u64,
            sh_link: dynsym_idx.0,
            sh_info: 0,
            sh_addralign: 4,
            sh_entsize: 8,
        });
        w.write_shstrtab_section_header();
    }

    Mips32Rel32Fixture {
        bytes: buf,
        slot_addr,
        sym_addr,
    }
}

/// A 64-bit little-endian ET_DYN whose writable `.data.rel.ro` slot carries an
/// `R_X86_64_PC32` RELA against a defined `func`, with a caller-chosen
/// (possibly negative) addend. The patched 4-byte field reads
/// `(sym_addr + addend - site_addr)` mod 2^32.
///
/// `slot_len` can be made shorter than the 4-byte field needs: the site sits at
/// `slot_addr + reloc_off`, so a `slot_len` ending before `reloc_off + 4`
/// produces a field straddling the staged region's end.
pub(crate) struct X86Pc32Fixture {
    pub bytes: Vec<u8>,
    /// Virtual address of the 4-byte relocation site.
    pub site_addr: u64,
    /// Start address (and section base) of the `.data.rel.ro` slot.
    pub slot_addr: u64,
    /// Virtual address (`st_value`) of the defined target symbol.
    pub sym_addr: u64,
    /// The signed addend baked into the RELA entry.
    pub addend: i64,
}

/// `slot_len` is the `.data.rel.ro` section's file-backed length and
/// `reloc_off` the site's offset within it.
///
/// Mirrors [`build_mips32be_rel32_elf_with`] for x86-64 RELA (24-byte entries
/// with an explicit `r_addend`) and a `Relative` (`S + A - P`) kind.
pub(crate) fn build_x86_64_pc32_rela_elf(
    slot_len: usize,
    reloc_off: u64,
    addend: i64,
) -> X86Pc32Fixture {
    let endian = Endianness::Little;
    let sym_addr: u64 = 0x1000; // `.text` / `func`
    let slot_addr: u64 = 0x2000; // `.data.rel.ro` section base
    let site_addr = slot_addr + reloc_off;

    let text = vec![0u8, 0, 0, 0]; // one dummy word; `func` defined here
    let slot = vec![0u8; slot_len]; // RELA site, starts zeroed

    // `.dynstr`: index 0 is the empty string; "func" follows.
    let mut dynstr = vec![0u8];
    let func_name_off = dynstr.len() as u32;
    dynstr.extend_from_slice(b"func\0");

    // Elf64_Sym is 24 bytes: name(4) info(1) other(1) shndx(2) value(8)
    // size(8). Symbol 0 is the null entry, symbol 1 is `func`.
    let sym_index: u32 = 1;
    let text_shndx: u16 = 1; // `.text` is section index 1
    let mut dynsym = vec![0u8; 24]; // null symbol
    let mut func_sym = Vec::with_capacity(24);
    func_sym.extend_from_slice(&func_name_off.to_le_bytes()); // st_name
    func_sym.push((elf::STB_GLOBAL << 4) | elf::STT_FUNC); // st_info
    func_sym.push(0); // st_other
    func_sym.extend_from_slice(&text_shndx.to_le_bytes()); // st_shndx
    func_sym.extend_from_slice(&sym_addr.to_le_bytes()); // st_value
    func_sym.extend_from_slice(&0u64.to_le_bytes()); // st_size
    dynsym.extend_from_slice(&func_sym);

    // One Elf64_Rela, 24 bytes: r_offset(8) r_info(8) r_addend(8), where
    // r_info = (sym << 32) | type for ELF64.
    let r_info: u64 = (u64::from(sym_index) << 32) | u64::from(elf::R_X86_64_PC32);
    let mut reladyn = Vec::with_capacity(24);
    reladyn.extend_from_slice(&site_addr.to_le_bytes());
    reladyn.extend_from_slice(&r_info.to_le_bytes());
    reladyn.extend_from_slice(&addend.to_le_bytes()); // signed addend, 2's-complement

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, /* is_64 */ true, &mut buf);

        // 0 = null, 1 = .text, 2 = .data.rel.ro, 3 = .dynsym, 4 = .dynstr,
        // 5 = .rela.dyn, 6 = .shstrtab.
        let _null = w.reserve_null_section_index();
        let text_name = w.add_section_name(b".text");
        let text_idx = w.reserve_section_index();
        let slot_name = w.add_section_name(b".data.rel.ro");
        let _slot_idx = w.reserve_section_index();
        let dynsym_name = w.add_section_name(b".dynsym");
        let dynsym_idx = w.reserve_section_index();
        let dynstr_name = w.add_section_name(b".dynstr");
        let dynstr_idx = w.reserve_section_index();
        let reladyn_name = w.add_section_name(b".rela.dyn");
        let _reladyn_idx = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        assert_eq!(text_idx.0, u32::from(text_shndx));
        assert_eq!(dynsym_idx.0, 3);

        w.reserve_file_header();
        w.reserve_program_headers(2);
        let text_off = w.reserve(text.len(), 1);
        let slot_off = w.reserve(slot.len(), 1);
        let dynsym_off = w.reserve(dynsym.len(), 1);
        let dynstr_off = w.reserve(dynstr.len(), 1);
        let reladyn_off = w.reserve(reladyn.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_DYN,
            e_machine: elf::EM_X86_64,
            e_entry: sym_addr,
            e_flags: 0,
        })
        .expect("write file header");

        w.write_align_program_headers();
        w.write_program_header(&ProgramHeader {
            p_type: elf::PT_LOAD,
            p_flags: elf::PF_R | elf::PF_X,
            p_offset: text_off as u64,
            p_vaddr: sym_addr,
            p_paddr: sym_addr,
            p_filesz: text.len() as u64,
            p_memsz: text.len() as u64,
            p_align: 1,
        });
        w.write_program_header(&ProgramHeader {
            p_type: elf::PT_LOAD,
            p_flags: elf::PF_R | elf::PF_W,
            p_offset: slot_off as u64,
            p_vaddr: slot_addr,
            p_paddr: slot_addr,
            p_filesz: slot.len() as u64,
            p_memsz: slot.len() as u64,
            p_align: 1,
        });

        w.write(&text);
        w.write(&slot);
        w.write(&dynsym);
        w.write(&dynstr);
        w.write(&reladyn);
        w.write_shstrtab();

        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(text_name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
            sh_addr: sym_addr,
            sh_offset: text_off as u64,
            sh_size: text.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 4,
            sh_entsize: 0,
        });
        // The relocation site.
        w.write_section_header(&SectionHeader {
            name: Some(slot_name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_WRITE),
            sh_addr: slot_addr,
            sh_offset: slot_off as u64,
            sh_size: slot.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_section_header(&SectionHeader {
            name: Some(dynsym_name),
            sh_type: elf::SHT_DYNSYM,
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0,
            sh_offset: dynsym_off as u64,
            sh_size: dynsym.len() as u64,
            sh_link: dynstr_idx.0,
            sh_info: 1,
            sh_addralign: 8,
            sh_entsize: 24,
        });
        w.write_section_header(&SectionHeader {
            name: Some(dynstr_name),
            sh_type: elf::SHT_STRTAB,
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0,
            sh_offset: dynstr_off as u64,
            sh_size: dynstr.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        // sh_link = .dynsym is what makes `dynamic_relocations()` pick this up.
        w.write_section_header(&SectionHeader {
            name: Some(reladyn_name),
            sh_type: elf::SHT_RELA,
            sh_flags: u64::from(elf::SHF_ALLOC),
            sh_addr: 0,
            sh_offset: reladyn_off as u64,
            sh_size: reladyn.len() as u64,
            sh_link: dynsym_idx.0,
            sh_info: 0,
            sh_addralign: 8,
            sh_entsize: 24,
        });
        w.write_shstrtab_section_header();
    }

    X86Pc32Fixture {
        bytes: buf,
        site_addr,
        slot_addr,
        sym_addr,
        addend,
    }
}

/// A minimal x86-64 ELF with one `SHT_PROGBITS`, `SHF_ALLOC | SHF_EXECINSTR`
/// `.text` section of `bytes` at `addr`.
pub(crate) fn simple_text_elf(addr: u64, bytes: &[u8]) -> Vec<u8> {
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

/// `simple_text_elf` with caller-chosen endianness, for round-trip tests.
pub(crate) fn simple_text_elf_with_endian(addr: u64, bytes: &[u8], endian: Endianness) -> Vec<u8> {
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

        let _null_idx = w.reserve_null_section_index();
        let sec_name = w.add_section_name(opts.name);
        let sec_idx = w.reserve_section_index();
        let shstrtab_idx = w.reserve_shstrtab_section_index();

        // The PT_LOAD is required for the loader to see this ELF at all:
        // ET_EXEC dispatches to segments, so sections alone would leave
        // nothing to map.
        w.reserve_file_header();
        w.reserve_program_headers(1);

        let sec_offset = w.reserve(opts.data.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_EXEC,
            e_machine: opts.e_machine,
            e_entry: opts.addr,
            e_flags: 0,
        })
        .expect("write file header");

        // PF_X echoes SHF_EXECINSTR and PF_W echoes SHF_WRITE, keeping the
        // segment and section views equivalent under the loader's filters.
        let mut p_flags = elf::PF_R;
        if opts.sh_flags & u64::from(elf::SHF_EXECINSTR) != 0 {
            p_flags |= elf::PF_X;
        }
        if opts.sh_flags & u64::from(elf::SHF_WRITE) != 0 {
            p_flags |= elf::PF_W;
        }
        w.write_align_program_headers();
        w.write_program_header(&ProgramHeader {
            p_type: elf::PT_LOAD,
            p_flags,
            p_offset: sec_offset as u64,
            p_vaddr: opts.addr,
            p_paddr: opts.addr,
            p_filesz: opts.data.len() as u64,
            p_memsz: opts.data.len() as u64,
            p_align: 1,
        });

        w.write(opts.data);
        w.write_shstrtab();

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

#[derive(Clone, Debug)]
pub(crate) struct SectionSpec {
    pub name: &'static [u8],
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool,
    pub writable: bool,
    /// `SHT_NOBITS`: no file-backed data, and `data` contributes only its
    /// length as `sh_size`.
    pub nobits: bool,
}

impl SectionSpec {
    pub(crate) fn text(addr: u64, data: Vec<u8>) -> Self {
        Self {
            name: b".text",
            addr,
            data,
            exec: true,
            writable: false,
            nobits: false,
        }
    }
    pub(crate) fn rodata(addr: u64, data: Vec<u8>) -> Self {
        Self {
            name: b".rodata",
            addr,
            data,
            exec: false,
            writable: false,
            nobits: false,
        }
    }
    pub(crate) fn data(addr: u64, data: Vec<u8>) -> Self {
        Self {
            name: b".data",
            addr,
            data,
            exec: false,
            writable: true,
            nobits: false,
        }
    }
    pub(crate) fn bss(addr: u64, size: usize) -> Self {
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

/// An x86-64 ET_REL with the given sections, in order, each at its `addr`.
///
/// ET_REL is the right `e_type` for a section-only fixture: ET_EXEC / ET_DYN
/// dispatch to PT_LOAD program headers, which a section-only ELF doesn't have,
/// so it would present no runtime layout to walk. ET_REL has no program headers
/// by definition, so the section walk under test is what runs.
///
/// A `nobits` section contributes no file bytes but still gets a header with
/// the right `sh_size` and `sh_type`, which is how `object` models `.bss`.
pub(crate) fn build_elf_with_sections(sections: &[SectionSpec]) -> Vec<u8> {
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

        let mut name_ids = Vec::with_capacity(sections.len());
        for spec in sections {
            name_ids.push(w.add_section_name(spec.name));
            w.reserve_section_index();
        }
        let _shstrtab_idx = w.reserve_shstrtab_section_index();

        w.reserve_file_header();

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

        // ET_REL because this builder emits no program headers; see the
        // entry point's docs.
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_REL,
            e_machine,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");

        for spec in sections {
            if !spec.nobits {
                w.write(&spec.data);
            }
        }

        w.write_shstrtab();

        w.write_null_section_header();

        for (i, spec) in sections.iter().enumerate() {
            let mut sh_flags = u64::from(elf::SHF_ALLOC);
            if spec.exec {
                sh_flags |= u64::from(elf::SHF_EXECINSTR);
            }
            if spec.writable {
                sh_flags |= u64::from(elf::SHF_WRITE);
            }
            let sh_type = if spec.nobits {
                elf::SHT_NOBITS
            } else {
                elf::SHT_PROGBITS
            };
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

#[derive(Clone, Debug)]
pub(crate) struct SegmentSpec {
    pub addr: u64,
    pub data: Vec<u8>,
    pub exec: bool,
}

/// An x86-64 ELF with the given segments, each a PT_LOAD with `p_vaddr = addr`
/// and `p_flags = PF_R | (PF_X if exec)`.
///
/// A `.segN` section is emitted per segment so the file parses through the
/// section view too, though the intended consumer is the segment-level readers.
pub(crate) fn build_elf_with_segments(segments: &[SegmentSpec]) -> Vec<u8> {
    let endian = Endianness::Little;
    let is_64 = true;

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, is_64, &mut buf);

        let _null_idx = w.reserve_null_section_index();
        let mut name_ids = Vec::with_capacity(segments.len());
        for i in 0..segments.len() {
            // `add_section_name` takes a slice bound to the writer's lifetime.
            // Leaking is acceptable in fixture code that runs a handful of
            // times per test binary.
            let owned: &'static [u8] =
                Box::leak(format!(".seg{i}").into_boxed_str().into_boxed_bytes());
            name_ids.push(w.add_section_name(owned));
            w.reserve_section_index();
        }
        let _shstrtab_idx = w.reserve_shstrtab_section_index();

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

        for spec in segments {
            w.write(&spec.data);
        }

        w.write_shstrtab();

        // One SHT_PROGBITS per segment, keeping the section view consistent
        // with the segment view.
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
    }
    buf
}
