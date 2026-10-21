//! The presets against the Ghidra data they model: each calling convention
//! against its compiler spec prototype, and each arch against its language
//! definition row. Ghidra is an oracle here, not a source: where it is wrong
//! (ppc64 ELFv2's stack offset) or models less (no AltiVec), the preset keeps
//! its own answer and `LEDGER` says why.
//!
//! Register lists are compared as BYTES through the preset's own register
//! table, so `XMM0_Qa` against `XMM0` or `s0..s15` against `d0..d7` is judged
//! on storage, not spelling, and asymmetrically, as soundness requires:
//! - callee-saved against `<unaffected>` minus the stack pointer, both ways:
//!   an extra entry claims a register survives a call, a missing one reads a
//!   preserved register as clobbered;
//! - argument and return carriers: the preset covers every cspec carrier.
//!   Extra carriers (vector windows, x87 returns, ppc64 `r4`) are deliberate;
//! - the stack pointer, `extrapop` against `ret_stack_pop`, and the stack
//!   pentry's `offset` / `align` against `stack_args`;
//! - `<killedbycall>` shares no byte with callee-saved, without exception.
//!
//! Every other difference is a `LEDGER` row, and a row that no longer
//! diverges fails too.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use strider_target::{ArchPreset, CallingConvention, Endianness, SleighArch};

/// `rel` under the vendored Ghidra processor tree.
fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../externals/rsleigh/sleigh/processors")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; is the rsleigh submodule checked out?",
            path.display()
        )
    })
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, tag: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name(tag))
}

fn attr(file: &str, node: roxmltree::Node<'_, '_>, name: &str) -> String {
    node.attribute(name)
        .unwrap_or_else(|| panic!("{file}: <{}> has no {name}", node.tag_name().name()))
        .to_owned()
}

fn number<T: std::str::FromStr>(file: &str, node: roxmltree::Node<'_, '_>, name: &str) -> T {
    let text = attr(file, node, name);
    text.parse()
        .unwrap_or_else(|_| panic!("{file}: {name}=\"{text}\" is not a number"))
}

/// One preset held to one cspec prototype, on every arch it pairs with.
struct Pairing {
    preset: &'static str,
    cc: fn() -> CallingConvention,
    cspec: &'static str,
    /// `None` is the cspec's `<default_proto>`.
    prototype: Option<&'static str>,
    arches: &'static [fn() -> SleighArch],
}

const X86_64_GCC: &str = "x86/data/languages/x86-64-gcc.cspec";
const X86_GCC: &str = "x86/data/languages/x86gcc.cspec";
const AARCH64: &str = "AARCH64/data/languages/AARCH64.cspec";
const ARM: &str = "ARM/data/languages/ARM.cspec";
const MIPS32_BE: &str = "MIPS/data/languages/mips32be.cspec";
const MIPS32_LE: &str = "MIPS/data/languages/mips32le.cspec";
const MIPS64_BE: &str = "MIPS/data/languages/mips64be.cspec";
const MIPS64_LE: &str = "MIPS/data/languages/mips64le.cspec";
const PPC32: &str = "PowerPC/data/languages/ppc_32.cspec";
const PPC64_BE: &str = "PowerPC/data/languages/ppc_64_be.cspec";
const PPC64_LE: &str = "PowerPC/data/languages/ppc_64_le.cspec";

