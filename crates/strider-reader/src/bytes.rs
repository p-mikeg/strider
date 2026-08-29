use std::sync::Arc;

use anyhow::Context as _;

/// A shared, immutable byte buffer regions are cut from: a memory-mapped file
/// or an owned allocation. Cloning shares, never copies.
///
/// # Mapping contract
///
/// A mapped file must not change on disk while it is mapped; the mapping's
/// bytes are otherwise not immutable, and a read can observe a torn or
/// truncated file (SIGBUS past the new end). [`check_unchanged`] turns the
/// common case of that -- a rebuild between two operations -- into an `Err`,
/// but a change racing a read in progress still tears.
///
/// [`check_unchanged`]: FileBytes::check_unchanged
#[derive(Clone)]
pub(crate) struct FileBytes(Store);

#[derive(Clone)]
enum Store {
    Owned(Arc<Vec<u8>>),
    Mapped(Arc<MappedFile>),
}

struct MappedFile {
    map: memmap2::Mmap,
    path: std::path::PathBuf,
    identity: FileIdentity,
}

/// What a `stat` says about the mapped file, sampled when it was mapped.
///
/// Only a coherence hint: a rewrite in place that keeps the size and lands
/// inside the filesystem's mtime granularity is indistinguishable from no
/// change at all.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    /// `None` on a filesystem that does not report one.
    mtime: Option<std::time::SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            mtime: meta.modified().ok(),
            #[cfg(unix)]
            dev: std::os::unix::fs::MetadataExt::dev(meta),
            #[cfg(unix)]
            ino: std::os::unix::fs::MetadataExt::ino(meta),
        }
    }

    /// What differs from `now`, for the error message; `None` when equal.
    fn diff(&self, now: &Self) -> Option<String> {
        if self == now {
            return None;
        }
        #[cfg(unix)]
        if (self.dev, self.ino) != (now.dev, now.ino) {
            return Some("it was replaced by a different file".to_owned());
        }
        if self.len != now.len {
            return Some(format!(
                "its size went from {} to {} bytes",
                self.len, now.len
            ));
        }
        Some("its modification time changed".to_owned())
    }
}

impl FileBytes {
    pub(crate) fn from_vec(bytes: Vec<u8>) -> Self {
        Self(Store::Owned(Arc::new(bytes)))
    }

    /// Maps `path` read-only, falling back to reading it into memory on any
    /// mapping failure (a filesystem that cannot map).
    ///
    /// Set `STRIDER_NO_MMAP=1` to read the file instead of mapping it. A
    /// mapping turns a paging error into SIGBUS, which no caller can catch and
    /// which kills the process; on a network or 9p mount that is a live risk
    /// even for a file nothing is writing. Reading costs the file's size in
    /// memory and gives an ordinary `Err` instead.
    ///
    /// # Errors
    ///
    /// When the file cannot be opened or stat'd, or when the fallback read
    /// fails too; the mapping error is then folded into the message.
    pub(crate) fn map_path<P: AsRef<std::path::Path>>(path: P) -> crate::Result<Self> {
        let path = path.as_ref();
        if std::env::var_os("STRIDER_NO_MMAP").is_some_and(|v| v != "0") {
            return Ok(Self::from_vec(
                std::fs::read(path).context("failed to read file")?,
            ));
        }
        let file = std::fs::File::open(path).context("failed to read file")?;
        // SAFETY: the mapping contract above is the caller's; the bytes are
        // only ever read through a shared reference.
        match unsafe { memmap2::Mmap::map(&file) } {
            Ok(map) => {
                // fstat on the fd just mapped, so the identity cannot be one a
                // rename slipped in between the open and the map.
                let meta = file.metadata().context("failed to stat mapped file")?;
                Ok(Self(Store::Mapped(Arc::new(MappedFile {
                    map,
                    path: path.to_path_buf(),
                    identity: FileIdentity::of(&meta),
                }))))
            }
            Err(map_err) => match std::fs::read(path) {
                Ok(bytes) => Ok(Self::from_vec(bytes)),
                Err(read_err) => Err(anyhow::Error::new(read_err).context(format!(
                    "failed to read file, and mapping it failed: {map_err}"
                ))),
            },
        }
    }

    /// One `stat` of the mapped file, comparing it against what it was when it
    /// was mapped. Owned bytes are always coherent, and answer `Ok` without a
    /// syscall.
    ///
    /// Meant for the top of an operation, not for the read path: a change
    /// between this and a later read is still a torn read or a SIGBUS.
    ///
    /// # Errors
    ///
    /// When the file no longer stats, or no longer looks like the file that
    /// was mapped.
    pub(crate) fn check_unchanged(&self) -> crate::Result<()> {
        let Store::Mapped(m) = &self.0 else {
            return Ok(());
        };
        let path = m.path.display();
        let meta = std::fs::metadata(&m.path)
            .with_context(|| format!("mapped file {path} can no longer be read"))?;
        match m.identity.diff(&FileIdentity::of(&meta)) {
            None => Ok(()),
            Some(what) => anyhow::bail!(
                "mapped file {path} changed on disk since it was mapped: {what}. \
                 Re-open it, or set STRIDER_NO_MMAP=1 to read it into memory instead."
            ),
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        match &self.0 {
            Store::Owned(v) => v,
            Store::Mapped(m) => &m.map,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.as_slice().len()
    }
}
