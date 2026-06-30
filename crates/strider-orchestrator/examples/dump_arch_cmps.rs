//! Dumps p-code lifts for `cmp`-then-conditional-branch sequences across
//! every flag-architecture Strider supports.  Used to design the
//! `FlagCmpCanonicalize` pass's per-arch rule set: each arch's
//! `cmp`+cond emits a fixed boolean tree of named flag varnodes that
//! can then be matched and rewritten.
//!
//! Run: `cargo run -p strider --example dump_arch_cmps`

use rsleigh::mem_readers::VecMemReader;
use rsleigh::{Insn, MemReader, Sleigh, SleighRegs, Vn, VnSpace};
use strider_target::SleighArch;

const BASE: u64 = 0x1000;

struct Sample {
    label: String,
    bytes: Vec<u8>,
}

/// All AArch64 (LE) `cmp w0, #5; b.<cond> +4` sequences.
fn aarch64_samples() -> (SleighArch, Vec<Sample>) {
    fn b_cond(cond4: u8) -> u32 {
        // 0101_0100 imm19=1 0 cond4
        0x5400_0000u32 | (1u32 << 5) | u32::from(cond4 & 0xf)
    }
    let cmp = 0x7100_141fu32; // cmp w0, #5  (subs wzr, w0, #5)
    let mut samples = Vec::new();
    samples.push(Sample {
        label: "cmp w0, #5".into(),
        bytes: cmp.to_le_bytes().to_vec(),
    });
    let codes = [
        ("EQ", 0b0000),
        ("NE", 0b0001),
        ("CS/HS", 0b0010),
        ("CC/LO", 0b0011),
        ("MI", 0b0100),
        ("PL", 0b0101),
        ("VS", 0b0110),
        ("VC", 0b0111),
        ("HI", 0b1000),
        ("LS", 0b1001),
        ("GE", 0b1010),
        ("LT", 0b1011),
        ("GT", 0b1100),
        ("LE", 0b1101),
    ];
    for (name, code) in codes {
        let enc = b_cond(code);
        samples.push(Sample {
            label: format!("b.{name} +4"),
            bytes: enc.to_le_bytes().to_vec(),
        });
    }
    (SleighArch::aarch64(), samples)
}

/// x86_64 `cmp rax, rbx; jcc +0` for every 1-byte-rel8 cond.
fn x86_64_samples() -> (SleighArch, Vec<Sample>) {
    // 48 39 D8 — cmp rax, rbx (REX.W + 39 /r, ModR/M=mod3 reg=rbx rm=rax).
    let cmp = [0x48u8, 0x39, 0xD8];
    let mut samples = Vec::new();
    samples.push(Sample {
        label: "cmp rax, rbx".into(),
        bytes: cmp.to_vec(),
    });
    let codes: &[(&str, u8)] = &[
        ("JO", 0x70),
        ("JNO", 0x71),
        ("JB/JNAE/JC", 0x72),
        ("JAE/JNB/JNC", 0x73),
        ("JE/JZ", 0x74),
        ("JNE/JNZ", 0x75),
        ("JBE/JNA", 0x76),
        ("JA/JNBE", 0x77),
        ("JS", 0x78),
        ("JNS", 0x79),
        ("JP", 0x7A),
        ("JNP", 0x7B),
        ("JL/JNGE", 0x7C),
        ("JGE/JNL", 0x7D),
        ("JLE/JNG", 0x7E),
        ("JG/JNLE", 0x7F),
    ];
    for &(name, op) in codes {
        samples.push(Sample {
            label: format!("{name} +0"),
            bytes: vec![op, 0x00],
        });
    }
    (SleighArch::x86_64(), samples)
}

