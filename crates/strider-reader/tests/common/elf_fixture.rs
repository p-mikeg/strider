//! Synthetic ELF byte builders, each producing a complete buffer
//! `object::File::parse` accepts.
//!
//! Everything goes through the low-level `object::write::elf::Writer` because
//! the high-level `object::write::Object` API always emits `sh_addr: 0`, which
//! is useless for testing a memory reader.

#![allow(dead_code)]

use object::write::elf::{FileHeader, ProgramHeader, SectionHeader, Writer};
use object::{Endianness, elf};

/// Bytes [`RelPlacement::outer_load`]'s mapping starts ahead of the slot.
const OUTER_LEAD: u64 = 8;

/// An ELF whose `.data.rel.ro` slot at `slot_addr` carries one `SHT_REL`
/// relocation against a symbol defined at `sym_addr`.
pub(crate) struct RelFixture {
    pub bytes: Vec<u8>,
    /// Virtual address of the relocation site (in `.data.rel.ro`).
    pub slot_addr: u64,
    /// Virtual address (`st_value`) of the defined target symbol.
    pub sym_addr: u64,
}

pub(crate) struct RelOpts {
    pub endian: Endianness,
    pub is_64: bool,
    pub e_machine: u16,
    /// Whole type half of `r_info`: the 8-bit type on ELF32, MIPS64's packed
    /// `r_ssym | r_type3 | r_type2 | r_type` on ELF64.
    pub r_type: u32,
    /// `r_sym` 1 (a `RelocationTarget::Symbol`) vs 0 / STN_UNDEF (a
    /// `RelocationTarget::Absolute`, the addend-only path).
    pub defined_symbol: bool,
    /// The site's initial bytes, i.e. the implicit addend `SHT_REL` stores in
    /// the field. Its length is the slot length.
    pub slot_init: Vec<u8>,
}

/// Where [`build_rel_elf`] seats the relocation site, and what covers it.
pub(crate) struct RelPlacement {
    /// `sh_addr` / `p_vaddr` of the `.data.rel.ro` slot, which is also the
    /// `r_offset` the relocation names.
    pub slot_addr: u64,
    /// A second PT_LOAD spanning the slot, with its own file bytes: two
    /// loaded regions then cover the site.
    pub outer_load: bool,
    /// Map the slot PF_R | PF_X rather than PF_R | PF_W, putting it in the
    /// instruction-fetch image.
    pub slot_exec: bool,
}

impl Default for RelPlacement {
    fn default() -> Self {
        Self {
            slot_addr: 0x2000,
            outer_load: false,
            slot_exec: false,
        }
    }
}

pub(crate) fn build_mips32be_rel32_elf() -> RelFixture {
    build_mips32be_rel32_elf_with(/* defined_symbol */ true)
}

pub(crate) fn build_mips32be_rel32_elf_with(defined_symbol: bool) -> RelFixture {
    build_rel_elf(RelOpts {
        endian: Endianness::Big,
        is_64: false,
        e_machine: elf::EM_MIPS,
        r_type: elf::R_MIPS_REL32,
        defined_symbol,
        slot_init: vec![0u8; 4],
    })
}

/// Layout:
/// - `.text`: one dummy instruction word at `0x1000`, where `func` is defined.
/// - `.data.rel.ro`: the relocation site at `slot_addr`.
/// - `.dynsym`: null symbol plus the defined `func`.
/// - `.dynstr`: string table for `.dynsym`.
/// - `.rel.dyn`: one `Elf32_Rel` / `Elf64_Rel` at `slot_addr`,
///   `sh_link = .dynsym`.
///
/// `object::dynamic_relocations()` only iterates `SHT_REL` sections whose
/// `sh_link` is the `SHT_DYNSYM` section, so that wiring is what makes the
/// reloc visible at all.
pub(crate) fn build_rel_elf(opts: RelOpts) -> RelFixture {
    build_rel_elf_placed(opts, RelPlacement::default())
}

