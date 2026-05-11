#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Cfg::vn_to_name` — every supported space variant.
//!
//! After the rsleigh-4 migration, `vn_to_display_name` delegates to
//! [`rsleigh::Vn::ctx_fmt`] (`crates/ir/src/dot/label.rs`).  rsleigh's
//! formatter happens to produce **byte-identical output** to the old
//! hand-rolled rendering for CONST / RAM / UNIQUE / known-register
//! varnodes (the sleigh space `.name()` table returns `"ram"`, `"unique"`,
//! `"register"`, `"const"` exactly).  The two formerly-erroring paths
//! (unknown-register-offset and exotic-space-byte) now produce a
//! best-effort fallback string instead of an error — pinned below.

mod common;
use common::{binary, build_cfg};

use cfg::test_api::vn_to_name;
use rsleigh::{Vn, VnSpace};

fn real_cfg() -> cfg::Cfg<reader::ElfFileMemReader> {
    let p = binary("x64", "add");
    build_cfg(
        &target::SleighArch::x86_64(),
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
    let vn = Vn { addr_off: 0x2a, addr_space: VnSpace::CONST, size: 4 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "0x2a:4");
}

// ── RAM ───────────────────────────────────────────────────────────────────────

#[test]
fn ram_formats_as_ram_offset_size() {
    let cfg = real_cfg();
    let vn = Vn { addr_off: 0x1000, addr_space: VnSpace::RAM, size: 8 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "ram[0x1000]:8");
}

// ── UNIQUE ────────────────────────────────────────────────────────────────────

#[test]
fn unique_formats_as_unique_offset_size() {
    let cfg = real_cfg();
    let vn = Vn { addr_off: 0x80, addr_space: VnSpace::UNIQUE, size: 1 };
    let name = vn_to_name(&cfg, &vn).unwrap();
    assert_eq!(name, "unique[0x80]:1");
}

// ── REGISTER ──────────────────────────────────────────────────────────────────

#[test]
fn register_known_offset_returns_register_name() {
    let cfg = real_cfg();
    let regs = cfg.sleigh().regs().unwrap();
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
fn register_unknown_offset_falls_back_to_space_addr_size() {
    let cfg = real_cfg();
    // Pick a REGISTER-space offset the register table will not map — far
    // outside any real register.  rsleigh's `VnCtxFmt::NotReg` arm formats
    // it as `<space-name>[0x<off>]:<size>` (no error).
    let bogus = Vn {
        addr_off: 0xffff_ffff_ffff_ffff, addr_space: VnSpace::REGISTER,
        size: 1,
    };
    let resolved = vn_to_name(&cfg, &bogus).unwrap();
    assert_eq!(resolved, "register[0xffffffffffffffff]:1");
}

// ── unsupported space ─────────────────────────────────────────────────────────

#[test]
fn unknown_space_byte_falls_back_to_shortcut_char() {
    let cfg = real_cfg();
    // Use a space shortcut that is neither CONST (#), REGISTER (%),
    // RAM (r), nor UNIQUE (u).  rsleigh's `VnSpaceCtxFmt::Unnamed` arm
    // renders the raw shortcut character via `Display for VnSpace`,
    // and the address as `<chr>[0x<off>]:<size>` (no error).
    let exotic = Vn {
        addr_off: 0, addr_space: VnSpace::new(b'?'),
        size: 1,
    };
    let resolved = vn_to_name(&cfg, &exotic).unwrap();
    assert_eq!(resolved, "?[0x0]:1");
}