/// x86 (32-bit) `cmp eax, ebx; jcc +0`.
fn x86_samples() -> (SleighArch, Vec<Sample>) {
    let cmp = [0x39u8, 0xD8]; // cmp eax, ebx
    let mut samples = Vec::new();
    samples.push(Sample {
        label: "cmp eax, ebx".into(),
        bytes: cmp.to_vec(),
    });
    let codes: &[(&str, u8)] = &[
        ("JO", 0x70),
        ("JNO", 0x71),
        ("JB", 0x72),
        ("JAE", 0x73),
        ("JE", 0x74),
        ("JNE", 0x75),
        ("JBE", 0x76),
        ("JA", 0x77),
        ("JS", 0x78),
        ("JNS", 0x79),
        ("JL", 0x7C),
        ("JGE", 0x7D),
        ("JLE", 0x7E),
        ("JG", 0x7F),
    ];
    for &(name, op) in codes {
        samples.push(Sample {
            label: format!("{name} +0"),
            bytes: vec![op, 0x00],
        });
    }
    (SleighArch::x86(), samples)
}

/// ARM (LE 32-bit) `cmp r0, r1; b.<cond> +0`.
fn arm_samples() -> (SleighArch, Vec<Sample>) {
    // cmp r0, r1: cond=AL(1110) 00 010101 Rn=0000 SBZ=0000 oper2=001
    // = 1110 0001 0101 0000 0000 0000 0000 0001 = 0xE1500001
    let cmp = 0xE150_0001u32;
    let mut samples = Vec::new();
    samples.push(Sample {
        label: "cmp r0, r1".into(),
        bytes: cmp.to_le_bytes().to_vec(),
    });
    // B.cond imm24=0:  cond 1010 0000_0000_0000_0000_0000_0000
    let codes: &[(&str, u8)] = &[
        ("BEQ", 0b0000),
        ("BNE", 0b0001),
        ("BCS/BHS", 0b0010),
        ("BCC/BLO", 0b0011),
        ("BMI", 0b0100),
        ("BPL", 0b0101),
        ("BVS", 0b0110),
        ("BVC", 0b0111),
        ("BHI", 0b1000),
        ("BLS", 0b1001),
        ("BGE", 0b1010),
        ("BLT", 0b1011),
        ("BGT", 0b1100),
        ("BLE", 0b1101),
    ];
    for &(name, code) in codes {
        let enc = (u32::from(code) << 28) | 0x0A00_0000;
        samples.push(Sample {
            label: format!("{name} +0"),
            bytes: enc.to_le_bytes().to_vec(),
        });
    }
    (SleighArch::arm(), samples)
}

/// ARM Thumb (LE) `cmp r0, r1; b.<cond> +0`.
fn arm_thumb_samples() -> (SleighArch, Vec<Sample>) {
    // CMP r0, r1 (T1, low regs): 0100_0010_1000_1000 = 0x4288
    let cmp = 0x4288u16;
    let mut samples = Vec::new();
    samples.push(Sample {
        label: "cmp r0, r1".into(),
        bytes: cmp.to_le_bytes().to_vec(),
    });
    // B<cond> T1 (imm8): 1101_cond4_imm8
    let codes: &[(&str, u8)] = &[
        ("BEQ", 0b0000),
        ("BNE", 0b0001),
        ("BCS", 0b0010),
        ("BCC", 0b0011),
        ("BMI", 0b0100),
        ("BPL", 0b0101),
        ("BVS", 0b0110),
        ("BVC", 0b0111),
        ("BHI", 0b1000),
        ("BLS", 0b1001),
        ("BGE", 0b1010),
        ("BLT", 0b1011),
        ("BGT", 0b1100),
        ("BLE", 0b1101),
    ];
    for &(name, code) in codes {
        let enc: u16 = 0xD000u16 | (u16::from(code) << 8);
        samples.push(Sample {
            label: format!("{name} +0"),
            bytes: enc.to_le_bytes().to_vec(),
        });
    }
    (SleighArch::arm_thumb(), samples)
}