/// Every preset, with the cspec Ghidra's ldefs gives its arch. A kernel
/// flavour takes its userland prototype; x86's `-mregparm=3` takes
/// `__regparm3`, ARM soft-float `__stdcall_softfp`. Ghidra gives a big-endian
/// ELFv2 image `ppc_64_be.cspec`, so ELFv2 is held to both ppc64 files.
const PAIRINGS: &[Pairing] = &[
    Pairing {
        preset: "x86_64_systemv",
        cc: CallingConvention::x86_64_systemv,
        cspec: X86_64_GCC,
        prototype: None,
        arches: &[SleighArch::x86_64],
    },
    Pairing {
        preset: "x86_cdecl",
        cc: CallingConvention::x86_cdecl,
        cspec: X86_GCC,
        prototype: None,
        arches: &[SleighArch::x86],
    },
    Pairing {
        preset: "x86_linux_kernel",
        cc: CallingConvention::x86_linux_kernel,
        cspec: X86_GCC,
        prototype: Some("__regparm3"),
        arches: &[SleighArch::x86],
    },
    Pairing {
        preset: "aarch64_aapcs64",
        cc: CallingConvention::aarch64_aapcs64,
        cspec: AARCH64,
        prototype: None,
        arches: &[SleighArch::aarch64, SleighArch::aarch64be],
    },
    Pairing {
        preset: "arm_aapcs",
        cc: CallingConvention::arm_aapcs,
        cspec: ARM,
        prototype: None,
        arches: &[
            SleighArch::arm,
            SleighArch::arm_be,
            SleighArch::arm_be_kernel,
            SleighArch::arm_thumb,
        ],
    },
    Pairing {
        preset: "arm_aapcs_soft",
        cc: CallingConvention::arm_aapcs_soft,
        cspec: ARM,
        prototype: Some("__stdcall_softfp"),
        arches: &[SleighArch::arm],
    },
    Pairing {
        preset: "mips_o32",
        cc: CallingConvention::mips_o32,
        cspec: MIPS32_BE,
        prototype: None,
        arches: &[SleighArch::mipsbe32],
    },
    Pairing {
        preset: "mips_o32",
        cc: CallingConvention::mips_o32,
        cspec: MIPS32_LE,
        prototype: None,
        arches: &[SleighArch::mipsle32],
    },
    Pairing {
        preset: "mips_n64",
        cc: CallingConvention::mips_n64,
        cspec: MIPS64_BE,
        prototype: None,
        arches: &[SleighArch::mipsbe64],
    },
    Pairing {
        preset: "mips_n64",
        cc: CallingConvention::mips_n64,
        cspec: MIPS64_LE,
        prototype: None,
        arches: &[SleighArch::mipsle64],
    },
    Pairing {
        preset: "powerpc_sysv32",
        cc: CallingConvention::powerpc_sysv32,
        cspec: PPC32,
        prototype: None,
        arches: &[SleighArch::ppc32be, SleighArch::ppc32le],
    },
    Pairing {
        preset: "powerpc_sysv32_linux_kernel",
        cc: CallingConvention::powerpc_sysv32_linux_kernel,
        cspec: PPC32,
        prototype: None,
        arches: &[SleighArch::ppc32be, SleighArch::ppc32le],
    },
    Pairing {
        preset: "powerpc64_elf_v1",
        cc: CallingConvention::powerpc64_elf_v1,
        cspec: PPC64_BE,
        prototype: None,
        arches: &[SleighArch::ppc64be],
    },
    Pairing {
        preset: "powerpc64_elf_v1_linux_kernel",
        cc: CallingConvention::powerpc64_elf_v1_linux_kernel,
        cspec: PPC64_BE,
        prototype: None,
        arches: &[SleighArch::ppc64be],
    },
    Pairing {
        preset: "powerpc64_elf_v2",
        cc: CallingConvention::powerpc64_elf_v2,
        cspec: PPC64_LE,
        prototype: None,
        arches: &[SleighArch::ppc64le],
    },
    Pairing {
        preset: "powerpc64_elf_v2",
        cc: CallingConvention::powerpc64_elf_v2,
        cspec: PPC64_BE,
        prototype: None,
        arches: &[SleighArch::ppc64be],
    },
    Pairing {
        preset: "powerpc64_elf_v2_linux_kernel",
        cc: CallingConvention::powerpc64_elf_v2_linux_kernel,
        cspec: PPC64_LE,
        prototype: None,
        arches: &[SleighArch::ppc64le],
    },
    Pairing {
        preset: "powerpc64_elf_v2_linux_kernel",
        cc: CallingConvention::powerpc64_elf_v2_linux_kernel,
        cspec: PPC64_BE,
        prototype: None,
        arches: &[SleighArch::ppc64be],
    },
];

/// A deliberate difference, and why it stays.
struct Divergence {
    presets: &'static [&'static str],
    items: &'static [&'static str],
    reason: &'static str,
}

const PPC: &[&str] = &[
    "powerpc_sysv32",
    "powerpc_sysv32_linux_kernel",
    "powerpc64_elf_v1",
    "powerpc64_elf_v1_linux_kernel",
    "powerpc64_elf_v2",
    "powerpc64_elf_v2_linux_kernel",
];
const PPC64: &[&str] = &[
    "powerpc64_elf_v1",
    "powerpc64_elf_v1_linux_kernel",
    "powerpc64_elf_v2",
    "powerpc64_elf_v2_linux_kernel",
];

