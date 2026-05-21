//! ELF-backed implementation of [`crate::MemRegion`]s and the
//! [`rsleigh::MemReader`] trait.
//!
//! This module is the ELF-specific half of the `strider-reader` crate. The generic
//! region-lookup machinery (`MemRegion`, `MemRegionsLookupTable`) lives in
//! [`crate`] so other backends (raw blobs, PE, Mach-O, …) can reuse it.
//!
//! # Submodules
//!
//! - [`sections`] — ELF section / segment walkers that produce
//!   [`MemRegion`](crate::MemRegion) sets from an
//!   [`object::File`].  Includes the two presets
//!   [`elf_get_code_and_readonly_sections_as_mem_regions`] (used by
//!   [`ElfFileMemReader`]) and
//!   [`elf_get_allocatable_file_backed_sections_as_mem_regions`] (used
//!   by [`apply_elf_relocations_autoload`] so dynamic relocs targeting
//!   writable sections like `.got.plt` / `.data.rel.ro` have something
//!   to patch).
//! - [`reader`] — [`ElfFileMemReader`], the
//!   [`rsleigh::MemReader`] + [`crate::ReadOnlyMemory`] impl that owns
//!   its regions.
//! - [`relocations`] — the relocation applier family
//!   ([`apply_elf_relocations`], [`apply_elf_relocations_autoload`],
//!   [`RelocationStats`], and the per-arch `R_*_RELATIVE` /
//!   `R_*_GLOB_DAT` / `R_*_JUMP_SLOT` tables).
//! - [`load`] — the top-level convenience entries
//!   ([`load_elf`] for `'static`-lifetime ELF parsing,
//!   [`elf_load_with_relocations`] for an all-in-one regions + relocs
//!   load).

pub mod load;
pub mod reader;
pub mod relocations;
pub mod sections;

pub use load::{elf_load_with_relocations, load_elf};
pub use reader::ElfFileMemReader;
pub use relocations::{
    RelocationStats, apply_elf_relocations, apply_elf_relocations_autoload,
};
pub use sections::{
    elf_get_allocatable_file_backed_sections_as_mem_regions,
    elf_get_code_and_readonly_sections_as_mem_regions,
};
