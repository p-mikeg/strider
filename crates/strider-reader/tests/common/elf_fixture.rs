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

/// One defined-symbol `R_MIPS_REL32` fixture: a big-endian MIPS32 ELF
/// whose `.data.rel.ro` slot at `slot_addr` carries a `REL` relocation
/// of type `R_MIPS_REL32` pointing at a defined symbol `target` located
/// at `sym_addr`.  Built by hand via the low-level `Writer` (the
/// high-level `object::write::Object` API can't place a section at a
/// chosen `sh_addr`, and its dynamic-symbol layout is opaque).
///
/// Returns the ELF bytes plus the two addresses the test asserts on.
pub struct Mips32Rel32Fixture {
    pub bytes: Vec<u8>,
    /// Virtual address of the 4-byte relocation site (in `.data.rel.ro`).
    pub slot_addr: u64,
    /// Virtual address (`st_value`) of the defined target symbol.
    pub sym_addr: u64,
}

/// Builds a defined-symbol [`Mips32Rel32Fixture`] (the REL32 points at
/// the defined `func` symbol).  See [`build_mips32be_rel32_elf_with`].
pub fn build_mips32be_rel32_elf() -> Mips32Rel32Fixture {
    build_mips32be_rel32_elf_with(/* defined_symbol */ true)
}

/// Builds [`Mips32Rel32Fixture`].  Layout:
/// - `.text`     — one dummy instruction word at `0x1000` (the symbol
///   `func` is defined here, `st_value = sym_addr`).
/// - `.data.rel.ro` — one 4-byte slot at `slot_addr`, initial value 0.
/// - `.dynsym`   — null symbol + one defined `func` symbol.
/// - `.dynstr`   — string table for `.dynsym`.
/// - `.rel.dyn`  — one `Elf32_Rel { r_offset = slot_addr,
///   r_info = (sym_index << 8) | R_MIPS_REL32 }`, `sh_link = .dynsym`.
///
/// When `defined_symbol` is `true` the reloc's `r_sym` is symbol 1
/// (the defined `func`); `object` reports a `RelocationTarget::Symbol`.
/// When `false`, `r_sym` is 0 (STN_UNDEF) — `object` reports
/// `RelocationTarget::Absolute`, exercising the addend-only path.
///
/// `object::dynamic_relocations()` iterates `SHT_REL` sections whose
/// `sh_link` is the `SHT_DYNSYM` section, so wiring `sh_link`
/// correctly is what makes the reloc visible.
pub fn build_mips32be_rel32_elf_with(defined_symbol: bool) -> Mips32Rel32Fixture {
    let endian = Endianness::Big;
    let sym_addr: u64 = 0x1000; // `.text` / `func`
    let slot_addr: u64 = 0x2000; // `.data.rel.ro` slot

    let text = vec![0u8, 0, 0, 0]; // one dummy MIPS word
    let slot = vec![0u8, 0, 0, 0]; // REL site, starts zeroed

    // `.dynstr`: index 0 is the empty string; "func" follows.
    let mut dynstr = vec![0u8];
    let func_name_off = dynstr.len() as u32;
    dynstr.extend_from_slice(b"func\0");

    // `.dynsym`: symbol 0 is the reserved null entry; symbol 1 is the
    // defined `func` (st_value = sym_addr, st_shndx = .text index).
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

    // `.rel.dyn`: one Elf32_Rel (8 bytes): r_offset(4) r_info(4).
    // r_info = (sym << 8) | type for ELF32.  `sym = 0` (STN_UNDEF) makes
    // `object` report a `RelocationTarget::Absolute` (addend-only path);
    // `sym = 1` makes it a `RelocationTarget::Symbol` (the `S + A` path).
    let r_sym = if defined_symbol { sym_index } else { 0 };
    let r_info: u32 = (r_sym << 8) | u32::from(elf::R_MIPS_REL32 as u8);
    let mut reldyn = Vec::with_capacity(8);
    reldyn.extend_from_slice(&(slot_addr as u32).to_be_bytes());
    reldyn.extend_from_slice(&r_info.to_be_bytes());

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, /* is_64 */ false, &mut buf);

        // Section index layout (must match `text_shndx` and dynsym link):
        //   0 = null, 1 = .text, 2 = .data.rel.ro, 3 = .dynsym,
        //   4 = .dynstr, 5 = .rel.dyn, 6 = .shstrtab.
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

        // Reserve layout: file header, program headers (PT_LOAD for
        // `.text` and `.data.rel.ro`), then each section's bytes,
        // then shstrtab, then section headers.  PT_LOADs are
        // required: the loader dispatches ET_DYN to program headers,
        // so without them `apply_elf_relocations`'s region map would
        // be empty and the REL32 site would have no region to patch.
        w.reserve_file_header();
        w.reserve_program_headers(2);
        // Reserve every block at align 1 so the reserved offsets match
        // the positions `w.write` lands at (no implicit padding the
        // plain `write` calls below wouldn't reproduce).  File-offset
        // alignment is irrelevant for the in-memory addresses the
        // applier patches.
        let text_off = w.reserve(text.len(), 1);
        let slot_off = w.reserve(slot.len(), 1);
        let dynsym_off = w.reserve(dynsym.len(), 1);
        let dynstr_off = w.reserve(dynstr.len(), 1);
        let reldyn_off = w.reserve(reldyn.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // ET_DYN: shared object, the shape `apply_elf_relocations`
        // targets.
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_DYN,
            e_machine: elf::EM_MIPS,
            e_entry: sym_addr,
            e_flags: 0,
        })
        .expect("write file header");

        // Program headers covering `.text` (R + X) and `.data.rel.ro`
        // (R + W).  These match the sections' `sh_addr` so the
        // segment-walker and section-walker would produce equivalent
        // regions; only the segment-walker is consulted for ET_DYN.
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
        // 1: .text (SHF_ALLOC | SHF_EXECINSTR).
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
        // 2: .data.rel.ro (SHF_ALLOC | SHF_WRITE) — the relocation site.
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
        // 3: .dynsym (SHT_DYNSYM); sh_link = .dynstr, sh_info = index of
        // first non-local symbol (1, since symbol 0 is the null entry).
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
        // 4: .dynstr (SHT_STRTAB).
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
        // 5: .rel.dyn (SHT_REL); sh_link = .dynsym so
        // `dynamic_relocations()` picks it up.
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