const LEDGER: &[Divergence] = &[
    Divergence {
        presets: PPC,
        items: &["callee-saved only in strider: LR"],
        reason: "the link-register tradeoff: modelled preserved so `InitialVar(LR)` reaches \
                 the return, as Ghidra itself does for ARM `lr` and AArch64 `x30`",
    },
    Divergence {
        presets: &["mips_o32", "mips_n64"],
        items: &["callee-saved only in strider: ra"],
        reason: "the link-register tradeoff, as for PowerPC `LR`",
    },
    Divergence {
        presets: PPC,
        items: &[
            "callee-saved only in strider: vs52",
            "callee-saved only in strider: vs53",
            "callee-saved only in strider: vs54",
            "callee-saved only in strider: vs55",
            "callee-saved only in strider: vs56",
            "callee-saved only in strider: vs57",
            "callee-saved only in strider: vs58",
            "callee-saved only in strider: vs59",
            "callee-saved only in strider: vs60",
            "callee-saved only in strider: vs61",
            "callee-saved only in strider: vs62",
            "callee-saved only in strider: vs63",
        ],
        reason: "v20-v31 are non-volatile under the AltiVec supplement and ELFv2 2.2.1; no \
                 Ghidra PowerPC prototype names a vector register",
    },
    Divergence {
        presets: PPC64,
        items: &["callee-saved only in strider: r13"],
        reason: "r13 is the thread pointer (the paca in the kernel), which a conforming callee \
                 never writes",
    },
    Divergence {
        presets: PPC64,
        items: &["callee-saved only in the cspec: r2Save"],
        reason: "Ghidra's pseudo-register for its `uponreturn` TOC save slot; an sla-internal \
                 register to strider (`internal_registers`)",
    },
    Divergence {
        presets: &["mips_o32"],
        items: &["callee-saved only in strider: f31"],
        reason: "o32 preserves $f20-$f31; Ghidra's list stops at f30",
    },
    Divergence {
        presets: &["arm_aapcs"],
        items: &["callee-saved only in the cspec: pc"],
        reason: "Ghidra marks `pc` unaffected as the return view of `lr`; strider returns \
                 through `lr`",
    },
    Divergence {
        presets: &["aarch64_aapcs64"],
        items: &["argument only in the cspec: x8"],
        reason: "the indirect-result register (Ghidra's `hiddenret`), not an argument; \
                 listing it would renumber every `arg(n)`",
    },
    Divergence {
        presets: &["mips_o32"],
        items: &[
            "argument only in the cspec: f12_13",
            "argument only in the cspec: f14_15",
        ],
        reason: "o32 passes doubles in the f12/f13 and f14/f15 pairs; the preset names the \
                 even halves, which `float_arg_slots` projects onto the pair",
    },
    Divergence {
        presets: &["arm_aapcs", "arm_aapcs_soft"],
        items: &["callee-saved only in strider: cpsr_mode"],
        reason: "CPSR.M, a register only the fork's ARM sla has: `sp` and `lr` are banked \
                 per mode and AAPCS keeps `sp`, so a conforming callee returns in its \
                 caller's mode",
    },
    Divergence {
        presets: &["arm_aapcs_soft"],
        items: &["callee-saved only in strider: lr"],
        reason: "the link-register tradeoff, which Ghidra's own VFP prototype makes",
    },
    Divergence {
        presets: &["arm_aapcs_soft"],
        items: &[
            "callee-saved only in strider: d8",
            "callee-saved only in strider: d9",
            "callee-saved only in strider: d10",
            "callee-saved only in strider: d11",
            "callee-saved only in strider: d12",
            "callee-saved only in strider: d13",
            "callee-saved only in strider: d14",
            "callee-saved only in strider: d15",
        ],
        reason: "AAPCS preserves d8-d15 on a VFP core whatever the float ABI; Ghidra's \
                 `__stdcall_softfp` names no VFP register",
    },
    Divergence {
        presets: &["powerpc64_elf_v2", "powerpc64_elf_v2_linux_kernel"],
        items: &["stack arguments at 112+8n in the cspec, 96+8n in strider"],
        reason: "ELFv2's parameter save area starts at r1+32, so the first stack-only \
                 argument is at 96; both Ghidra ppc64 cspecs carry ELFv1's 112",
    },
];

