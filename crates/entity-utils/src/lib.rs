#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

extern crate alloc;

pub mod set;
pub mod worklist;

pub use set::DenseEntitySet;
