//! ARM / AArch64 mapping symbols: a `$d` marks the bytes up to the next code
//! mapping symbol as data, which is how a literal pool inside a function is
//! told apart from code.

#[path = "common/mod.rs"]
mod common;

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

/// A run of `$d` marks with no code mark between them is one span, not one
/// nested range per mark, which also keeps the scan for each one's end from
/// costing the rest of the run.
#[test]
fn a_run_of_data_marks_is_one_range() {
    use object::write::{Object, Symbol, SymbolSection};
    use object::{
        Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
    };

    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Arm, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0u8; 0x40], 4);
    let mut mark = |name: String, value: u64| {
        obj.add_symbol(Symbol {
            name: name.into_bytes(),
            value,
            size: 0,
            kind: SymbolKind::Label,
            scope: SymbolScope::Compilation,
            weak: false,
            section: SymbolSection::Section(text),
            flags: SymbolFlags::None,
        });
    };
    mark("$a".to_owned(), 0);
    for (i, at) in [0x10u64, 0x14, 0x18, 0x1c].iter().enumerate() {
        mark(format!("$d.{i}"), *at);
    }
    mark("$a.1".to_owned(), 0x20);
    mark("$d.4".to_owned(), 0x30);
    let bytes = obj.write().expect("write");

    let file = object::File::parse(&bytes[..]).expect("parse");
    let base = common::section_base(&bytes, ".text");
    assert_eq!(
        mapping_symbol_data_ranges(&file),
        vec![base + 0x10..base + 0x20, base + 0x30..base + 0x40]
    );
}
