//! Backend-agnostic assertions over the `rsleigh::MemReader` and
//! `reader::ReadOnlyMemory` traits.
//!
//! When a new backend (PE, Mach-O, raw blob, …) lands, its test file
//! builds the reader and calls these helpers in addition to its own
//! backend-specific assertions.
