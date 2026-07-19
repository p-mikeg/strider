// Included per test file via `#[path = "common/mod.rs"] mod common;`, so each
// test crate compiles its own copy and exercises only a subset. Hence the
// blanket `dead_code` allow.

#![allow(dead_code)]
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

pub(crate) mod elf_fixture;
pub(crate) mod reader_contract;