pub(crate) fn build_rel_elf_placed(opts: RelOpts, place: RelPlacement) -> RelFixture {
    let RelOpts {
        endian,
        is_64,
        e_machine,
        r_type,
        defined_symbol,
        slot_init,
    } = opts;
    let be = matches!(endian, Endianness::Big);
    let u16b = |v: u16| if be { v.to_be_bytes() } else { v.to_le_bytes() };
    let u32b = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
    let u64b = |v: u64| if be { v.to_be_bytes() } else { v.to_le_bytes() };

    let sym_addr: u64 = 0x1000; // `.text` / `func`
    let slot_addr: u64 = place.slot_addr; // `.data.rel.ro` slot

    let text = vec![0u8, 0, 0, 0]; // one dummy instruction word
    let slot = slot_init;
    // Distinct filler, so a read served by the outer mapping is recognisable.
    let outer = place
        .outer_load
        .then(|| vec![0xeeu8; OUTER_LEAD as usize + slot.len() + 4]);

    // `.dynstr`: index 0 is the empty string; "func" follows.
    let mut dynstr = vec![0u8];
    let func_name_off = dynstr.len() as u32;
    dynstr.extend_from_slice(b"func\0");

    // Symbol 0 is the reserved null entry, symbol 1 the defined `func`.
    // Elf32_Sym is 16 bytes: name(4) value(4) size(4) info(1) other(1)
    // shndx(2); Elf64_Sym is 24: name(4) info(1) other(1) shndx(2) value(8)
    // size(8).
    let sym_index: u32 = 1;
    let text_shndx: u16 = 1; // `.text` is section index 1 (see below)
    let sym_entsize = if is_64 { 24 } else { 16 };
    let st_info = (elf::STB_GLOBAL << 4) | elf::STT_FUNC;
    let mut dynsym = vec![0u8; sym_entsize]; // null symbol
    dynsym.extend_from_slice(&u32b(func_name_off));
    if is_64 {
        dynsym.push(st_info);
        dynsym.push(0); // st_other
        dynsym.extend_from_slice(&u16b(text_shndx));
        dynsym.extend_from_slice(&u64b(sym_addr));
        dynsym.extend_from_slice(&u64b(0)); // st_size
    } else {
        dynsym.extend_from_slice(&u32b(sym_addr as u32));
        dynsym.extend_from_slice(&u32b(0)); // st_size
        dynsym.push(st_info);
        dynsym.push(0); // st_other
        dynsym.extend_from_slice(&u16b(text_shndx));
    }

    // One Elf32_Rel (8 bytes) or Elf64_Rel (16), where r_info is
    // `(sym << 8) | type` for ELF32 and `(sym << 32) | type` for ELF64.
    let r_sym = if defined_symbol { sym_index } else { 0 };
    let rel_entsize = if is_64 { 16 } else { 8 };
    let mut reldyn = Vec::with_capacity(rel_entsize);
    if is_64 && e_machine == elf::EM_MIPS {
        // MIPS64 lays `r_info` out as an `r_sym` word in target endianness
        // followed by four single bytes, so the type half is NOT byte-swapped
        // on a little-endian target.
        reldyn.extend_from_slice(&u64b(slot_addr));
        reldyn.extend_from_slice(&u32b(r_sym));
        reldyn.push(((r_type >> 24) & 0xff) as u8); // r_ssym
        reldyn.push(((r_type >> 16) & 0xff) as u8); // r_type3
        reldyn.push(((r_type >> 8) & 0xff) as u8); // r_type2
        reldyn.push((r_type & 0xff) as u8); // r_type
    } else if is_64 {
        reldyn.extend_from_slice(&u64b(slot_addr));
        reldyn.extend_from_slice(&u64b((u64::from(r_sym) << 32) | u64::from(r_type)));
    } else {
        reldyn.extend_from_slice(&u32b(slot_addr as u32));
        reldyn.extend_from_slice(&u32b((r_sym << 8) | (r_type & 0xff)));
    }

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, is_64, &mut buf);

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
        w.reserve_program_headers(2 + u32::from(outer.is_some()));
        // Align 1 throughout so reserved offsets match where `w.write` lands,
        // with no implicit padding the plain `write` calls below wouldn't
        // reproduce. File-offset alignment doesn't affect the in-memory
        // addresses the applier patches.
        let text_off = w.reserve(text.len(), 1);
        let slot_off = w.reserve(slot.len(), 1);
        let dynsym_off = w.reserve(dynsym.len(), 1);
        let dynstr_off = w.reserve(dynstr.len(), 1);
        let reldyn_off = w.reserve(reldyn.len(), 1);
        let outer_off = outer.as_ref().map(|o| w.reserve(o.len(), 1));
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // ET_DYN, the shape `apply_elf_relocations` targets.
        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_DYN,
            e_machine,
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
            p_flags: elf::PF_R
                | if place.slot_exec {
                    elf::PF_X
                } else {
                    elf::PF_W
                },
            p_offset: slot_off as u64,
            p_vaddr: slot_addr,
            p_paddr: slot_addr,
            p_filesz: slot.len() as u64,
            p_memsz: slot.len() as u64,
            p_align: 1,
        });
        if let (Some(outer), Some(off)) = (outer.as_ref(), outer_off) {
            w.write_program_header(&ProgramHeader {
                p_type: elf::PT_LOAD,
                p_flags: elf::PF_R | elf::PF_W,
                p_offset: off as u64,
                p_vaddr: slot_addr - OUTER_LEAD,
                p_paddr: slot_addr - OUTER_LEAD,
                p_filesz: outer.len() as u64,
                p_memsz: outer.len() as u64,
                p_align: 1,
            });
        }

        w.write(&text);
        w.write(&slot);
        w.write(&dynsym);
        w.write(&dynstr);
        w.write(&reldyn);
        if let Some(outer) = outer.as_ref() {
            w.write(outer);
        }
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
            sh_entsize: sym_entsize as u64,
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
            sh_entsize: rel_entsize as u64,
        });
        w.write_shstrtab_section_header();
    }

    RelFixture {
        bytes: buf,
        slot_addr,
        sym_addr,
    }
}

