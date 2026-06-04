//! `ReadOnlyMemory` trait — read access to a statically-known region of
//! memory (typically a binary's `.rodata` or `.text` section).
//!
//! A tiny generic crate (like `dot` / `entity-utils` / `graphwalk`) so every
//! crate that needs the trait — the optimizer, the lifter, the reader — can
//! depend on it one-way.  In particular the optimizer depends on this rather
//! than on `strider-reader` so it never back-edges through the ELF-parsing
//! reader crate.  Concrete impls (e.g. `strider_reader::ElfFileMemReader`)
//! live in the `strider-reader` crate.

/// Provides read access to a statically-known region of memory (e.g. a
/// binary's `.rodata` or `.text` section).
///
/// The optimizer uses this trait to resolve `Load` nodes whose address is a
/// compile-time constant into the corresponding constant values, eliminating
/// the load entirely.
pub trait ReadOnlyMemory: Send + Sync {
    /// Fills `buf` with the bytes at `[addr, addr + buf.len())`.
    ///
    /// Fill-all-or-error: returns `Err` if any byte in the range is
    /// unmapped (no partial fill, no truncation).  The bytes are copied
    /// **raw** — there is NO endianness swap.  Callers that need an
    /// integer decode the raw bytes per the target's endianness
    /// (the optimizer does this via `strider_target::Endianness`); the
    /// reader no longer decodes.
    ///
    /// # Errors
    ///
    /// Returns an error when the address is unmapped or the requested
    /// range extends past the end of the mapped region.
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()>;
}

// Blanket impls so any `Arc<T>` / `Box<T>` whose inner type implements
// `ReadOnlyMemory` is itself a `ReadOnlyMemory`.  Lets callers wrap a
// shared rom in an `Arc` (or own one in a `Box`) and feed it directly
// to the optimizer's `LoadReadOnly` pass without inlining a custom
// load-folder for each call site.
impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for std::sync::Arc<T> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read(addr, buf)
    }
}

impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for Box<T> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read(addr, buf)
    }
}
