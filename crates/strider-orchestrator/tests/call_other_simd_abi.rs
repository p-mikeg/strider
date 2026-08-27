//! SIMD / crypto user-ops: they must lift at all, and the ones the sla proves
//! are register-to-register must not sit on the memory chain.

use rsleigh::mem_readers::BufMemReader;
use strider_ir::IRViewer;
use strider_pattern::{Matcher, call_other};

struct Lifted {
    function: strider_ir::Function,
}

impl Lifted {
    /// The named `CallOther`'s memory output, `None` if the op is absent.
    fn call_other_mem_out(&self, name: &str) -> Option<strider_ir::node::ValueId> {
        let pat = call_other().name(name).build();
        let m = Matcher::new(&self.function).find_all(&pat).unwrap();
        let node = m.first()?.root();
        // Outputs: [ctrl, mem, ...values].
        Some(self.function.node_outputs(node)[1])
    }

    fn assert_pure(&self, name: &str) {
        let mem = self
            .call_other_mem_out(name)
            .unwrap_or_else(|| panic!("{name}: no CallOther node in the lifted function"));
        let users: Vec<_> = self.function.value_uses(mem).collect();
        assert!(
            users.is_empty(),
            "{name}: PURE means the memory edge does not advance; got users {users:?}"
        );
    }

    fn assert_clobbers_memory(&self, name: &str) {
        let mem = self
            .call_other_mem_out(name)
            .unwrap_or_else(|| panic!("{name}: no CallOther node in the lifted function"));
        assert!(
            self.function.value_uses(mem).next().is_some(),
            "{name}: must advance the memory edge"
        );
    }
}

fn lift(
    arch: strider_target::SleighArch,
    cc: strider_target::CallingConvention,
    bytes: Vec<u8>,
) -> anyhow::Result<Lifted> {
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let mut h = strider_orchestrator::Lifter::new(arch, sleigh)?;
    let cc = cc.build(h.sleigh_regs())?;
    let cfg = h.build_cfg(
        strider_cfg::MachineInsnAddr::from(entry),
        &strider_cfg::CfgOptions::default(),
        &Default::default(),
    )?;
    let outcome = h.build_ir(&cfg, cc)?;
    Ok(Lifted {
        function: outcome.function,
    })
}

fn lift_x86_64(bytes: Vec<u8>) -> anyhow::Result<Lifted> {
    lift(
        strider_target::SleighArch::x86_64(),
        strider_target::CallingConvention::x86_64_systemv(),
        bytes,
    )
}

fn lift_aarch64(bytes: Vec<u8>) -> anyhow::Result<Lifted> {
    lift(
        strider_target::SleighArch::aarch64(),
        strider_target::CallingConvention::aarch64_aapcs64(),
        bytes,
    )
}

#[test]
fn x86_aesenc_lifts_and_is_pure() {
    // aesenc xmm0, xmm1 ; ret
    let f = lift_x86_64(vec![0x66, 0x0f, 0x38, 0xdc, 0xc1, 0xc3]).expect("aesenc must lift");
    f.assert_pure("aesenc");
}

#[test]
fn x86_crc32_lifts_and_is_pure() {
    // crc32 eax, ecx ; ret
    let f = lift_x86_64(vec![0xf2, 0x0f, 0x38, 0xf1, 0xc1, 0xc3]).expect("crc32 must lift");
    f.assert_pure("crc32");
}

#[test]
fn aarch64_neon_aese_lifts_and_is_pure() {
    // aese v0.16b, v1.16b ; ret
    let f = lift_aarch64(vec![0x20, 0x48, 0x28, 0x4e, 0xc0, 0x03, 0x5f, 0xd6])
        .expect("NEON_aese must lift");
    f.assert_pure("NEON_aese");
}

#[test]
fn aarch64_sve_ldr_lifts_and_clobbers_memory() {
    // ldr z0, [x0] ; ret -- the sla passes the BASE REGISTER, emitting no
    // p-code LOAD, so the access is implicit and must stay on the memory chain.
    let f = lift_aarch64(vec![0x00, 0x40, 0x80, 0x85, 0xc0, 0x03, 0x5f, 0xd6])
        .expect("SVE_ldr must lift");
    f.assert_clobbers_memory("SVE_ldr");
}
