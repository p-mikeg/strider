//! ELF-backed implementation of [`crate::MemRegion`]s and the
//! [`rsleigh::MemReader`] trait.
//!
//! This module is the ELF-specific half of the `strider-reader` crate. The generic
//! region-lookup machinery (`MemRegion`, `MemRegionsLookupTable`) lives in
//! [`crate`] so other backends (raw blobs, PE, Mach-O, …) can reuse it.
//!
//! # Submodules
//!
//! - [`sections`] — ELF segment / section loaders that produce
//!   [`MemRegion`](crate::MemRegion) sets from an [`object::File`].
//!   The auto-dispatching entry points are kind-dispatched on
//!   `obj.kind()`: ET_EXEC / ET_DYN walk PT_LOAD segments (program
//!   headers), ET_REL walks sections with first-wins VMA dedup.  The
//!   narrower preset [`elf_get_loadable_regions`] is used by
//!   [`ElfFileMemReader`]; the broader
//!   [`elf_get_loadable_regions_including_writable`] is used by
//!   [`apply_elf_relocations_autoload`] so dynamic relocs targeting
//!   writable runtime data (`.got.plt` / `.data.rel.ro`) have
//!   something to patch.  [`elf_get_loadable_regions_sections_only`]
//!   (+ its `_including_writable` sibling) force the section-walk
//!   strategy even for a linked ET_EXEC/ET_DYN binary — used by
//!   `strider.lift.load_elf(path, from_segments=False)`.
//! - [`reader`] — [`ElfFileMemReader`], the
//!   [`rsleigh::MemReader`] + [`crate::ReadOnlyMemory`] impl that owns
//!   its regions.
//! - [`relocations`] — the relocation applier family
//!   ([`apply_elf_relocations`], [`apply_elf_relocations_autoload`],
//!   and the per-arch `R_*_RELATIVE` / `R_*_GLOB_DAT` /
//!   `R_*_JUMP_SLOT` tables).  ET_DYN uses the dynamic-relocations
//!   table; ET_REL uses per-section relocations.
//! - [`load`] — the top-level convenience entries
//!   ([`load_elf`] for `'static`-lifetime ELF parsing,
//!   [`elf_load_with_relocations`] for an all-in-one regions + relocs
//!   load).

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
