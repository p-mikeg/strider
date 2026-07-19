//! The ELF backend: [`crate::MemRegion`] sets built from an [`object::File`],
//! plus the reader that serves them.
//!
//! [`sections`] loaders are kind-dispatched on `obj.kind()`: ET_EXEC / ET_DYN
//! walk PT_LOAD segments, ET_REL walks sections with first-wins VMA dedup. The
//! `_sections_only` presets force the section walk regardless of kind.
//! [`relocations`] patches sites in place: ET_DYN via the dynamic-relocations
//! table, ET_REL via per-section tables.

pub mod load;
pub mod reader;
pub mod relocations;
pub mod sections;

pub use load::{OwnedElf, elf_load_readonly_with_relocations, elf_load_with_relocations, load_elf};
pub use reader::ElfFileMemReader;
pub use relocations::{apply_elf_relocations, apply_elf_relocations_autoload};
pub use sections::{
    elf_get_loadable_regions, elf_get_loadable_regions_including_writable,
    elf_get_loadable_regions_sections_only,
    elf_get_loadable_regions_sections_only_including_writable,
};
