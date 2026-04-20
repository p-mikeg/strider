//! Memory-op pattern constructors.

use crate::pat::{LoadPat, StackStorePat, StackStorePhiPat, StorePat};

/// Starts building a `Load` pattern.  Chain `.addr()` / `.space()` to add
/// constraints.
pub fn load() -> LoadPat {
    LoadPat::new()
}
/// Starts building a `Store` pattern.  Chain `.addr()` / `.data()` / `.space()`
/// to add constraints.
pub fn store() -> StorePat {
    StorePat::new()
}
/// Starts building a `StackStore` pattern.  Chain `.offset()` / `.data()` /
/// `.space()` to add constraints.
pub fn stack_store() -> StackStorePat {
    StackStorePat::new()
}
/// Starts building a `StackStorePhi` pattern.  Chain `.offsets(…)` /
/// `.data()` / `.space()` to add constraints.
pub fn stack_store_phi() -> StackStorePhiPat {
    StackStorePhiPat::new()
}
