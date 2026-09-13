//! Every DOT and HTML render of an analysed function is a function of the
//! bytes alone. Two engines alive at once hold their Sleigh `AddrSpace`s at
//! different host addresses, so whatever leaks one into a label differs here.

mod common;

use dot::{DotStyle, GraphDot};
use object::{Object, ObjectSymbol};
use strider_orchestrator::opt::{OptOptions, ReadOnlyMemory};
use strider_orchestrator::{AnalyzeResult, LiftOptions, Strider};

fn renders<R: rsleigh::MemReader>(
    strider: &Strider<R>,
    result: &AnalyzeResult,
) -> Vec<(&'static str, String)> {
    let sleigh = strider.sleigh();
    let (cfg, function) = (&result.cfg, &result.function);
    let cfg_dot = GraphDot::new(cfg.dot_dumper(sleigh), DotStyle::dark_cfg());
    let fn_dot = GraphDot::new(
        function.dot_dumper(sleigh).expect("dot_dumper"),
        DotStyle::dark(),
    );
    let entry = function.entry();
    vec![
        ("cfg dot", cfg_dot.as_dot().expect("cfg dot")),
        ("cfg html", cfg_dot.as_html_from_dot().expect("cfg html")),
        (
            "cfg neighborhood",
            cfg.neighborhood_dot(sleigh, cfg.entry(), 5, 60)
                .expect("cfg neighborhood"),
        ),
        ("function dot", fn_dot.as_dot().expect("function dot")),
        (
            "function html",
            fn_dot.as_html_from_dot().expect("function html"),
        ),
        (
            "function neighborhood",
            function
                .dot_dumper(sleigh)
                .expect("dot_dumper")
                .neighborhood_dot(entry, 5, 32, 60, false)
                .expect("function neighborhood"),
        ),
        ("raw dot", function.raw_dot().expect("raw dot")),
        ("raw html", function.raw_html().expect("raw html")),
        (
            "raw neighborhood",
            function
                .raw_neighborhood_dot(entry, 5, 32, 60, false)
                .expect("raw neighborhood"),
        ),
    ]
}

fn assert_renders_identical_across_engines(arch: common::Arch, case: &str, fn_name: &str) {
    let path = common::binary_path(arch, case);
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.checked_file().expect("the mapped file is unchanged");
    let addr = obj.symbol_by_name(fn_name).expect("symbol").address();
    let analyse = || {
        let sa = arch.sleigh();
        let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem");
        let sleigh = rsleigh::Sleigh::new(sa.sla_spec(), sa.pspec(), mem).expect("sleigh");
        let cc = arch.cc().build(&sleigh.regs().expect("regs")).expect("cc");
        let rom: Box<dyn ReadOnlyMemory> =
            Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));
        let mut strider = Strider::new(sa, sleigh, Some(rom)).expect("Strider::new");
        let result = strider
            .analyze(
                addr,
                &cc,
                &LiftOptions::default(),
                &OptOptions::default(),
                None,
            )
            .expect("analyze");
        (strider, result)
    };
    let (strider_a, result_a) = analyse();
    let (strider_b, result_b) = analyse();
    let a = renders(&strider_a, &result_a);
    let b = renders(&strider_b, &result_b);
    for ((name, a), (_, b)) in a.iter().zip(&b) {
        let first_diff = a.lines().zip(b.lines()).find(|(x, y)| x != y);
        assert!(
            a == b,
            "{case}::{fn_name} {name} differs between two engines: {first_diff:?}"
        );
    }
}

#[test]
fn x86_64_memory_renders_are_identical_across_engines() {
    assert_renders_identical_across_engines(common::Arch::X64, "memory", "array_copy");
}

#[test]
fn aarch64_switch_renders_are_identical_across_engines() {
    assert_renders_identical_across_engines(common::Arch::Aarch64, "switch", "dispatch_value");
}
