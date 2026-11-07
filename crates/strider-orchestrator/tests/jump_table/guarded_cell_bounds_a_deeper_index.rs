//! `switch (map[c])` over a ROM byte map.
//!
//! gcc 11 -O2 for ARM and Thumb-2:
//!
//! ```c
//! static const unsigned char map[256] = {['a']=1, ['b']=2, ['c']=3, ['d']=4,
//!     ['e']=5, ['f']=F, ['h']=6, ['0']=7, ['1']=8};
//! int two_s(const char *s) { int r = 0;
//!     for (; *s; s++) switch (map[(unsigned char)*s]) { case 0: ... case 8: ... }
//!     return r; }
//! ```
//!
//! ```text
//! ARM, F = 9:    ldrb r3,[r1,r3]; cmp r3,#8; addls pc,pc,r3,lsl #2; b .L3
//! Thumb, F = 12: ldrb r3,[r1,r3]; cmp r3,#8; bhi .L3; tbb [pc,r3]
//! ```
//!
//! The table is indexed by the cell, but the bounded index the cell's address
//! reads is the char `c`. `map['f']` is past the table, and the guard on the
//! cell sends it to the default: no arm may come from it.

use strider_ir_test_utils::MockRom;

use crate::common;

const BASE: u64 = 0x4001a8;

/// A 256-byte map holding `cells`, zero elsewhere.
fn map(cells: &[(u8, u8)]) -> Vec<u8> {
    let mut map = vec![0u8; 256];
    for &(c, v) in cells {
        map[usize::from(c)] = v;
    }
    map
}

/// The `two_s` map, with `map['f'] = f`.
fn two_s_map(f: u8) -> Vec<u8> {
    map(&[
        (b'a', 1),
        (b'b', 2),
        (b'c', 3),
        (b'd', 4),
        (b'e', 5),
        (b'f', f),
        (b'h', 6),
        (b'0', 7),
        (b'1', 8),
    ])
}