/// One x86-64 PC-relative (`R_X86_64_PC32`, `RelocationKind::Relative`)
/// RELA fixture: a 64-bit little-endian ET_DYN whose writable
/// `.data.rel.ro` slot at `slot_addr` carries a `RELA` relocation of type
/// `R_X86_64_PC32` against a defined `func` symbol at `sym_addr`, with the
/// caller-chosen (possibly negative) addend.  The patched 4-byte field
/// should read `(sym_addr + addend - site_addr)` modulo 2^32.
///
/// `slot_len` lets the caller make the `.data.rel.ro` section *shorter*
/// than the 4-byte relocation field needs (used to exercise the autoload
/// width-straddle case): the relocation site is placed at
/// `slot_addr + reloc_off`, so a `slot_len` that ends before
/// `reloc_off + 4` produces a field that straddles the staged region's end.
pub struct X86Pc32Fixture {
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

/// Builds an [`X86Pc32Fixture`].  `slot_len` is the byte length of the
/// `.data.rel.ro` section (its file-backed data); `reloc_off` is the
/// offset of the 4-byte relocation site within that section.
///
/// Mirrors [`build_mips32be_rel32_elf_with`] but for x86-64 RELA
/// (24-byte entries with an explicit `r_addend`) and a `Relative`
/// (`S + A - P`) reloc kind.
pub fn build_x86_64_pc32_rela_elf(slot_len: usize, reloc_off: u64, addend: i64) -> X86Pc32Fixture {
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

    // `.dynsym`: Elf64_Sym is 24 bytes: name(4) info(1) other(1) shndx(2)
    // value(8) size(8).  Symbol 0 is the null entry; symbol 1 is `func`.
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

    // `.rela.dyn`: one Elf64_Rela (24 bytes): r_offset(8) r_info(8)
    // r_addend(8).  r_info = (sym << 32) | type for ELF64.
    let r_info: u64 = (u64::from(sym_index) << 32) | u64::from(elf::R_X86_64_PC32);
    let mut reladyn = Vec::with_capacity(24);
    reladyn.extend_from_slice(&site_addr.to_le_bytes());
    reladyn.extend_from_slice(&r_info.to_le_bytes());
    reladyn.extend_from_slice(&addend.to_le_bytes()); // signed addend, 2's-complement

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, /* is_64 */ true, &mut buf);