/// MIPS (32-bit, BE) — has no flag register.  Show what the `slt`-then-`bnez`
/// idiom and the direct register-comparison branches lift to.
fn mips_samples() -> (SleighArch, Vec<Sample>) {
    let mut samples = Vec::new();
    // slt $4, $5, $6 — set $4 to 1 if $5 < $6 signed (R-type, op=0, funct=0x2A)
    //   bits: 000000 00101 00110 00100 00000 101010
    let slt = 0x00A6_202Au32;
    samples.push(Sample {
        label: "slt $4, $5, $6".into(),
        bytes: slt.to_be_bytes().to_vec(),
    });
    // sltu $4, $5, $6 — funct=0x2B
    let sltu = 0x00A6_202Bu32;
    samples.push(Sample {
        label: "sltu $4, $5, $6".into(),
        bytes: sltu.to_be_bytes().to_vec(),
    });
    // MIPS branches have delay slots; Sleigh requires the next insn to be
    // decodable.  Append `nop` (= 0x00000000) bytes after each branch so the
    // delay-slot fetch succeeds.
    let nop = 0u32.to_be_bytes().to_vec();
    let mut push_branch = |label: &str, enc: u32| {
        let mut b = enc.to_be_bytes().to_vec();
        b.extend_from_slice(&nop);
        samples.push(Sample {
            label: format!("{label} (+ delay-slot nop)"),
            bytes: b,
        });
    };
    push_branch("beq $2, $3, +4", 0x1043_0001);
    push_branch("bne $2, $3, +4", 0x1443_0001);
    push_branch("bgez $2, +4 (op=1 rt=1)", 0x0441_0001);
    push_branch("bltz $2, +4 (op=1 rt=0)", 0x0440_0001);
    (SleighArch::mipsbe32(), samples)
}

/// PPC (32-bit, BE).  cmp + bc-form conditional branches.
fn ppc_samples() -> (SleighArch, Vec<Sample>) {
    let mut samples = Vec::new();
    // cmpw cr0, r3, r4 — primary=31, BF=0, L=0, RA=3, RB=4, secondary=0
    //   = 011111 000 0 0 00011 00100 0000000000 0
    //   word: 0111_1100_0001_1000_0010_0000_0000_0000 = 0x7C18_2000  (let me re-derive)
    //   Actually: 011111(0x1F) 000 0 0 00011 00100 0000000000 0
    //     bits 0-5: 011111
    //     bits 6-8: 000
    //     bit 9:    0
    //     bit 10:   0
    //     bits 11-15: 00011
    //     bits 16-20: 00100
    //     bits 21-30: 0000000000
    //     bit 31:   0
    //   Concatenated MSB→LSB: 011111_000_0_0_00011_00100_0000000000_0
    //     = 0111 1100 0000 0001 1001 0000 0000 0000
    //   That's 0x7C019000 — hmm, let me verify with a known reference.
    //
    //   Actually `cmpw r3, r4` on PPC is well-documented as 0x7C03_2000.
    //   Encoding decode for 0x7C032000: 0111 1100 0000 0011 0010 0000 0000 0000
    //     bits 0-5: 011111 ✓
    //     bits 6-8: 000 (BF=0)
    //     bit 9:    0
    //     bit 10:   0 (L=0)
    //     bits 11-15: 00011 = 3 ✓
    //     bits 16-20: 00100 = 4 ✓
    //     bits 21-30: 0000000000 = 0 (cmp subop) ✓
    //     bit 31:   0 ✓
    //   Yes, 0x7C032000 is correct.
    let cmpw = 0x7C03_2000u32;
    samples.push(Sample {
        label: "cmpw cr0, r3, r4".into(),
        bytes: cmpw.to_be_bytes().to_vec(),
    });
    // bc 12, 2, +4 (= beq cr0, +4): primary=16, BO=12 (01100), BI=2, BD=1, AA=0, LK=0
    //   = 010000 01100 00010 00000000000001 0 0
    //   = 0100 0001 1000 0010 0000 0000 0000 0100
    //   = 0x41820004
    let beq = 0x4182_0004u32;
    samples.push(Sample {
        label: "beq cr0, +4".into(),
        bytes: beq.to_be_bytes().to_vec(),
    });
    // bne cr0, +4: BO=4 (00100), BI=2 → 010000 00100 00010 00000000000001 0 0
    //   = 0100 0000 1000 0010 0000 0000 0000 0100 = 0x40820004
    let bne = 0x4082_0004u32;
    samples.push(Sample {
        label: "bne cr0, +4".into(),
        bytes: bne.to_be_bytes().to_vec(),
    });
    // blt cr0, +4: BO=12, BI=0 → 010000 01100 00000 00000000000001 0 0 = 0x41800004
    let blt = 0x4180_0004u32;
    samples.push(Sample {
        label: "blt cr0, +4".into(),
        bytes: blt.to_be_bytes().to_vec(),
    });
    // bgt cr0, +4: BO=12, BI=1 → 010000 01100 00001 00000000000001 0 0 = 0x41810004
    let bgt = 0x4181_0004u32;
    samples.push(Sample {
        label: "bgt cr0, +4".into(),
        bytes: bgt.to_be_bytes().to_vec(),
    });
    // bge cr0, +4: BO=4, BI=0 → 0x40800004
    let bge = 0x4080_0004u32;
    samples.push(Sample {
        label: "bge cr0, +4".into(),
        bytes: bge.to_be_bytes().to_vec(),
    });
    // ble cr0, +4: BO=4, BI=1 → 0x40810004
    let ble = 0x4081_0004u32;
    samples.push(Sample {
        label: "ble cr0, +4".into(),
        bytes: ble.to_be_bytes().to_vec(),
    });
    (SleighArch::ppc32be(), samples)
}

