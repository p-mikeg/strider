use object::{Architecture as A, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable};

use crate::MemRegion;
use crate::elf::sections::AddressSet;

use super::Ctx;
use super::apply::{is_ifunc, read_field};

/// Records the object an `R_*_COPY` at `addr` fills: `size` bytes, or with
/// no size known, up to the end of every region holding `addr`.
pub(super) fn note_copy(ctx: &mut Ctx<'_, '_>, addr: u64, size: Option<u64>, r_type: u32) {
    let end = match size {
        Some(size) => addr.saturating_add(size),
        None => ctx
            .regions
            .iter()
            .filter(|region| region.contains(addr))
            .map(MemRegion::end_addr)
            .max()
            .unwrap_or(addr),
    };
    ctx.copies.push((addr, end, r_type));
}

/// Holes over every byte of every `R_*_COPY` object a region holds: the
/// loader copies the defining library's bytes there.
pub(super) fn hole_copies(ctx: &mut Ctx<'_, '_>) {
    let copies = std::mem::take(&mut ctx.copies);
    let Some(&(_, _, r_type)) = copies.first() else {
        return;
    };
    let objects = AddressSet::merged(copies.iter().map(|&(lo, hi, _)| (lo, hi)).collect());
    let mut held = Vec::new();
    for region in ctx.regions {
        let (start, end) = (region.start_addr(), region.end_addr());
        for &(lo, hi) in objects.overlapping(start, end) {
            held.push((lo.max(start), hi.min(end)));
        }
    }
    for &(lo, hi) in AddressSet::merged(held).as_slice() {
        let holder = ctx.holding(lo, 1);
        ctx.unmodelled(holder, lo, (hi - lo) as usize, r_type);
    }
}

/// Holes over the MIPS primary GOT entries the loader writes with no
/// relocation record (glibc `elf_machine_runtime_setup`,
/// `elf_machine_got_rel`): the lazy resolver's word, the module pointer's
/// when the second word's top bit asks for one, and every global entry
/// (`DT_MIPS_GOTSYM .. DT_MIPS_SYMTABNO`) except one whose `.dynsym` symbol
/// the image defines, is no IFUNC, and already holds its `st_value`.
///
/// Local entries (`B + value`, `B` = 0) stay as filed. With no `SHT_DYNSYM`
/// header (`object` needs one) every global entry is a hole; with a GOT tag
/// missing, every `.got` word. Holes name `R_MIPS_GLOB_DAT`.
pub(super) fn mips_implicit_got(ctx: &mut Ctx<'_, '_>) {
    let obj = ctx.obj;
    if !matches!(obj.architecture(), A::Mips | A::Mips64)
        || !matches!(
            obj.kind(),
            object::ObjectKind::Executable | object::ObjectKind::Dynamic
        )
    {
        return;
    }
    let word: u64 = if obj.is_64() { 8 } else { 4 };
    let width = word as usize;
    let tags = match mips_got_tags(obj) {
        None => return,
        Some(Some(tags)) => tags,
        // A missing tag says nothing about which words the loader writes.
        Some(None) => {
            for sec in obj.sections().filter(|s| s.name() == Ok(".got")) {
                let base = ctx.layout.section_base(&sec);
                // To the end of its mapping, whatever `sh_size` claims.
                for i in 0..sec.size() / word {
                    let Some(addr) = i.checked_mul(word).and_then(|o| base.checked_add(o)) else {
                        break;
                    };
                    let Some(holder) = ctx.holding(addr, width) else {
                        break;
                    };
                    ctx.unmodelled(Some(holder), addr, width, object::elf::R_MIPS_GLOB_DAT);
                }
            }
            return;
        }
    };
    let slot = |i: u64| {
        i.checked_mul(word)
            .and_then(|off| tags.base.checked_add(off))
    };
    let hole = |ctx: &mut Ctx<'_, '_>, addr: u64| {
        let holder = ctx.holding(addr, width);
        ctx.unmodelled(holder, addr, width, object::elf::R_MIPS_GLOB_DAT);
    };

    if tags.local_gotno >= 1
        && let Some(addr) = slot(0)
    {
        hole(ctx, addr);
    }
    if tags.local_gotno >= 2
        && let Some(addr) = slot(1)
        && let Some(region) = ctx.holding(addr, width)
        && read_field(region, addr, width, ctx.endian_le) >> (word * 8 - 1) != 0
    {
        hole(ctx, addr);
    }

    // `.dynsym` says which entries the loader leaves as they are.
    let dynsym = obj.dynamic_symbol_table();
    let globals = tags.symtabno.saturating_sub(tags.gotsym);
    for i in 0..globals {
        let Some(addr) = tags.local_gotno.checked_add(i).and_then(slot) else {
            break;
        };
        // Nothing past the GOT's mapping is served.
        let Some(region) = ctx.holding(addr, width) else {
            break;
        };
        let held = read_field(region, addr, width, ctx.endian_le);
        let index = object::SymbolIndex((tags.gotsym + i) as usize);
        let served = dynsym
            .as_ref()
            .and_then(|table| table.symbol_by_index(index).ok())
            .is_some_and(|sym| {
                matches!(
                    sym.section(),
                    object::SymbolSection::Section(_) | object::SymbolSection::Absolute
                ) && !is_ifunc(&sym)
                    && held == sym.address()
            });
        if !served {
            hole(ctx, addr);
        }
    }
}

