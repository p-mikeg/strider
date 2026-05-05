//! Integration tests for v2's precise per-op ABI lifting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use pattern::{Matcher, call_other};
use rsleigh::mem_readers::BufMemReader;

#[test]
fn cpuid_clobbers_only_eax_ebx_ecx_edx() {
    let arch = strider::SleighArch::x86_64();
    let probe = BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe regs");
    let strider_h = strider::Strider::new(
        arch,
        regs,
        strider::CallingConvention::x86_64_systemv_abi(),
    )
    .expect("strider");

    // Bytes: cpuid (0x0F 0xA2) ; ret (0xC3)
    let bytes = vec![0x0fu8, 0xa2, 0xc3];
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh");
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .expect("cfg");
    let outcome = strider_h.analyze_cfg(&cfg).expect("analyze_cfg");

    // Find a cpuid* CallOther.  Sleigh's lift selects one of
    // cpuid / cpuid_<leaf>_info based on EAX; with no EAX setup it
    // falls through to cpuid_brand_part3_info.  Any cpuid* user-op
    // is acceptable here - we're testing the precise-ABI shape.
    let cpuid_names = [
        "cpuid",
        "cpuid_basic_info",
        "cpuid_Version_info",
        "cpuid_cache_tlb_info",
        "cpuid_serial_info",
        "cpuid_Deterministic_Cache_Parameters_info",
        "cpuid_MONITOR_MWAIT_Features_info",
        "cpuid_Thermal_Power_Management_info",
        "cpuid_Extended_Feature_Enumeration_info",
        "cpuid_Direct_Cache_Access_info",
        "cpuid_Architectural_Performance_Monitoring_info",
        "cpuid_Extended_Topology_info",
        "cpuid_Processor_Extended_States_info",
        "cpuid_Quality_of_Service_info",
        "cpuid_brand_part1_info",
        "cpuid_brand_part2_info",
        "cpuid_brand_part3_info",
    ];
    let mut found_node: Option<ir::node::NodeId> = None;
    let mut found_name: Option<&'static str> = None;
    for n in cpuid_names {
        let pat = pattern::Pat::from(call_other().name(n));
        let matches = Matcher::new(&outcome.graph).find_all(&pat);
        if let Some(m) = matches.first() {
            found_node = Some(m.root());
            found_name = Some(n);
            break;
        }
    }
    let node = found_node.expect("a cpuid* CallOther exists in this fixture");
    let name = found_name.expect("name");

    // Outputs: [ctrl, mem, value(tmpptr)].  Empty implicit_writes
    // (Sleigh handles register writes via subsequent Loads from the
    // returned tmpptr).  3 outputs total.
    let n_outs = outcome.graph.graph.node_outputs(node).len();
    assert_eq!(
        n_outs, 3,
        "{name} CallOther: ctrl + mem + value (tmpptr); got {n_outs}"
    );
}

#[test]
fn unmodelled_sysreg_read_clobbers_only_destination() {
    let arch = strider::SleighArch::aarch64();
    let probe = BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe regs");
    let strider_h = strider::Strider::new(
        arch,
        regs,
        strider::CallingConvention::aarch64_aapcs64(),
    )
    .expect("strider");

    // mrs x0, S3_5_C15_C0_7 (encoding: 0xD53DF0E0 LE = E0 F0 3D D5)
    // Followed by ret (0xD65F03C0 LE = C0 03 5F D6)
    let bytes = vec![0xe0u8, 0xf0, 0x3d, 0xd5, 0xc0, 0x03, 0x5f, 0xd6];
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh");
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .expect("cfg");
    let outcome = strider_h.analyze_cfg(&cfg).expect("analyze_cfg");

    let pat = pattern::Pat::from(call_other().name("UnkSytemRegRead"));
    let matches = Matcher::new(&outcome.graph).find_all(&pat);
    assert_eq!(
        matches.len(),
        1,
        "exactly one UnkSytemRegRead in this fixture"
    );
    let node = matches[0].root();

    // Outputs: [ctrl, mem, value(x0)].  No implicit clobbers.
    // 3 outputs total.
    let n_outs = outcome.graph.graph.node_outputs(node).len();
    assert_eq!(
        n_outs, 3,
        "UnkSytemRegRead CallOther: ctrl + mem + value (x0); got {n_outs}"
    );
}