        // Section index layout: 0 = null, 1 = .text, 2 = .data.rel.ro,
        // 3 = .dynsym, 4 = .dynstr, 5 = .rela.dyn, 6 = .shstrtab.
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
        // 1: .text
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
        // 2: .data.rel.ro — the relocation site (writable).
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
        // 3: .dynsym
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
        // 4: .dynstr
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
        // 5: .rela.dyn (SHT_RELA); sh_link = .dynsym so
        // `dynamic_relocations()` picks it up.
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
pub fn simple_text_elf_with_endian(addr: u64, bytes: &[u8], endian: Endianness) -> Vec<u8> {
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

        // Reserve layout: file header, program header, then section
        // data, then section headers.  The PT_LOAD program header is
        // required for the loader to see this ELF: ET_EXEC dispatches
        // to PT_LOAD segments (the runtime memory layout), so emitting
        // sections alone would leave the loader with nothing to map.
        w.reserve_file_header();
        w.reserve_program_headers(1);

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

        // One PT_LOAD program header covering the single section.
        // PF_R is always set; PF_X echoes SHF_EXECINSTR; PF_W echoes
        // SHF_WRITE.  The loader's code+rodata filter rejects PF_W
        // segments — the corresponding `sh_flags` filter rejects
        // SHF_WRITE sections — so the two views stay equivalent.
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
        Self {
            name: b".text",
            addr,
            data,
            exec: true,
            writable: false,
            nobits: false,
        }
    }
    pub fn rodata(addr: u64, data: Vec<u8>) -> Self {
        Self {
            name: b".rodata",
            addr,
            data,
            exec: false,
            writable: false,
            nobits: false,
        }
    }
    pub fn data(addr: u64, data: Vec<u8>) -> Self {
        Self {
            name: b".data",
            addr,
            data,
            exec: false,
            writable: true,
            nobits: false,
        }
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

/// Builds a 64-bit little-endian x86-64 ELF (ET_REL — a relocatable
/// object file) with the given sections, in order.  Each section
/// lands at its `addr`; the writer emits `SHT_PROGBITS` (or
/// `SHT_NOBITS` if `spec.nobits`) with `SHF_ALLOC` plus
/// `SHF_EXECINSTR` / `SHF_WRITE` per the spec.
///
/// ET_REL is the right `e_type` for a section-only fixture: a
/// linked ET_EXEC binary describes its runtime layout via PT_LOAD
/// program headers (and the analyser dispatches to those for ET_EXEC
/// / ET_DYN), so a section-only ELF marked ET_EXEC would have no
/// runtime layout to walk.  ET_REL has no program headers by
/// definition, so the analyser walks sections — which is exactly
/// what these fixtures intend to exercise.
///
/// Sections with `nobits == true` contribute nothing to the file
/// on-disk but still have a section header with the right `sh_size`
/// and `sh_type`.  This is how `object` models `.bss`.
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
        for spec in sections {
            name_ids.push(w.add_section_name(spec.name));
            w.reserve_section_index();
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

        // Write file header — `ET_REL` because this builder emits only
        // section headers (no program headers).  A linked ET_EXEC binary
        // describes its runtime layout via PT_LOAD, which the loader
        // dispatcher walks in preference to sections; ET_REL is the
        // kind whose layout *is* its section table.
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_REL,
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
        for i in 0..segments.len() {
            // Writer::add_section_name takes &'a [u8] bound to the writer's
            // lifetime. Leaking per-call is acceptable in test-fixture code
            // that runs a handful of times per test binary.
            let owned: &'static [u8] =
                Box::leak(format!(".seg{i}").into_boxed_str().into_boxed_bytes());
            name_ids.push(w.add_section_name(owned));
            w.reserve_section_index();
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
    }
    buf
}
