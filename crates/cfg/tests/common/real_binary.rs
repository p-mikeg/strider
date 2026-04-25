#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Real-binary CFG helpers for integration tests.

use cfg::Cfg;
use object::{Object, ObjectSymbol};

/// Returns the path to the test binary for `arch` under `binary_tests/out/<arch>/test.elf`.
pub fn binary(arch: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../binary_tests/out")
        .join(arch)
        .join("test.elf")
}

/// Resolves a named symbol's start address from an ELF file on disk.
pub fn symbol_addr(binary_path: &str, fn_name: &str) -> u64 {
    let leaked: &'static [u8] = Box::leak(
        std::fs::read(binary_path)
            .expect("read binary")
            .into_boxed_slice(),
    );
    let obj: &'static object::File<'static> =
        Box::leak(Box::new(object::File::parse(leaked).expect("parse ELF")));
    obj.symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol '{fn_name}' not found in {binary_path}"))
        .address()
}

/// Builds a CFG for the named function using `sla_spec`/`pspec` to decode.
///
/// The ELF is loaded from `binary_path`. The returned reader owns its backing
/// regions, so no leak or `'static` lifetime gymnastics are required.
pub fn build_cfg(
    binary_path: &str,
    fn_name: &str,
    sla_spec: rsleigh::sla_spec::SlaSpec,
    pspec: rsleigh::pspec::PSpec,
) -> Cfg<reader::ElfFileMemReader> {
    let addr = symbol_addr(binary_path, fn_name);
    let mem_reader =
        reader::ElfFileMemReader::from_path(binary_path).expect("build ElfFileMemReader");
    let sleigh = rsleigh::Sleigh::new(sla_spec, pspec, mem_reader).expect("create Sleigh");
    cfg::Builder::new(sleigh, addr, cfg::OptionsBuilder::new().build())
        .build()
        .unwrap_or_else(|e| panic!("CFG build failed for '{fn_name}': {e:?}"))
}
