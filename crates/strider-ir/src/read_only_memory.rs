//! `ReadOnlyMemory` trait — read access to a statically-known region of
//! memory (typically a binary's `.rodata` or `.text` section).
//!
//! Defined here rather than in `strider-reader` so optimizer passes can depend
//! on the trait without back-edging through `strider-binary`.  Concrete impls
//! (e.g. `strider_reader::ElfFileMemReader`) live in the `strider-reader` crate.

/// Provides read access to a statically-known region of memory (e.g. a
/// binary's `.rodata` or `.text` section).
///
/// The optimizer uses this trait to resolve `Load` nodes whose address is a
/// compile-time constant into the corresponding constant values, eliminating
/// the load entirely.
pub trait ReadOnlyMemory: Send + Sync {
    /// Reads up to `size` bytes from RAM at `addr`, returning the value as a
    /// little-endian-decoded u64.  Returns `None` if the address is unmapped
    /// or the size exceeds 8 bytes.
    fn read(&self, addr: u64, size: usize) -> Option<u64>;
}

// Blanket impls so any `Arc<T>` / `Box<T>` whose inner type implements
// `ReadOnlyMemory` is itself a `ReadOnlyMemory`.  Lets callers wrap a
// shared rom in an `Arc` (or own one in a `Box`) and feed it directly
// to the optimizer's `LoadReadOnly` pass without inlining a custom
// load-folder for each call site.
impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for std::sync::Arc<T> {
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        (**self).read(addr, size)
    }
}

impl<T: ?Sized + ReadOnlyMemory> ReadOnlyMemory for Box<T> {
    fn read(&self, addr: u64, size: usize) -> Option<u64> {
        (**self).read(addr, size)
    }
}
