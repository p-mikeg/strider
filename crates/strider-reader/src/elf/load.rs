//! Top-level convenience entries:
//!
//! - [`load_elf`] — read + parse an ELF from disk into a
//!   `'static`-lifetime [`object::File`] (intentionally leaks the
//!   backing bytes; suitable for tests and short-lived CLI tools).
//! - [`elf_load_with_relocations`] — load every allocatable file-backed
//!   section and apply dynamic relocations in one call, returning the
//!   patched regions and the [`super::relocations::RelocationStats`].

use anyhow::Context as _;

use crate::{MemRegion, Result};

use super::relocations::{RelocationStats, apply_elf_relocations, apply_elf_relocations_autoload};
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
pub fn elf_load_with_relocations(
    obj: &object::File<'_>,
) -> Result<(Vec<MemRegion>, RelocationStats)> {
    let mut regions = elf_get_loadable_regions_including_writable(obj)?;
    // Use the autoload variant so this bundled path is consistent with
    // the standalone `add_region_from_elf(path)` + `apply_elf_relocations(path)`
    // sequence used from Python.  In practice the upfront
    // `elf_get_loadable_regions_including_writable` already covers
    // every relocation-targeted section, so the autoload step is a
    // no-op; the symmetry just guards against future ELF shapes that
    // emit relocation sites against sections the upfront loader misses.
    let stats = apply_elf_relocations_autoload(&mut regions, obj)?;
    Ok((regions, stats))
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
/// are counted under [`RelocationStats::skipped_no_region`] and simply
/// not applied.  Relocations into `.rodata` (e.g. PC-relative jump
/// tables) ARE applied.
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
pub fn elf_load_readonly_with_relocations(
    obj: &object::File<'_>,
) -> Result<(Vec<MemRegion>, RelocationStats)> {
    let mut regions = elf_get_loadable_regions(obj)?;
    let stats = apply_elf_relocations(&mut regions, obj)?;
    Ok((regions, stats))
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
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<object::File<'static>> {
    let data = std::fs::read(path).context("failed to read file")?;
    // Validate the parse on a borrowed view BEFORE leaking. If the bytes
    // don't parse as ELF, we drop `data` normally instead of leaking it
    // onto the heap forever for nothing — the leak only pays for itself
    // when we actually return an `object::File<'static>`.
    object::File::parse(&data[..]).context("failed to parse ELF")?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    // Re-parse the (now `'static`) bytes.  The first parse above already
    // validated these exact bytes, so this parse is expected to succeed;
    // we still propagate via `?` rather than `expect`/`unwrap` (forbidden
    // in this crate) so any unforeseen non-determinism surfaces as a
    // normal `Err` instead of a panic.
    object::File::parse(leaked).context("failed to parse ELF")
}