/// The parts of a `<prototype>` a preset is held to.
struct Prototype {
    stack_pointer: String,
    extrapop: i64,
    /// Register carriers, `join` pieces included.
    inputs: Vec<String>,
    outputs: Vec<String>,
    /// `(offset, align)` of each stack pentry.
    stack: Vec<(i128, i128)>,
    unaffected: Vec<String>,
    killed: Vec<String>,
}

fn prototype(cspec: &str, name: Option<&str>) -> Prototype {
    let text = read(cspec);
    let doc = roxmltree::Document::parse(&text).unwrap_or_else(|e| panic!("{cspec}: {e}"));
    let root = doc.root_element();
    let stack_pointer = attr(
        cspec,
        child(root, "stackpointer").unwrap_or_else(|| panic!("{cspec}: no <stackpointer>")),
        "register",
    );
    let proto = match name {
        None => child(root, "default_proto").and_then(|d| child(d, "prototype")),
        Some(want) => root
            .descendants()
            .find(|n| n.has_tag_name("prototype") && n.attribute("name") == Some(want)),
    }
    .unwrap_or_else(|| panic!("{cspec}: no prototype {name:?}"));
    let (inputs, stack) = carriers(cspec, child(proto, "input"));
    let (outputs, _) = carriers(cspec, child(proto, "output"));
    Prototype {
        stack_pointer,
        extrapop: number(cspec, proto, "extrapop"),
        inputs,
        outputs,
        stack,
        unaffected: registers(cspec, child(proto, "unaffected")),
        killed: registers(cspec, child(proto, "killedbycall")),
    }
}

fn elements<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children().filter(roxmltree::Node::is_element)
}

/// Every `<pentry>` under `list`, `<group>`s included: its register, the
/// pieces of a `join`, or a stack slot. `<rule>`s are skipped; anything else
/// fails rather than vanishing from the comparison.
fn carriers(
    cspec: &str,
    list: Option<roxmltree::Node<'_, '_>>,
) -> (Vec<String>, Vec<(i128, i128)>) {
    let mut pentries = Vec::new();
    for c in list.into_iter().flat_map(elements) {
        match c.tag_name().name() {
            "pentry" => pentries.push(c),
            "group" => pentries.extend(elements(c).inspect(|p| {
                assert!(
                    p.has_tag_name("pentry"),
                    "{cspec}: a <group> holding <{}>, which this test does not read",
                    p.tag_name().name()
                );
            })),
            "rule" => {}
            tag => {
                panic!("{cspec}: a parameter list holding <{tag}>, which this test does not read")
            }
        }
    }
    let mut regs = Vec::new();
    let mut stack = Vec::new();
    for pentry in pentries {
        for c in elements(pentry) {
            match (c.tag_name().name(), c.attribute("space")) {
                ("register", _) => regs.push(attr(cspec, c, "name")),
                ("addr", Some("join")) => regs.extend(
                    c.attributes()
                        .filter(|a| a.name().starts_with("piece"))
                        .map(|a| a.value().to_owned()),
                ),
                ("addr", Some("stack")) => {
                    stack.push((number(cspec, c, "offset"), number(cspec, pentry, "align")));
                }
                (tag, space) => {
                    panic!(
                        "{cspec}: a <pentry> holding <{tag}> in {space:?}, which this test does not read"
                    )
                }
            }
        }
    }
    (regs, stack)
}

/// The `<register>` names under `list`; anything else fails.
fn registers(cspec: &str, list: Option<roxmltree::Node<'_, '_>>) -> Vec<String> {
    let Some(list) = list else {
        return Vec::new();
    };
    list.children()
        .filter(roxmltree::Node::is_element)
        .map(|c| {
            assert!(
                c.has_tag_name("register"),
                "{cspec}: <{}> holds <{}>, which this test does not read",
                list.tag_name().name(),
                c.tag_name().name()
            );
            attr(cspec, c, "name")
        })
        .collect()
}

fn bytes(vn: rsleigh::Vn) -> impl Iterator<Item = u64> {
    (0..u64::from(vn.size)).map(move |i| vn.addr_off + i)
}

