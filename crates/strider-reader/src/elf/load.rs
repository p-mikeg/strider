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

use super::relocations::{RelocationStats, apply_elf_relocations_autoload};
use super::sections::elf_get_allocatable_file_backed_sections_as_mem_regions;

/// Convenience: load every allocatable file-backed section (via
/// [`elf_get_allocatable_file_backed_sections_as_mem_regions`])
/// and apply dynamic relocations to the resulting regions in-place.
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
    let mut regions = elf_get_allocatable_file_backed_sections_as_mem_regions(obj)?;
    // Use the autoload variant so this bundled path is consistent with
    // the standalone `add_region_from_elf(path)` + `apply_elf_relocations(path)`
    // sequence used from Python.  In practice the upfront
    // `elf_get_allocatable_file_backed_sections_as_mem_regions` already
    // covers every relocation-targeted section, so the autoload step
    // is a no-op; the symmetry just guards against future ELF shapes
    // that emit relocation sites against sections the upfront loader
    // misses.
    let stats = apply_elf_relocations_autoload(&mut regions, obj)?;
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
    // Re-parse the (now `'static`) bytes. Identical bytes parse identically,
    // so this `?` cannot fail in practice; we still propagate via `?` to
    // avoid `expect`/`unwrap` (forbidden in this crate).
    object::File::parse(leaked).context("failed to parse ELF")
}