/// An x86-64 ET_REL whose `.data` and `.text.f` both sit at VMA 0, with a
/// `.rela.data` entry writing an absolute symbol into `.data + 0`.
///
/// `.data` comes first in section order, so the code-and-readonly load
/// materialises `.text.f` at VMA 0 while an all-allocatable dedup picks
/// `.data`: siting the `.rela.data` entry through the wrong winner overwrites
/// the function body. Both are eight bytes, as `gcc -c -ffunction-sections`
/// emits them.
pub(crate) struct EtRelCollisionFixture {
    pub bytes: Vec<u8>,
    /// The initialised contents of `.data`, at VMA 0.
    pub data_bytes: Vec<u8>,
    /// The body of `.text.f`, also at VMA 0.
    pub text_bytes: Vec<u8>,
    /// `st_value` of the `SHN_ABS` symbol the relocation targets.
    pub sym_value: u64,
}

pub(crate) fn build_et_rel_vma_collision_elf() -> EtRelCollisionFixture {
    build_et_rel_vma_collision_elf_with(vec![0x90u8; 8])
}

/// As [`build_et_rel_vma_collision_elf`], with `.text.f`'s body chosen by the
/// caller. Passing eight zero bytes makes it byte-identical to `.data`, which
/// is what a zero-initialised or same-length pair looks like and what no
/// content comparison can tell apart.
pub(crate) fn build_et_rel_vma_collision_elf_with(text_bytes: Vec<u8>) -> EtRelCollisionFixture {
    build_et_rel_vma_collision_elf_full(text_bytes, elf::SHN_ABS, 0xdead_beef)
}

/// As [`build_et_rel_vma_collision_elf_with`], with the relocation's target
/// symbol placed in `sym_shndx` carrying `sym_value`. `SHN_COMMON` stores the
/// symbol's ALIGNMENT in `st_value`, not an address.
pub(crate) fn build_et_rel_vma_collision_elf_full(
    text_bytes: Vec<u8>,
    sym_shndx: u16,
    sym_value: u64,
) -> EtRelCollisionFixture {
    build_et_rel_vma_collision_elf_sited(text_bytes, sym_shndx, sym_value, 0, elf::R_X86_64_64)
}