fn byte_set(named: &[(String, rsleigh::Vn)]) -> BTreeSet<u64> {
    named.iter().flat_map(|(_, vn)| bytes(*vn)).collect()
}

fn resolve(
    regs: &rsleigh::SleighRegs,
    cspec: &str,
    names: &[String],
) -> Vec<(String, rsleigh::Vn)> {
    names
        .iter()
        .map(|n| {
            let vn = regs
                .name_to_vn(n)
                .unwrap_or_else(|| panic!("{cspec}: {n} is not a register of this arch"));
            (n.clone(), vn)
        })
        .collect()
}

fn named(regs: &rsleigh::SleighRegs, vns: &[rsleigh::Vn]) -> Vec<(String, rsleigh::Vn)> {
    vns.iter()
        .map(|&vn| {
            let name = regs
                .vn_to_name(vn)
                .unwrap_or_else(|| panic!("{vn:?} has no register name"));
            (name.to_owned(), vn)
        })
        .collect()
}

/// The registers in `named` holding a byte of `diff`.
fn holding(named: &[(String, rsleigh::Vn)], diff: &BTreeSet<u64>) -> Vec<String> {
    named
        .iter()
        .filter(|(_, vn)| bytes(*vn).any(|b| diff.contains(&b)))
        .map(|(n, _)| n.clone())
        .collect()
}

/// Adds `pairing`'s divergences on one arch to `seen`, and every register
/// both killed and callee-saved to `killed_and_saved`.
fn check(
    pairing: &Pairing,
    regs: &rsleigh::SleighRegs,
    proto: &Prototype,
    seen: &mut BTreeSet<String>,
    killed_and_saved: &mut BTreeSet<String>,
) {
    let preset = pairing.preset;
    let cc = (pairing.cc)()
        .build(regs)
        .unwrap_or_else(|e| panic!("{preset}: {e:#}"));
    let mut note = |what: String| {
        seen.insert(format!("{preset}: {what}"));
    };

    if regs.name_to_vn(&proto.stack_pointer) != Some(cc.stack_vn) {
        note(format!(
            "stack pointer {} in the cspec",
            proto.stack_pointer
        ));
    }
    if proto.extrapop != cc.ret_stack_pop {
        note(format!(
            "extrapop {} in the cspec, ret_stack_pop {} in strider",
            proto.extrapop, cc.ret_stack_pop
        ));
    }
    let stack = cc.stack_args.map(|s| (s.base_offset, s.increment));
    for &(offset, align) in &proto.stack {
        if Some((offset, align)) != stack {
            let ours = stack.map_or_else(|| "none".to_owned(), |(b, i)| format!("{b}+{i}n"));
            note(format!(
                "stack arguments at {offset}+{align}n in the cspec, {ours} in strider"
            ));
        }
    }

    let unaffected: Vec<(String, rsleigh::Vn)> = resolve(regs, pairing.cspec, &proto.unaffected)
        .into_iter()
        .filter(|(_, vn)| *vn != cc.stack_vn)
        .collect();
    let saved = named(regs, &cc.callee_saved_regs);
    let (theirs, ours) = (byte_set(&unaffected), byte_set(&saved));
    for r in holding(&saved, &(&ours - &theirs)) {
        note(format!("callee-saved only in strider: {r}"));
    }
    for r in holding(&unaffected, &(&theirs - &ours)) {
        note(format!("callee-saved only in the cspec: {r}"));
    }

    let args: Vec<rsleigh::Vn> = cc
        .arg_passing_regs
        .iter()
        .chain(&cc.arg_passing_regs_float)
        .copied()
        .collect();
    let rets: Vec<rsleigh::Vn> = cc
        .ret_val_regs
        .iter()
        .chain(&cc.ret_val_regs_float)
        .copied()
        .collect();
    for (what, cspec_list, ours) in [
        ("argument", &proto.inputs, &args),
        ("return", &proto.outputs, &rets),
    ] {
        let theirs = resolve(regs, pairing.cspec, cspec_list);
        let covered = byte_set(&named(regs, ours));
        for r in holding(&theirs, &(&byte_set(&theirs) - &covered)) {
            note(format!("{what} only in the cspec: {r}"));
        }
    }

    let killed = resolve(regs, pairing.cspec, &proto.killed);
    for r in holding(&killed, &byte_set(&saved)) {
        killed_and_saved.insert(format!("{preset}: {r}"));
    }
}

