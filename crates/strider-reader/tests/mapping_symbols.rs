//! ARM / AArch64 mapping symbols: a `$d` marks the bytes up to the next code
//! mapping symbol as data, which is how a literal pool inside a function is
//! told apart from code.

use strider_reader::elf::mapping_symbol_data_ranges;

fn ranges_of(fixture: &str) -> Vec<std::ops::Range<u64>> {
    let path = format!(
        "{}/../../fixtures/out/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let obj = object::File::parse(&bytes[..]).expect("parse");
    mapping_symbol_data_ranges(&obj)
}

#[test]
fn thumb_literal_pools_run_to_the_next_code_mapping_symbol() {
    // readelf -sW: in `.plt` $a 0x398, $d 0x3a8, $a 0x3ac; in `.text` (ending
    // 0x5d4) $d at 0x444, 0x474, 0x498, 0x4bc, 0x4f0 and 0x52c, each followed
    // by a $t or $a. The `$d`s of non-executable sections are not listed.
    assert_eq!(
        ranges_of("arm_thumb/calls.elf"),
        vec![
            0x3a8..0x3ac,
            0x444..0x448,
            0x474..0x47c,
            0x498..0x4a0,
            0x4bc..0x4cc,
            0x4f0..0x500,
            0x52c..0x540,
        ]
    );
}

#[test]
fn an_image_without_mapping_symbols_has_no_data_ranges() {
    assert!(ranges_of("x64/calls.elf").is_empty());
}