/// Holes over the two lazy-binding GOT header words the loader writes with
/// no relocation record: ARM's at `DT_PLTGOT + 4` / `+ 8`, secure-PLT PPC32's
/// at `DT_PPC_GOT + 4` / `+ 8`, when a `DT_JMPREL` is present (whether or not
/// `DF_BIND_NOW` is set). A hole names the arch's `JUMP_SLOT`.
pub(super) fn lazy_got_header(ctx: &mut Ctx<'_, '_>) {
    use object::elf;
    let obj = ctx.obj;
    let (base_tag, r_type) = match obj.architecture() {
        A::Arm => (elf::DT_PLTGOT, elf::R_ARM_JUMP_SLOT),
        A::PowerPc => (elf::DT_PPC_GOT, elf::R_PPC_JMP_SLOT),
        _ => return,
    };
    let Some(Some(tags)) = crate::elf::dynamic::dynamic_tags(obj) else {
        return;
    };
    let (Some(base), Some(_)) = (tags.get(base_tag), tags.get(elf::DT_JMPREL)) else {
        return;
    };
    for off in [4, 8] {
        let Some(addr) = base.checked_add(off) else {
            return;
        };
        let holder = ctx.holding(addr, 4);
        ctx.unmodelled(holder, addr, 4, r_type);
    }
}

/// The dynamic tags that place a MIPS primary GOT.
pub(super) struct MipsGotTags {
    /// `DT_PLTGOT`, the GOT's start.
    base: u64,
    /// `DT_MIPS_LOCAL_GOTNO`, the reserved and local entries before the
    /// global ones.
    local_gotno: u64,
    /// `DT_MIPS_GOTSYM`, the `.dynsym` index of the first global entry's
    /// symbol; `DT_MIPS_SYMTABNO` with no global entries.
    gotsym: u64,
    /// `DT_MIPS_SYMTABNO`, the `.dynsym` count.
    symtabno: u64,
}

/// [`MipsGotTags`] from the loader's dynamic array: `None` without a
/// `PT_DYNAMIC`, `Some(None)` when the array cannot be read or lacks any of
/// the four tags.
pub(super) fn mips_got_tags(obj: &object::File<'_>) -> Option<Option<MipsGotTags>> {
    let Some(tags) = crate::elf::dynamic::dynamic_tags(obj)? else {
        return Some(None);
    };
    let tag = |want: u32| tags.get(want);
    let (Some(base), Some(local_gotno), Some(gotsym), Some(symtabno)) = (
        tag(object::elf::DT_PLTGOT),
        tag(object::elf::DT_MIPS_LOCAL_GOTNO),
        tag(object::elf::DT_MIPS_GOTSYM),
        tag(object::elf::DT_MIPS_SYMTABNO),
    ) else {
        return Some(None);
    };
    Some(Some(MipsGotTags {
        base,
        local_gotno,
        gotsym,
        symtabno,
    }))
}