/// As [`build_et_rel_vma_collision_elf_full`], with the relocation's `r_offset`
/// within `.data` and its type chosen by the caller. An `r_offset` at `.data`'s
/// own end sites the field in whatever follows.
pub(crate) fn build_et_rel_vma_collision_elf_sited(
    text_bytes: Vec<u8>,
    sym_shndx: u16,
    sym_value: u64,
    r_offset: u64,
    r_type: u32,
) -> EtRelCollisionFixture {
    let endian = Endianness::Little;
    let data_bytes = vec![0u8; 8];

    let mut strtab = vec![0u8];
    let target_name_off = strtab.len() as u32;
    strtab.extend_from_slice(b"target\0");

    // Elf64_Sym: name(4) info(1) other(1) shndx(2) value(8) size(8). Symbol 0
    // is the null entry; symbol 1 is `target`, SHN_ABS so its address is
    // `st_value` regardless of section placement.
    let mut symtab = vec![0u8; 24];
    symtab.extend_from_slice(&target_name_off.to_le_bytes());
    symtab.push((elf::STB_GLOBAL << 4) | elf::STT_OBJECT);
    symtab.push(0);
    symtab.extend_from_slice(&sym_shndx.to_le_bytes());
    symtab.extend_from_slice(&sym_value.to_le_bytes());
    symtab.extend_from_slice(&0u64.to_le_bytes());

    // One Elf64_Rela at `.data + r_offset`: r_offset(8) r_info(8) r_addend(8).
    let mut rela = Vec::with_capacity(24);
    rela.extend_from_slice(&r_offset.to_le_bytes());
    rela.extend_from_slice(&((1u64 << 32) | u64::from(r_type)).to_le_bytes());
    rela.extend_from_slice(&0i64.to_le_bytes());

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, /* is_64 */ true, &mut buf);

        // 0 = null, 1 = .data, 2 = .text.f, 3 = .rela.data, 4 = .symtab,
        // 5 = .strtab, 6 = .shstrtab.
        let _null = w.reserve_null_section_index();
        let data_name = w.add_section_name(b".data");
        let data_idx = w.reserve_section_index();
        let text_name = w.add_section_name(b".text.f");
        let _text_idx = w.reserve_section_index();
        let rela_name = w.add_section_name(b".rela.data");
        let _rela_idx = w.reserve_section_index();
        let symtab_name = w.add_section_name(b".symtab");
        let symtab_idx = w.reserve_section_index();
        let strtab_name = w.add_section_name(b".strtab");
        let strtab_idx = w.reserve_section_index();
        let _shstr = w.reserve_shstrtab_section_index();

        assert_eq!(data_idx.0, 1);
        assert_eq!(symtab_idx.0, 4);

        w.reserve_file_header();
        let data_off = w.reserve(data_bytes.len(), 1);
        let text_off = w.reserve(text_bytes.len(), 1);
        let rela_off = w.reserve(rela.len(), 1);
        let symtab_off = w.reserve(symtab.len(), 1);
        let strtab_off = w.reserve(strtab.len(), 1);
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");

        w.write(&data_bytes);
        w.write(&text_bytes);
        w.write(&rela);
        w.write(&symtab);
        w.write(&strtab);
        w.write_shstrtab();

        w.write_null_section_header();
        w.write_section_header(&SectionHeader {
            name: Some(data_name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_WRITE),
            sh_addr: 0,
            sh_offset: data_off as u64,
            sh_size: data_bytes.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_section_header(&SectionHeader {
            name: Some(text_name),
            sh_type: elf::SHT_PROGBITS,
            sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
            sh_addr: 0,
            sh_offset: text_off as u64,
            sh_size: text_bytes.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        // `sh_info` names the section the entries apply to, which is how
        // `ObjectSection::relocations()` attaches them to `.data`.
        w.write_section_header(&SectionHeader {
            name: Some(rela_name),
            sh_type: elf::SHT_RELA,
            sh_flags: 0,
            sh_addr: 0,
            sh_offset: rela_off as u64,
            sh_size: rela.len() as u64,
            sh_link: symtab_idx.0,
            sh_info: data_idx.0,
            sh_addralign: 8,
            sh_entsize: 24,
        });
        w.write_section_header(&SectionHeader {
            name: Some(symtab_name),
            sh_type: elf::SHT_SYMTAB,
            sh_flags: 0,
            sh_addr: 0,
            sh_offset: symtab_off as u64,
            sh_size: symtab.len() as u64,
            sh_link: strtab_idx.0,
            sh_info: 1,
            sh_addralign: 8,
            sh_entsize: 24,
        });
        w.write_section_header(&SectionHeader {
            name: Some(strtab_name),
            sh_type: elf::SHT_STRTAB,
            sh_flags: 0,
            sh_addr: 0,
            sh_offset: strtab_off as u64,
            sh_size: strtab.len() as u64,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 1,
            sh_entsize: 0,
        });
        w.write_shstrtab_section_header();
    }

    EtRelCollisionFixture {
        bytes: buf,
        data_bytes,
        text_bytes,
        sym_value,
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
    /// `SHF_TLS`: allocatable, but addressed as an offset into the per-thread
    /// block rather than into the flat address space.
    pub tls: bool,
    /// `sh_size` to write in place of `data.len()`. SHT_NOBITS only, which has
    /// no file bytes to bound it.
    pub sh_size: Option<u64>,
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
            tls: false,
            sh_size: None,
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
            tls: false,
            sh_size: None,
        }
    }
    /// The firmware / `ld -N` shape: allocatable, executable AND writable.
    pub(crate) fn rwx(addr: u64, data: Vec<u8>) -> Self {
        Self {
            name: b".text",
            addr,
            data,
            exec: true,
            writable: true,
            nobits: false,
            tls: false,
            sh_size: None,
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
            tls: false,
            sh_size: None,
        }
    }
    /// `.tdata`: allocatable, writable and `SHF_TLS`.
    pub(crate) fn tdata(addr: u64, data: Vec<u8>) -> Self {
        Self {
            name: b".tdata",
            addr,
            data,
            exec: false,
            writable: true,
            nobits: false,
            tls: true,
            sh_size: None,
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
            tls: false,
            sh_size: None,
        }
    }

    /// A `.bss` declaring `sh_size` outright. SHT_NOBITS has no file bytes, so
    /// nothing rejects a size no allocation could ever back.
    pub(crate) fn bss_declaring(addr: u64, sh_size: u64) -> Self {
        Self {
            name: b".bss",
            addr,
            data: Vec::new(),
            exec: false,
            writable: true,
            nobits: true,
            tls: false,
            sh_size: Some(sh_size),
        }
    }
}

/// An x86-64 ET_REL with the given sections, in order, each at its `addr`.
///
/// ET_REL is the right `e_type` for a section-only fixture: ET_EXEC / ET_DYN
/// dispatch to PT_LOAD program headers, so only ET_REL routes the loader down
/// the section walk under test.
///
/// A `nobits` section contributes no file bytes but still gets a header with
/// the right `sh_size` and `sh_type`, which is how `object` models `.bss`.
pub(crate) fn build_elf_with_sections(sections: &[SectionSpec]) -> Vec<u8> {
    build_sections_elf(sections, &[], Endianness::Little, true, elf::EM_X86_64)
}

/// [`build_elf_with_sections`] plus a `.symtab` / `.strtab` pair defining
/// `symbols`. ET_REL, so each `st_value` is an offset into its section.
pub(crate) fn build_elf_with_sections_and_symbols(
    sections: &[SectionSpec],
    symbols: &[SymbolSpec],
) -> Vec<u8> {
    build_sections_elf(sections, symbols, Endianness::Little, true, elf::EM_X86_64)
}

#[derive(Clone, Debug)]
pub(crate) struct SymbolSpec {
    pub name: &'static [u8],
    /// Index into the section list, i.e. `st_shndx - 1`.
    pub section: usize,
    /// `st_value`.
    pub value: u64,
    pub size: u64,
}

fn build_sections_elf(
    sections: &[SectionSpec],
    symbols: &[SymbolSpec],
    endian: Endianness,
    is_64: bool,
    e_machine: u16,
) -> Vec<u8> {
    // Elf64_Sym little-endian is the only symbol encoding written here.
    assert!(
        symbols.is_empty() || (is_64 && endian == Endianness::Little),
        "symbol emission is 64-bit little-endian only"
    );
    let mut strtab = vec![0u8];
    let mut symtab = vec![0u8; 24];
    for sym in symbols {
        let name_off = strtab.len() as u32;
        strtab.extend_from_slice(sym.name);
        strtab.push(0);
        symtab.extend_from_slice(&name_off.to_le_bytes());
        symtab.push((elf::STB_GLOBAL << 4) | elf::STT_OBJECT);
        symtab.push(0);
        symtab.extend_from_slice(&((sym.section as u16 + 1).to_le_bytes()));
        symtab.extend_from_slice(&sym.value.to_le_bytes());
        symtab.extend_from_slice(&sym.size.to_le_bytes());
    }

    let mut buf = Vec::new();
    {
        let mut w = Writer::new(endian, is_64, &mut buf);

        let _null_idx = w.reserve_null_section_index();

        let mut name_ids = Vec::with_capacity(sections.len());
        for spec in sections {
            name_ids.push(w.add_section_name(spec.name));
            w.reserve_section_index();
        }
        let symtab_ids = (!symbols.is_empty()).then(|| {
            let symtab_name = w.add_section_name(b".symtab");
            let symtab_idx = w.reserve_section_index();
            let strtab_name = w.add_section_name(b".strtab");
            let strtab_idx = w.reserve_section_index();
            (symtab_name, symtab_idx, strtab_name, strtab_idx)
        });
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
        let symtab_offsets = symtab_ids.map(|_| {
            (
                w.reserve(symtab.len(), 8) as u64,
                w.reserve(strtab.len(), 1) as u64,
            )
        });
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
        if symtab_offsets.is_some() {
            w.write_align(8);
            w.write(&symtab);
            w.write(&strtab);
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
            if spec.tls {
                sh_flags |= u64::from(elf::SHF_TLS);
            }
            let sh_type = if spec.nobits {
                elf::SHT_NOBITS
            } else {
                elf::SHT_PROGBITS
            };
            let sh_size = spec.sh_size.unwrap_or(spec.data.len() as u64);
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

        if let (Some((symtab_name, _, strtab_name, strtab_idx)), Some((symtab_off, strtab_off))) =
            (symtab_ids, symtab_offsets)
        {
            // `sh_info` is the first non-local symbol index; every symbol here
            // is STB_GLOBAL, so only the null entry precedes them.
            w.write_section_header(&SectionHeader {
                name: Some(symtab_name),
                sh_type: elf::SHT_SYMTAB,
                sh_flags: 0,
                sh_addr: 0,
                sh_offset: symtab_off,
                sh_size: symtab.len() as u64,
                sh_link: strtab_idx.0,
                sh_info: 1,
                sh_addralign: 8,
                sh_entsize: 24,
            });
            w.write_section_header(&SectionHeader {
                name: Some(strtab_name),
                sh_type: elf::SHT_STRTAB,
                sh_flags: 0,
                sh_addr: 0,
                sh_offset: strtab_off,
                sh_size: strtab.len() as u64,
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

/// An x86-64 ET_REL whose `count` allocatable section headers all name ONE
/// `blob_len`-byte file range, at addresses `blob_len` apart.
///
/// Section dedup is on the loaded address, so every header is its own region
/// and the copying loader materialises `blob_len` bytes per header while the
/// file grows by none.
pub(crate) fn build_shared_file_range_elf(count: usize, blob_len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = Writer::new(Endianness::Little, true, &mut buf);
        let _null = w.reserve_null_section_index();
        let names: Vec<_> = (0..count)
            .map(|_| {
                let name = w.add_section_name(b".text");
                w.reserve_section_index();
                name
            })
            .collect();
        let _shstrtab = w.reserve_shstrtab_section_index();

        w.reserve_file_header();
        let blob_off = w.reserve(blob_len, 1) as u64;
        w.reserve_shstrtab();
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_SYSV,
            abi_version: 0,
            e_type: elf::ET_REL,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: 0,
        })
        .expect("write file header");
        w.write(&vec![0x90u8; blob_len]);
        w.write_shstrtab();

        w.write_null_section_header();
        for (i, name) in names.iter().enumerate() {
            w.write_section_header(&SectionHeader {
                name: Some(*name),
                sh_type: elf::SHT_PROGBITS,
                sh_flags: u64::from(elf::SHF_ALLOC | elf::SHF_EXECINSTR),
                sh_addr: (i as u64 + 1) * blob_len as u64,
                sh_offset: blob_off,
                sh_size: blob_len as u64,
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
/// A mips64 ET_DYN with one `SHT_REL` `R_MIPS_REL32 | R_MIPS_64` slot, in the
/// caller's endianness. mips64el's `.rel.dyn` is the transposed-`r_info` case.
pub(crate) fn build_mips64_rel32_elf(endian: Endianness) -> RelFixture {
    build_rel_elf(RelOpts {
        endian,
        is_64: true,
        e_machine: elf::EM_MIPS,
        // r_ssym=0 | r_type3=0 | r_type2=R_MIPS_64 | r_type=R_MIPS_REL32
        r_type: (elf::R_MIPS_64 << 8) | elf::R_MIPS_REL32,
        defined_symbol: true,
        slot_init: vec![0u8; 8],
    })
}

/// A mips64el ET_DYN whose one `SHT_REL` entry carries a relocation type
/// nothing here handles (`R_MIPS_COPY`) and a symbol index that collides with
/// `R_MIPS_16`.
///
/// `object` reads `Elf64_Rel::r_info` as a single little-endian `u64`, so the
/// `r_type` it reports is the real `r_sym`, and the `kind` / `size` it derived
/// from that describe a 16-bit absolute relocation.
pub(crate) fn build_mips64el_transposed_kind_elf() -> RelFixture {
    build_rel_elf(RelOpts {
        endian: Endianness::Little,
        is_64: true,
        e_machine: elf::EM_MIPS,
        r_type: elf::R_MIPS_COPY,
        defined_symbol: true,
        slot_init: vec![0u8; 8],
    })
}