/// The sorted `Switch` arms and the incompleteness reasons of the function at
/// `entry` in `bytes`, mapped and served as ROM at `base`.
fn analyze_image(arch: common::Arch, base: u64, bytes: Vec<u8>, entry: u64) -> (Vec<u64>, String) {
    let r = common::analyze_bytes(
        arch,
        bytes.clone(),
        base,
        Some(Box::new(MockRom::raw_bytes(base, bytes))),
        entry,
    );
    let mut arms: Vec<u64> = r
        .cfg
        .regions()
        .filter_map(|reg| match &reg.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .flatten()
        .map(|t| t.addr)
        .collect();
    arms.sort_unstable();
    (arms, r.incomplete().to_string())
}

/// `two_s` with its map placed right after `code`.
fn analyze(arch: common::Arch, code: &[&str], f: u8, entry: u64) -> (Vec<u64>, String) {
    let mut bytes = common::hex(&code.concat());
    bytes.extend(two_s_map(f));
    analyze_image(arch, BASE, bytes, entry)
}

/// ARM `two_s`, 0xc8 bytes.
const ARM: &[&str] = &[
    "0030d0e5000053e32a00000aac109fe50020a0e304e02de50cc0a0e3a0e09fe5",
    "01108fe00ee08fe00330d1e7080053e303f18f90090000ea1a0000ea1b0000ea",
    "160000ea130000ea0f0000ea0c0000ea090000ea060000eaffffffea1220a0e3",
    "0130f0e5000053e3eeffff1a0200a0e104f09de4112082e2f8ffffea102002e2",
    "f6ffffea0f2082e3f4ffffea00309ee5032082e0f1ffffea0d2042e2efffffea",
    "9c0202e0edffffea0a2082e2ebffffea0b2022e2e9ffffea0300a0e11eff2fe1",
    "a00000002c0e0100",
];

/// Mutation: drop the cell-guard filter in `dispatch_domain`.
#[test]
fn arm_cell_guard_excludes_a_cell_past_the_table() {
    let (arms, incomplete) = analyze(common::Arch::Arm, ARM, 9, BASE);
    let want: Vec<u64> = (0..9).map(|i| 0x4001e0 + 4 * i).collect();
    assert_eq!(arms, want, "{arms:#x?}");
    assert!(incomplete.is_empty(), "{incomplete}");
}

/// Thumb `two_s`, 0x6c bytes.
const THUMB: &[&str] = &[
    "03786bb310b500221649174c4ff00c0e79447c44cb5c082b07d8dfe803f01c1e",
    "191714110e0c0500122210f8013f002bf0d1104610bd1132f7e702f01002f4e7",
    "42f00f02f1e723681a44eee70d3aece70efb02f2e9e70a32e7e782f00b02e4e7",
    "1846704758000000420e0100",
];

/// Mutation: drop the cell-guard filter in `dispatch_domain`.
#[test]
fn thumb_cell_guard_excludes_a_cell_past_the_table() {
    let (arms, incomplete) = analyze(common::Arch::ArmThumb, THUMB, 12, BASE | 1);
    assert_eq!(
        arms,
        [
            0x4001d0, 0x4001de, 0x4001e2, 0x4001e8, 0x4001ee, 0x4001f4, 0x4001f8, 0x4001fe,
            0x400202
        ],
        "{arms:#x?}"
    );
    assert!(incomplete.is_empty(), "{incomplete}");
}

/// clang 14 -O2 AArch64 `g_phi2`, 0xa4 bytes at 0x40031c, over an extern
/// `xm8` holding, among others, `['b']=2, ['f']=9, ['g']=13`:
///
/// ```c
/// int g_phi2(int k) { int r = k; unsigned char c;
///     if (k > 5) c = 'g'; else if (k < 0) c = 'b'; else c = 'f';
///     sink = c; switch (xm8[c]) { case 0: ... case 8: ... } return r; }
/// ```
///
/// The index is a phi of the constants `'b'`, `'f'` and `'g'`; only `'b'`
/// passes the cell's `cmp w9,#8; b.hi`.
const G_PHI2: &[&str] = &[
    "c80c8052490c80521f00007128b1881a1f140071e90c80522ac1881a08000090",
    "08a1169109496a38880000b00a0100b93f210071680300540a0000904a251691",
    "8b0000104c6969386b090c8b60011fd600280011c0035fd6680180520000084a",
    "c0035fd60804000b00751e53c0035fd600340051c0035fd6080140b90001000b",
    "c0035fd6000c0032c0035fd600001c12c0035fd600440011c0035fd640028052",
    "c0035fd6",
];

/// Mutation: drop the cell-guard filter on the phi-constants path of
/// `dispatch_domain`.
#[test]
fn aarch64_cell_guard_filters_phi_constants() {
    const G_PHI2_AT: u64 = 0x40031c;
    const TABLE_AT: u64 = 0x400589;
    const XM8_AT: u64 = 0x4005a8;
    let mut bytes = common::hex(&G_PHI2.concat());
    bytes.resize(usize::try_from(TABLE_AT - G_PHI2_AT).expect("small"), 0);
    bytes.extend(common::hex("000205080a0d0f1113"));
    bytes.resize(usize::try_from(XM8_AT - G_PHI2_AT).expect("small"), 0);
    bytes.extend(map(&[
        (b'a', 1),
        (b'b', 2),
        (b'c', 3),
        (b'd', 4),
        (b'e', 5),
        (b'f', 9),
        (b'g', 13),
        (b'h', 6),
        (b'0', 7),
        (b'1', 8),
        (b'2', 255),
    ]));
    let (arms, incomplete) = analyze_image(common::Arch::Aarch64, G_PHI2_AT, bytes, G_PHI2_AT);
    assert_eq!(arms, [0x400380], "{arms:#x?}");
    assert!(incomplete.is_empty(), "{incomplete}");
}
