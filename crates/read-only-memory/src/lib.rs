/// Read access to a statically-known region of memory.
///
/// Every address an impl resolves must be runtime-immutable: `LoadReadOnly`
/// folds a constant-address load to these bytes without consulting its memory
/// chain, so resolving writable memory folds a store-then-reload to the stale
/// file value. An unresolved load stays sound.
///
/// `Send` lets `strider-py` drop the GIL around `analyze`. Not `Sync`, so an
/// impl may hold a `RefCell` memo.
pub trait ReadOnlyMemory: Send {
    /// Fills all of `buf` with the raw bytes at `[addr, addr + buf.len())`, no
    /// endianness swap, or errors; never a partial fill. An empty `buf` still
    /// resolves `addr` where a region table answers.
    ///
    /// # Errors
    ///
    /// When the address is unmapped, the range runs past the mapped region, or
    /// `addr + buf.len()` overflows `u64`.
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()>;
}

impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for Box<T> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read(addr, buf)
    }
}
