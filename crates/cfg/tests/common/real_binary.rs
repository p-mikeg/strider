#![allow(deprecated)] // x86-only test fixtures: Builder::new defaults LE+X86_64 which is correct here.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Real-binary CFG helpers for integration tests.

use cfg::Cfg;
use object::{Object, ObjectSymbol};

/// Maps a fixture function name to the category file that defines it.
/// The original monolithic `test.c` was split into per-category fixtures
/// (`fixtures/cases/<category>.c`) by the analyzer-crate review; this map
/// preserves the cfg-integration test interface across that split.
pub fn category_for_fn(fn_name: &str) -> &'static str {
    match fn_name {
        // arithmetic.c
        "add" | "sub" | "mul" | "udiv" | "umod" | "sdiv" | "smod"
        | "bit_and" | "bit_or" | "bit_xor" | "bit_not"
        | "shl" | "lshr" | "ashr" | "negate" => "arithmetic",
        // control.c
        "abs_val" | "max_val" | "clamp" | "select_three"
        | "sum_to_n" | "factorial" | "count_bits"
        | "nested_loops" | "early_return" => "control",
        // memory.c
        "array_sum" | "array_fill" | "array_copy" | "pointer_chase"
        | "struct_field_load" | "struct_field_store" | "tagged_union_read" => "memory",
        // calls.c
        "fib_recursive" | "mutual_a" | "mutual_b" | "nested_3deep"
        | "repeat_call_pair" | "pass_through" | "apply_indirect"
        | "leaf" | "mid" | "pair_a" => "calls",
        // complex.c
        "read_struct_fields" | "write_struct_fields" | "nested_struct_field"
        | "bit_test_zero" | "if_bit_clear_call" | "call_with_field_arg"
        | "dispatch_on_flag" | "multi_arg_call_in_branch" | "complex_dispatch"
        | "call_uses_call_return"
        | "cb_zero" | "cb_set" | "invoke" | "ext_three"
        | "produce" | "consume" => "complex",
        // calling_convention.c
        "forward_1" | "forward_2" | "forward_4" | "forward_8" | "forward_16"
        | "narrow_widths" | "mixed_4"
        | "returns_int" | "uses_return"
        | "sink1" | "sink2" | "sink4" | "sink8" | "sink16"
        | "sink_narrow" | "sink_mixed" => "calling_convention",
        other => panic!("unknown fixture function {other:?} — add it to category_for_fn"),
    }
}

/// Returns the path to the test binary for `(arch, fn_name)` under
/// `fixtures/out/<arch>/<category>.elf`.  The category is derived from
/// the function name via [`category_for_fn`].
pub fn binary(arch: &str, fn_name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{}.elf", category_for_fn(fn_name)))
}

/// Resolves a named symbol's start address from an ELF file on disk.
///
/// On ARM Thumb the ELF symbol's low bit is set to mark the function as
/// Thumb-encoded.  Callers that want the raw decode address should use
/// [`symbol_decode_addr`], which masks bit 0 when the binary path lives
/// under `fixtures/out/arm_thumb/`.
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

/// Returns the address Sleigh expects to begin decoding from.
///
/// For ARM Thumb fixtures (binaries under `fixtures/out/arm_thumb/...`)
/// the ELF symbol's low bit is the Thumb-mode marker, not part of the
/// instruction address.  Sleigh raises "Instruction address not aligned"
/// if you hand it the raw symbol value, so this helper masks it off.
pub fn symbol_decode_addr(binary_path: &str, fn_name: &str) -> u64 {
    let raw = symbol_addr(binary_path, fn_name);
    if binary_path.contains("/arm_thumb/") {
        raw & !1
    } else {
        raw
    }
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
    let addr = symbol_decode_addr(binary_path, fn_name);
    let mem_reader =
        reader::ElfFileMemReader::from_path(binary_path).expect("build ElfFileMemReader");
    let sleigh = rsleigh::Sleigh::new(sla_spec, pspec, mem_reader).expect("create Sleigh");
    cfg::Builder::new(sleigh, addr, cfg::OptionsBuilder::new().build())
        .build()
        .unwrap_or_else(|e| panic!("CFG build failed for '{fn_name}': {e:?}"))
}
