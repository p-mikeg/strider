//! `ReadOnlyMemory`: read access to a statically-known region of memory
//! (typically a binary's `.rodata` or `.text`).
//!
//! Its own crate so the optimizer can depend on the trait without depending
//! on the ELF-parsing `strider-reader`, which owns the concrete impls.

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
pub trait ReadOnlyMemory: Send + Sync {
    /// Fills `buf` with the bytes at `[addr, addr + buf.len())`.
    ///
    /// Fill-all-or-error: no partial fill, no truncation. Bytes are copied
    /// raw, with NO endianness swap; a caller wanting an integer decodes them
    /// per the target's endianness itself.
    ///
    /// # Errors
    ///
    /// When the address is unmapped or the range runs past the mapped region.
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()>;
}

impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for Box<T> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read(addr, buf)
    }
}
