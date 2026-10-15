/// Read access to a statically-known region of memory.
///
/// # Immutability contract
///
/// Every address an impl resolves MUST be runtime-immutable. `LoadReadOnly`
/// folds a constant-address load to the resolved bytes WITHOUT consulting the
/// load's memory-token chain, so resolving writable memory (`.data`, `.got`,
/// `.data.rel.ro`, the stack, ...) makes a store-then-reload fold to the stale
/// file-initial value. When in doubt resolve fewer addresses: an unresolved
/// load stays intact and sound.
///
/// `Send` is what `strider-py` needs to drop the GIL around `analyze` with
/// `Python::allow_threads`, which requires everything the closure captures to
/// be `Send`. `Sync` was carried alongside it and nothing ever needed it: no
/// `&dyn ReadOnlyMemory` crosses a thread in the workspace, and requiring it
/// barred an impl holding a `RefCell` memo, which the single-threaded design
/// otherwise invites.
pub trait ReadOnlyMemory: Send {
    /// Fills `buf` with the bytes at `[addr, addr + buf.len())`.
    ///
    /// Fill-all-or-error: no partial fill, no truncation. Bytes are copied
    /// raw, with NO endianness swap; a caller wanting an integer decodes them
    /// per the target's endianness itself.
    ///
    /// An EMPTY `buf` still resolves `addr` wherever a region table answers:
    /// `RegionIndex::covering` floors the length at 1, so an unmapped address
    /// is an error rather than a vacuous `Ok(())`, which is what `# Errors`
    /// below states. An impl that resolves nothing of its own (`MockRom`, a
    /// Python `read`) may still answer `Ok(())`; no caller issues a
    /// zero-length read.
    ///
    /// `addr + buf.len()` is computed in `u64` and MUST NOT wrap: a range
    /// that overflows the address space is out of bounds, never an error-free
    /// wrap to low memory.
    ///
    /// # Errors
    ///
    /// When the address is unmapped, the range runs past the mapped region, or
    /// `addr + buf.len()` would overflow.
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()>;
}

impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for Box<T> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read(addr, buf)
    }
}
