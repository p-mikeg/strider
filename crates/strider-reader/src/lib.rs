pub type Result<T> = anyhow::Result<T>;

mod bytes;
pub mod elf;
mod index;
mod region;
mod table;
#[cfg(test)]
mod tests;
pub(crate) use bytes::FileBytes;
pub use elf::{ElfFileMemReader, OwnedElf, load_elf};
pub use index::{Covering, RegionIndex};
pub use region::MemRegion;
pub(crate) use region::{MAX_PATCH_LEN, Patch, collapse_equal_starts};
pub use table::MemRegionsLookupTable;

/// Error type for every [`rsleigh::MemReader`] impl in the strider crates.
///
/// `rsleigh::MemReader` requires `Err: std::error::Error + 'static`, which
/// `anyhow::Error` does not implement. This wrapper satisfies the
/// bound while keeping `anyhow!` / `?` usable at call sites.
#[derive(Debug)]
pub struct MemReadError(pub(crate) anyhow::Error);

impl std::fmt::Display for MemReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for MemReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // `anyhow::Error` isn't a `std::error::Error`, but its inner cause is.
        self.0.source()
    }
}

impl From<anyhow::Error> for MemReadError {
    fn from(err: anyhow::Error) -> Self {
        MemReadError(err)
    }
}

pub use read_only_memory::ReadOnlyMemory;
