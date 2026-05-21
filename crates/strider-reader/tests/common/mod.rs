// Shared helpers for the strider-reader crate's integration tests.
//
// Included by each test file via:
//     #[path = "common/mod.rs"]
//     mod common;
//
// Items use `#[allow(dead_code)]` because any given test crate only
// exercises a subset — unused items would otherwise warn.

#![allow(dead_code)]
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

pub mod elf_fixture;
pub mod reader_contract;
