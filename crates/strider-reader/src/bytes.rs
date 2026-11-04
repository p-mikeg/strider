use std::sync::Arc;

use anyhow::Context as _;

/// A shared byte buffer regions are cut from: a memory-mapped file or an
/// owned allocation. Cloning shares.
///
/// A mapped file must not change on disk while mapped, else a read tears or
/// SIGBUSes past a new end; [`check_unchanged`](Self::check_unchanged) catches
/// a change between operations.
#[derive(Clone)]
pub(crate) struct FileBytes(Store);

#[derive(Clone)]
enum Store {
    Owned(Arc<Vec<u8>>),
    Mapped(Arc<MappedFile>),
}

struct MappedFile {
    map: memmap2::Mmap,
    /// Held open so the check re-stats the mapped inode, not the path.
    file: std::fs::File,
    path: std::path::PathBuf,
    identity: FileIdentity,
}

/// The mapped inode's size and mtime when mapped: a hint, blind to a
/// same-size rewrite within one second. Whole seconds because drvfs truncates
/// mtime on `utimensat`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    /// Seconds since the epoch; `None` when unavailable or before it.
    mtime_secs: Option<u64>,
}

impl FileIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            mtime_secs: meta.modified().ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            }),
        }
    }

    /// What differs from `now`, for the error message; `None` when equal.
    fn diff(&self, now: &Self) -> Option<String> {
        if self == now {
            return None;
        }
        if self.len != now.len {
            return Some(format!(
                "its size went from {} to {} bytes",
                self.len, now.len
            ));
        }
        Some("it was written to".to_owned())
    }
}

/// Cap on the capacity reserved from a `stat` size, which may be bogus.
const MAX_RESERVED_READ: u64 = 64 << 20;

/// The first `len` bytes of `file`, `len` being its `stat` size, so a
/// growing file or a `/proc` file that stats empty stops there.
fn read_all(file: &mut std::fs::File, len: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let reserve = usize::try_from(len.min(MAX_RESERVED_READ)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(reserve);
    file.take(len).read_to_end(&mut bytes)?;
    Ok(bytes)
}

impl FileBytes {
    pub(crate) fn from_vec(bytes: Vec<u8>) -> Self {
        Self(Store::Owned(Arc::new(bytes)))
    }

    /// Maps `path` read-only, reading it instead when mapping fails or
    /// `STRIDER_NO_MMAP=1` is set (a mapping turns a paging error, live on a
    /// network or 9p mount, into SIGBUS).
    ///
    /// # Errors
    ///
    /// When the file cannot be opened or stat'd, is not a regular file, or
    /// cannot be read.
    pub(crate) fn map_path<P: AsRef<std::path::Path>>(path: P) -> crate::Result<Self> {
        let path = path.as_ref();
        // Before the open, which blocks on a FIFO. A stat failure falls
        // through to the open's own error.
        if std::fs::metadata(path).is_ok_and(|m| !m.is_file()) {
            anyhow::bail!("{}: not a regular file", path.display());
        }
        let mut file = std::fs::File::open(path).context("failed to read file")?;
        // Again on the fd: the path may have been swapped since the stat.
        let opened = file.metadata().context("failed to stat file")?;
        if !opened.is_file() {
            anyhow::bail!("{}: not a regular file", path.display());
        }
        if std::env::var_os("STRIDER_NO_MMAP").is_some_and(|v| v != "0") {
            return Ok(Self::from_vec(
                read_all(&mut file, opened.len()).context("failed to read file")?,
            ));
        }
        // SAFETY: the mapping contract above is the caller's; the bytes are
        // only ever read through a shared reference.
        match unsafe { memmap2::Mmap::map(&file) } {
            Ok(map) => {
                let meta = file.metadata().context("failed to stat mapped file")?;
                Ok(Self(Store::Mapped(Arc::new(MappedFile {
                    map,
                    identity: FileIdentity::of(&meta),
                    file,
                    path: path.to_path_buf(),
                }))))
            }
            Err(map_err) => match read_all(&mut file, opened.len()) {
                Ok(bytes) => Ok(Self::from_vec(bytes)),
                Err(read_err) => Err(anyhow::Error::new(read_err).context(format!(
                    "failed to read file, and mapping it failed: {map_err}"
                ))),
            },
        }
    }

    /// One `stat` of the mapped file against its identity when mapped;
    /// always `Ok` for owned bytes.
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
        let meta = m
            .file
            .metadata()
            .with_context(|| format!("mapped file {path} can no longer be stat'd"))?;
        match m.identity.diff(&FileIdentity::of(&meta)) {
            None => Ok(()),
            Some(what) => anyhow::bail!(
                "mapped file {path} changed on disk since it was mapped: {what}. \
                 Re-open it, or set STRIDER_NO_MMAP=1 to read it into memory instead."
            ),
        }
    }

    /// Identity of the backing mapping, so regions sharing one stat it once.
    /// `None` for owned bytes.
    pub(crate) fn mapping_id(&self) -> Option<usize> {
        match &self.0 {
            Store::Owned(_) => None,
            Store::Mapped(m) => Some(Arc::as_ptr(m) as usize),
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

/// The unsigned integer `bytes` hold, little-endian under `le`.
pub(crate) fn read_uint(bytes: &[u8], le: bool) -> u64 {
    let n = bytes.len();
    debug_assert!(n <= 8, "a field wider than the carrier");
    let mut word = [0u8; 8];
    if le {
        word[..n].copy_from_slice(bytes);
        u64::from_le_bytes(word)
    } else {
        word[8 - n..].copy_from_slice(bytes);
        u64::from_be_bytes(word)
    }
}

/// The low `out.len()` bytes of `value`, little-endian under `le`.
pub(crate) fn write_uint(out: &mut [u8], value: u64, le: bool) {
    let n = out.len();
    debug_assert!(n <= 8, "a field wider than the carrier");
    if le {
        out.copy_from_slice(&value.to_le_bytes()[..n]);
    } else {
        out.copy_from_slice(&value.to_be_bytes()[8 - n..]);
    }
}
