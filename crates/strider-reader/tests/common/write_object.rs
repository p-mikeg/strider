//! `object::write` shorthands for hand-built relocatable objects.

use object::write::{Object, Relocation, SectionId, Symbol, SymbolId, SymbolSection};
use object::{RelocationFlags, SymbolFlags, SymbolKind, SymbolScope};

/// A global text symbol at `value` in `section`.
pub(crate) fn defined(
    obj: &mut Object<'_>,
    name: &str,
    section: SectionId,
    value: u64,
) -> SymbolId {
    obj.add_symbol(Symbol {
        name: name.as_bytes().to_vec(),
        value,
        size: 0,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Section(section),
        flags: SymbolFlags::None,
    })
}

/// A global symbol no section defines.
pub(crate) fn undefined(obj: &mut Object<'_>, name: &str) -> SymbolId {
    obj.add_symbol(Symbol {
        name: name.as_bytes().to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Unknown,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Undefined,
        flags: SymbolFlags::None,
    })
}

/// An ELF relocation of `r_type` at `offset` in `section`.
pub(crate) fn reloc(
    obj: &mut Object<'_>,
    section: SectionId,
    offset: u64,
    symbol: SymbolId,
    addend: i64,
    r_type: u32,
) {
    obj.add_relocation(
        section,
        Relocation {
            offset,
            symbol,
            addend,
            flags: RelocationFlags::Elf { r_type },
        },
    )
    .expect("relocation");
}
