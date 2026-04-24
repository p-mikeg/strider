//! Synthetic ELF byte builders used by integration tests.
//!
//! All builders produce a complete ELF byte buffer that `object::File::parse`
//! can consume. Sections are placed at caller-chosen virtual addresses by
//! writing via `object::write::elf::Writer` (low-level API).
