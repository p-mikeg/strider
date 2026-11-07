/// Read access to a statically-known region of memory.
///
/// Every address an impl resolves must be runtime-immutable: a constant-address
/// load folds to these bytes regardless of prior stores. `Send` but not `Sync`,
/// so an impl may hold a `RefCell` memo.
pub trait ReadOnlyMemory: Send {
    /// Fills all of `buf` with the raw bytes at `[addr, addr + buf.len())`, no
    /// endianness swap.
    ///
    /// # Errors
    ///
    /// When the range is not wholly mapped or `addr + buf.len()` overflows `u64`.
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()>;

    /// [`Self::read`] as a `bool`; override to skip building the error.
    fn try_read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.read(addr, buf).is_ok()
    }
}

impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for Box<T> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read(addr, buf)
    }

    fn try_read(&self, addr: u64, buf: &mut [u8]) -> bool {
        (**self).try_read(addr, buf)
    }
}