/// Every preset constructor `CallingConvention` defines has a `PAIRINGS` row.
#[test]
fn every_calling_convention_preset_is_paired() {
    let defined: BTreeSet<&str> = include_str!("../src/calling_convention/mod.rs")
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("pub const fn ")?
                .strip_suffix("() -> Self {")
        })
        .collect();
    let paired: BTreeSet<&str> = PAIRINGS.iter().map(|p| p.preset).collect();
    assert_eq!(
        defined, paired,
        "CallingConvention presets against PAIRINGS"
    );
}

#[test]
fn every_calling_convention_preset_agrees_with_its_ghidra_cspec() {
    let mut tables: HashMap<SleighArch, rsleigh::SleighRegs> = HashMap::new();
    let mut seen = BTreeSet::new();
    let mut killed_and_saved = BTreeSet::new();
    for pairing in PAIRINGS {
        let proto = prototype(pairing.cspec, pairing.prototype);
        for arch in pairing.arches {
            let arch = arch();
            let regs = tables.entry(arch).or_insert_with(|| arch.probe_regs());
            check(pairing, regs, &proto, &mut seen, &mut killed_and_saved);
        }
    }
    assert!(
        killed_and_saved.is_empty(),
        "registers the cspec kills across a call that the preset keeps:\n{}",
        Vec::from_iter(killed_and_saved).join("\n")
    );
    let mut reasons: BTreeMap<String, &str> = BTreeMap::new();
    for row in LEDGER {
        for preset in row.presets {
            for item in row.items {
                let key = format!("{preset}: {item}");
                assert!(
                    reasons.insert(key.clone(), row.reason).is_none(),
                    "{key} is in two ledger rows"
                );
            }
        }
    }
    let unexplained: Vec<&str> = seen
        .iter()
        .filter(|d| !reasons.contains_key(*d))
        .map(String::as_str)
        .collect();
    let stale: Vec<String> = reasons
        .iter()
        .filter(|(d, _)| !seen.contains(*d))
        .map(|(d, why)| format!("{d} ({why})"))
        .collect();
    assert!(
        unexplained.is_empty() && stale.is_empty(),
        "divergences no ledger row explains:\n{}\n\nledger rows that no longer diverge:\n{}",
        unexplained.join("\n"),
        stale.join("\n")
    );
}

/// The ldefs file and Ghidra language id each preset is.
fn language(preset: ArchPreset) -> (&'static str, &'static str) {
    const X86_LDEFS: &str = "x86/data/languages/x86.ldefs";
    const ARM_LDEFS: &str = "ARM/data/languages/ARM.ldefs";
    const AARCH64_LDEFS: &str = "AARCH64/data/languages/AARCH64.ldefs";
    const MIPS_LDEFS: &str = "MIPS/data/languages/mips.ldefs";
    const PPC_LDEFS: &str = "PowerPC/data/languages/ppc.ldefs";
    match preset {
        ArchPreset::X86 => (X86_LDEFS, "x86:LE:32:default"),
        ArchPreset::X86_64 => (X86_LDEFS, "x86:LE:64:default"),
        ArchPreset::Arm => (ARM_LDEFS, "ARM:LE:32:v8"),
        ArchPreset::ArmBe => (ARM_LDEFS, "ARM:BE:32:v8"),
        ArchPreset::ArmBeKernel => (ARM_LDEFS, "ARM:LEBE:32:v8LEInstruction"),
        ArchPreset::ArmThumb => (ARM_LDEFS, "ARM:LE:32:v8T"),
        ArchPreset::Aarch64 => (AARCH64_LDEFS, "AARCH64:LE:64:v8A"),
        ArchPreset::Aarch64Be => (AARCH64_LDEFS, "AARCH64:BE:64:v8A"),
        ArchPreset::MipsBe32 => (MIPS_LDEFS, "MIPS:BE:32:default"),
        ArchPreset::MipsLe32 => (MIPS_LDEFS, "MIPS:LE:32:default"),
        ArchPreset::MipsBe64 => (MIPS_LDEFS, "MIPS:BE:64:default"),
        ArchPreset::MipsLe64 => (MIPS_LDEFS, "MIPS:LE:64:default"),
        ArchPreset::Ppc32Be => (PPC_LDEFS, "PowerPC:BE:32:default"),
        ArchPreset::Ppc32Le => (PPC_LDEFS, "PowerPC:LE:32:default"),
        ArchPreset::Ppc64Be => (PPC_LDEFS, "PowerPC:BE:64:A2ALT"),
        ArchPreset::Ppc64Le => (PPC_LDEFS, "PowerPC:LE:64:A2ALT"),
    }
}

