use std::sync::Arc;

use anyhow::Context as _;

/// A shared, immutable byte buffer regions are cut from: a memory-mapped file
/// or an owned allocation. Cloning shares, never copies.
///
/// # Mapping contract
///
/// A mapped file must not change on disk while it is mapped; the mapping's
/// bytes are otherwise not immutable, and a read can observe a torn or
/// truncated file (SIGBUS past the new end).
#[derive(Clone)]
pub(crate) struct FileBytes(Store);

#[derive(Clone)]
enum Store {
    Owned(Arc<Vec<u8>>),
    Mapped(Arc<memmap2::Mmap>),
}

impl FileBytes {
    pub(crate) fn from_vec(bytes: Vec<u8>) -> Self {
        Self(Store::Owned(Arc::new(bytes)))
    }

    /// Maps `path` read-only, falling back to reading it into memory on any
    /// mapping failure (a filesystem that cannot map, an empty file).
    ///
    /// # Errors
    ///
    /// When the file cannot be opened, or when the fallback read fails too, in
    /// which case the mapping error is the reported cause.
    pub(crate) fn map_path<P: AsRef<std::path::Path>>(path: P) -> crate::Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path).context("failed to read file")?;
        // SAFETY: the mapping contract above is the caller's; the bytes are
        // only ever read through a shared reference.
        match unsafe { memmap2::Mmap::map(&file) } {
            Ok(map) => Ok(Self(Store::Mapped(Arc::new(map)))),
            Err(map_err) => match std::fs::read(path) {
                Ok(bytes) => Ok(Self::from_vec(bytes)),
                Err(read_err) => Err(anyhow::Error::new(read_err).context(format!(
                    "failed to read file, and mapping it failed: {map_err}"
                ))),
            },
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        match &self.0 {
            Store::Owned(v) => v,
            Store::Mapped(m) => m,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.as_slice().len()
    }
}
