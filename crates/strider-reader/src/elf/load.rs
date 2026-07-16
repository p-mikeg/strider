//! Top-level convenience entries:
//!
//! - [`load_elf`] — read + parse an ELF from disk into an [`OwnedElf`]
//!   that owns its backing bytes and frees them on drop (no leak).
//! - [`elf_load_with_relocations`] — load every allocatable file-backed
//!   section and apply dynamic relocations in one call, returning the
//!   patched regions.

use anyhow::Context as _;

use crate::{MemRegion, Result};

use super::relocations::{apply_elf_relocations, apply_elf_relocations_autoload};
use super::sections::{elf_get_loadable_regions, elf_get_loadable_regions_including_writable};

/// Convenience: load every code + read-only + writable-allocatable
/// mapping (via [`elf_get_loadable_regions_including_writable`])
/// and apply relocations to the resulting regions in-place.
///
/// Use this when you want analysis-grade fidelity for an ET_DYN
/// binary: code, rodata, and writable-but-relocated data
/// (`.data.rel.ro`, `.got`) all land in the returned regions with
/// every applicable relocation patched in.
///
/// # Errors
///
/// Propagates any error from the inner helpers; relocation
/// resolution itself only errors on a malformed ELF.
pub fn elf_load_with_relocations(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    let mut regions = elf_get_loadable_regions_including_writable(obj)?;
    // Use the autoload variant so this bundled path is consistent with
    // the standalone `add_region_from_elf(path)` + `apply_elf_relocations(path)`
    // sequence used from Python.  In practice the upfront
    // `elf_get_loadable_regions_including_writable` already covers
    // every relocation-targeted section, so the autoload step is a
    // no-op; the symmetry just guards against future ELF shapes that
    // emit relocation sites against sections the upfront loader misses.
    apply_elf_relocations_autoload(&mut regions, obj)?;
    Ok(regions)
}

/// Convenience: load only the code + read-only mappings (via
/// [`elf_get_loadable_regions`], i.e. `.text` / `.rodata` / `.plt` /
/// `.eh_frame` — writable sections EXCLUDED) and apply relocations to
/// just those regions.
///
/// This is the **runtime-immutable** image: the right source for the
/// optimizer's `LoadReadOnly` rom, which folds a constant-address load
/// to the resolved bytes WITHOUT consulting the memory chain and so
/// trusts every resolvable address to be runtime-immutable.  Writable
/// sections (`.data`, `.got`, `.data.rel.ro`) are deliberately absent:
/// a store-then-reload of such a global must NOT fold to its
/// file-initial value.
///
/// Relocations are applied with the non-autoload
/// [`apply_elf_relocations`] so the writable sections are not pulled
/// back in: relocations whose site lands in an absent (writable) region
/// are simply not applied.  Relocations into `.rodata` (e.g. PC-relative
/// jump tables) ARE applied.
///
/// Capability tradeoff: RELRO sections (`.data.rel.ro`, `.got`) are
/// runtime-immutable only post-RELRO yet carry SHF_WRITE statically, so
/// they are excluded here — some GOT-based folds are conservatively
/// lost.  This is sound (soundness over capability).
///
/// # Errors
///
/// Propagates any error from the inner helpers; relocation resolution
/// itself only errors on a malformed ELF.
pub fn elf_load_readonly_with_relocations(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    let mut regions = elf_get_loadable_regions(obj)?;
    apply_elf_relocations(&mut regions, obj)?;
    Ok(regions)
}

/// Loads and parses an ELF file from `path`, returning a `'static` reference.
///
/// On success, the file bytes are read into a `Box<[u8]>` that is then
/// intentionally **leaked** so the returned `object::File<'static>` remains
/// valid for the lifetime of the process. This is suitable for tests and
/// short-lived CLI tools where the cost of a one-time leak is acceptable.
///
/// On error, no bytes are leaked: the file is read, validated by an
/// in-place parse, and only on parse success are the bytes promoted to
/// `'static` (a second parse — guaranteed to succeed since the bytes are
/// identical — produces the returned `object::File`).
///
/// Callers that only need an [`super::reader::ElfFileMemReader`] should
/// prefer [`super::reader::ElfFileMemReader::from_path`], which does
/// not leak.
///
/// # Errors
///
/// Returns an error if the file cannot be read from disk or if
/// the bytes fail to parse as a valid ELF.
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<OwnedElf> {
    let data = std::fs::read(path).context("failed to read file")?;
    OwnedElf::parse(data)
}

/// An owned ELF: the backing file bytes, freed on drop.
///
/// This replaces the historical `Box::leak` loader, which fabricated a
/// `'static` `object::File` by leaking the whole file — fine for a
/// short-lived CLI, but an unbounded per-call leak in a long-lived process
/// (the strider-py `load_elf` / `_LoadedElf` path).
///
/// Only the bytes are stored; the parsed [`object::File`] is a borrowing view
/// with no owned variant, so keeping one alongside the bytes it reads would be
/// a self-referential struct.  Instead [`file`](Self::file) re-parses on each
/// call — `object::File::parse` is a lazy, header-only parse (it records
/// section/segment table offsets rather than copying the file), and callers
/// resolve `file()` once per load in cold setup code, so the cost is
/// negligible.  This keeps the type safe (no `unsafe`, no self-reference).
pub struct OwnedElf {
    backing: Box<[u8]>,
}

impl std::fmt::Debug for OwnedElf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never dump the backing bytes (an ELF can be hundreds of MB).
        f.debug_struct("OwnedElf")
            .field("backing_len", &self.backing.len())
            .finish_non_exhaustive()
    }
}

impl OwnedElf {
    /// Parse owned `bytes` into an `OwnedElf`, validating them as a well-formed
    /// ELF up front.  On a parse error the bytes are dropped normally (no
    /// allocation retained).
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` do not parse as a valid ELF.
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        // Validate now so `file()` can re-parse the identical bytes infallibly.
        object::File::parse(&bytes[..]).context("failed to parse ELF")?;
        Ok(Self {
            backing: bytes.into_boxed_slice(),
        })
    }

    /// The parsed ELF, borrowed for no longer than `self` — so views it
    /// yields (symbols, sections, …) cannot escape past the backing bytes.
    ///
    /// Re-parses the backing bytes each call; see the type docs for why that
    /// is cheap.  Infallible: the bytes were validated in [`parse`](Self::parse)
    /// and are immutable, so this re-parse of identical bytes cannot fail.
    #[inline]
    pub fn file(&self) -> object::File<'_> {
        object::File::parse(&self.backing[..])
            .expect("bytes were validated as ELF at construction")
    }
}