/// The rsleigh constant compiled from an ldefs `slafile`.
fn sla(file: &str) -> rsleigh::sla_spec::SlaSpec {
    use rsleigh::sla_spec::*;
    match file {
        "x86.sla" => SLA_SPEC_X86,
        "x86-64.sla" => SLA_SPEC_X86_64,
        "ARM8_le.sla" => SLA_SPEC_ARM8_LE,
        "ARM8_be.sla" => SLA_SPEC_ARM8_BE,
        "ARM8_be8.sla" => SLA_SPEC_ARM8_BE8,
        "AARCH64.sla" => SLA_SPEC_AARCH64,
        "AARCH64BE.sla" => SLA_SPEC_AARCH64BE,
        "mips32be.sla" => SLA_SPEC_MIPS32BE,
        "mips32le.sla" => SLA_SPEC_MIPS32LE,
        "mips64be.sla" => SLA_SPEC_MIPS64BE,
        "mips64le.sla" => SLA_SPEC_MIPS64LE,
        "ppc_32_be.sla" => SLA_SPEC_PPC_32_BE,
        "ppc_32_le.sla" => SLA_SPEC_PPC_32_LE,
        "ppc_64_isa_altivec_be.sla" => SLA_SPEC_PPC_64_ISA_ALTIVEC_BE,
        "ppc_64_isa_altivec_le.sla" => SLA_SPEC_PPC_64_ISA_ALTIVEC_LE,
        other => panic!("no rsleigh constant mapped for {other}"),
    }
}

/// The rsleigh constant parsed from an ldefs `processorspec`.
fn pspec(file: &str) -> rsleigh::pspec::PSpec {
    use rsleigh::pspec::*;
    match file {
        "x86.pspec" => PSPEC_X86,
        "x86-64.pspec" => PSPEC_X86_64,
        "ARMt.pspec" => PSPEC_ARMT,
        "ARMtTHUMB.pspec" => PSPEC_ARMTTHUMB,
        "AARCH64.pspec" => PSPEC_AARCH64,
        "mips32.pspec" => PSPEC_MIPS32,
        "mips64.pspec" => PSPEC_MIPS64,
        "ppc_32.pspec" => PSPEC_PPC_32,
        "ppc_64.pspec" => PSPEC_PPC_64,
        other => panic!("no rsleigh constant mapped for {other}"),
    }
}

/// Compared by value: `arm_thumb` uses `ARMCortex.pspec`, which sets the same
/// context as Ghidra's `ARMtTHUMB.pspec`, so the two constants are equal.
#[test]
fn every_arch_preset_is_its_ghidra_ldefs_language() {
    let mut wrong = Vec::new();
    for &preset in ArchPreset::ALL {
        let (ldefs, id) = language(preset);
        let text = read(ldefs);
        let doc = roxmltree::Document::parse(&text).unwrap_or_else(|e| panic!("{ldefs}: {e}"));
        let rows: Vec<_> = doc
            .descendants()
            .filter(|n| n.has_tag_name("language") && n.attribute("id") == Some(id))
            .collect();
        let [lang] = rows[..] else {
            panic!("{ldefs}: {} languages {id}", rows.len());
        };
        let arch = preset.arch();
        let slafile = attr(ldefs, lang, "slafile");
        if arch.sla_spec() != sla(&slafile) {
            wrong.push(format!("{preset:?}: its sla is not {id}'s {slafile}"));
        }
        let processorspec = attr(ldefs, lang, "processorspec");
        if arch.pspec() != pspec(&processorspec) {
            wrong.push(format!(
                "{preset:?}: its pspec is not {id}'s {processorspec}"
            ));
        }
        let endian = match attr(ldefs, lang, "endian").as_str() {
            "little" => Endianness::Little,
            "big" => Endianness::Big,
            other => panic!("{ldefs}: {id} has endian {other:?}"),
        };
        if arch.endianness() != endian {
            wrong.push(format!(
                "{preset:?}: data is not {endian:?}-endian as {id} says"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