// ── Pretty-printer ────────────────────────────────────────────────────────

fn fmt_vn<R: MemReader>(sleigh: &Sleigh<R>, regs: &SleighRegs, vn: Vn) -> String {
    if vn.addr_space == VnSpace::CONST {
        return format!("#{:#x}:{}", vn.addr_off, vn.size);
    }
    if vn.addr_space == VnSpace::REGISTER
        && let Some(name) = regs.vn_to_name(vn)
    {
        return format!("{name}:{}", vn.size);
    }
    if let Some(info) = sleigh.space_info(vn.addr_space) {
        let label = info.name().unwrap_or("?");
        return format!("{label}({:#x}):{}", vn.addr_off, vn.size);
    }
    format!(
        "{}({:#x}):{}",
        vn.addr_space.shortcut(),
        vn.addr_off,
        vn.size
    )
}

fn fmt_insn<R: MemReader>(sleigh: &Sleigh<R>, regs: &SleighRegs, insn: &Insn) -> String {
    let mut s = String::new();
    if let Some(out) = insn.output {
        s.push_str(&fmt_vn(sleigh, regs, out));
        s.push_str(" = ");
    }
    s.push_str(&format!("{:?}", insn.opcode));
    if !insn.inputs.is_empty() {
        s.push('(');
        for (i, vn) in insn.inputs.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&fmt_vn(sleigh, regs, *vn));
        }
        s.push(')');
    }
    s
}

fn dump_arch(arch_label: &str, arch: SleighArch, samples: Vec<Sample>) -> anyhow::Result<()> {
    println!();
    println!("################################################################");
    println!("# {arch_label}");
    println!("################################################################");

    let mut buf: Vec<u8> = Vec::new();
    let mut starts: Vec<u64> = Vec::new();
    for s in &samples {
        starts.push(BASE + buf.len() as u64);
        buf.extend_from_slice(&s.bytes);
    }
    let reader = VecMemReader::new(buf, BASE);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader)?;
    let regs = sleigh.regs()?;

    for (s, &addr) in samples.iter().zip(starts.iter()) {
        let bytes_hex: String = s
            .bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!("\n{:<24}@ {addr:#x}    bytes: {bytes_hex}", s.label);
        println!("----------------------------------------------------------------");
        let res = sleigh.lift_one(addr)?;
        for (i, insn) in res.insns.iter().enumerate() {
            println!("  [{i}] {}", fmt_insn(&sleigh, &regs, insn));
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let (a, s) = aarch64_samples();
    dump_arch("AArch64 (LE)", a, s)?;
    let (a, s) = x86_64_samples();
    dump_arch("x86_64", a, s)?;
    let (a, s) = x86_samples();
    dump_arch("x86 (32-bit)", a, s)?;
    let (a, s) = arm_samples();
    dump_arch("ARM (LE 32-bit)", a, s)?;
    let (a, s) = arm_thumb_samples();
    dump_arch("ARM Thumb (LE)", a, s)?;
    let (a, s) = mips_samples();
    dump_arch("MIPS (BE 32-bit)", a, s)?;
    let (a, s) = ppc_samples();
    dump_arch("PowerPC (BE 32-bit)", a, s)?;
    Ok(())
}
