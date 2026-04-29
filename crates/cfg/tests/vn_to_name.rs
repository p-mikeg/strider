#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Cfg::vn_to_name` — every supported space variant plus the
//! two error paths (`InvalidRegVn`, `UnsupportedVnSpaceDisplay`).

mod common;
use common::{binary, build_cfg};

use cfg::test_api::vn_to_name;
use rsleigh::{Vn, VnAddr, VnSpace};

fn real_cfg() -> cfg::Cfg<reader::ElfFileMemReader> {
    let p = binary("x64", "add");
    build_cfg(
        p.to_str().unwrap(),
        "add",
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    )
}

// ── CONST ─────────────────────────────────────────────────────────────────────

#[test]
fn const_formats_as_hex_offset_colon_size() {
    let cfg = real_cfg();
    let vn = Vn { addr: VnAddr { space: VnSpace::CONST, off: 0x2a }, size: 4 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "0x2a:4");
}

// ── RAM ───────────────────────────────────────────────────────────────────────

#[test]
fn ram_formats_as_ram_offset_size() {
    let cfg = real_cfg();
    let vn = Vn { addr: VnAddr { space: VnSpace::RAM, off: 0x1000 }, size: 8 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "ram[0x1000]:8");
}

// ── UNIQUE ────────────────────────────────────────────────────────────────────

#[test]
fn unique_formats_as_unique_offset_size() {
    let cfg = real_cfg();
    let vn = Vn { addr: VnAddr { space: VnSpace::UNIQUE, off: 0x80 }, size: 1 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "unique[0x80]:1");
}

// ── REGISTER ──────────────────────────────────────────────────────────────────

#[test]
fn register_known_offset_returns_register_name() {
    let cfg = real_cfg();
    let regs = cfg.sleigh.regs().unwrap();
    // Pick a well-known x86-64 register. Try a few names until one resolves.
    let candidates = ["RAX", "RDI", "RSI", "EAX", "AX"];
    let (name, vn) = candidates
        .iter()
        .find_map(|&n| regs.name_to_vn(n).map(|v| (n, v)))
        .expect("no known register name resolved on x86-64 Sleigh");

    let resolved = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(resolved, name);
}

#[test]
fn register_unknown_offset_returns_invalid_reg_vn_error() {
    let cfg = real_cfg();
    // Pick a REGISTER-space offset the register table will not map — far
    // outside any real register.
    let bogus = Vn {
        addr: VnAddr { space: VnSpace::REGISTER, off: 0xffff_ffff_ffff_ffff },
        size: 1,
    };
    let err = vn_to_name(&cfg, &bogus).unwrap_err();
    assert!(err.to_string().contains("invalid register vn"), "got: {err}");
}

// ── unsupported space ─────────────────────────────────────────────────────────

#[test]
fn unsupported_space_returns_unsupported_error() {
    let cfg = real_cfg();
    // Use a space shortcut that is neither CONST (#), REGISTER (%),
    // RAM (r), nor UNIQUE (u). Any other byte triggers the `_` arm.
    let exotic = Vn {
        addr: VnAddr { space: VnSpace::new(b'?'), off: 0 },
        size: 1,
    };
    let err = vn_to_name(&cfg, &exotic).unwrap_err();
    assert!(err.to_string().contains("unsupported varnode space"), "got: {err}");
}
