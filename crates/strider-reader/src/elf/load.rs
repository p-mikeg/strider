use anyhow::Context as _;

use crate::{MemRegion, Result};

use super::relocations::{apply_elf_relocations, apply_elf_relocations_autoload};
use super::sections::{elf_get_loadable_regions, elf_get_loadable_regions_including_writable};

/// Loads code, rodata and writable-allocatable mappings, then relocates them in
/// place. Analysis-grade fidelity for an ET_DYN binary: `.data.rel.ro` / `.got`
/// land in the result with every applicable relocation patched in.
///
/// # Errors
///
/// Propagates the inner helpers' errors; relocation resolution itself only
/// fails on a malformed ELF.
pub fn elf_load_with_relocations(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    let mut regions = elf_get_loadable_regions_including_writable(obj)?;
    // The upfront include-writable load already covers every
    // relocation-targeted section, so autoload is a no-op here. Using it anyway
    // keeps this bundled path identical to the standalone load-then-relocate
    // sequence, and covers future ELF shapes with sites the loader misses.
    apply_elf_relocations_autoload(&mut regions, obj)?;
    Ok(regions)
}

/// Loads only the runtime-immutable image (`.text` / `.rodata` / `.plt` /
/// `.eh_frame`; writable sections excluded) and relocates it.
///
/// Writable sections are absent on purpose: a store-then-reload of such a
/// global must not fold to its file-initial value.
///
/// Uses the non-autoload applier so writable sections are not pulled back in;
/// relocations whose site lands in an absent region go unapplied, while
/// relocations into `.rodata` (PC-relative jump tables) do apply.
///
/// RELRO sections (`.data.rel.ro`, `.got`) carry SHF_WRITE statically even
/// though they are immutable post-RELRO, so they are excluded too.
///
/// # Errors
///
/// Propagates the inner helpers' errors; relocation resolution itself only
/// fails on a malformed ELF.
pub fn elf_load_readonly_with_relocations(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    let mut regions = elf_get_loadable_regions(obj)?;
    apply_elf_relocations(&mut regions, obj)?;
    Ok(regions)
}

/// # Errors
///
/// When the file cannot be read from disk, or its bytes do not parse as ELF.
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<OwnedElf> {
    let data = std::fs::read(path).context("failed to read file")?;
    OwnedElf::parse(data)
}

/// An owned ELF: the backing file bytes, freed on drop.
///
/// Only the bytes are stored. [`object::File`] is a borrowing view with no
/// owned variant, so holding one alongside its bytes would make this
/// self-referential; [`file`](Self::file) re-parses instead.
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
    /// # Errors
    ///
    /// When `bytes` do not parse as a valid ELF.
    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        // Validate now so `file()` can re-parse the identical bytes infallibly.
        object::File::parse(&bytes[..]).context("failed to parse ELF")?;
        Ok(Self {
            backing: bytes.into_boxed_slice(),
        })
    }

    /// Re-parses each call; see the type docs. Infallible because
    /// [`parse`](Self::parse) validated these exact immutable bytes.
    #[inline]
    pub fn file(&self) -> object::File<'_> {
        object::File::parse(&self.backing[..]).expect("bytes were validated as ELF at construction")
    }
}
