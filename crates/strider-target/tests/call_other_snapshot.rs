//! Per-preset snapshot of how every user-op the preset's sla declares
//! classifies. `UPDATE_EXPECT=1 cargo test -p strider-target --test
//! call_other_snapshot` re-blesses. The `UNCLASSIFIED` lines are the ops this
//! arch's sla can emit that the tables do not yet answer: the to-do list for
//! a new arch.

use expect_test::expect_file;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use strider_target::ArchPreset;
use strider_target::call_other_abi::{
    CallOtherClass, classify, is_msr_register_mode, is_rounding_mode_control,
    rounding_mode_register,
};

fn render(preset: ArchPreset) -> String {
    let arch = preset.arch();
    let names: BTreeSet<&str> = arch
        .decode_tables()
        .user_op_names
        .iter()
        .map(String::as_str)
        .collect();
    let mut out = String::new();
    for name in names {
        let class = if is_rounding_mode_control(preset, name) {
            let mut s = "lowered:rounding".to_owned();
            if let Some(r) = rounding_mode_register(preset, name) {
                write!(s, " reg={r}").unwrap();
            }
            s
        } else if is_msr_register_mode(preset, name) {
            "lowered:msr_mode".to_owned()
        } else {
            match classify(preset, name) {
                None => "UNCLASSIFIED".to_owned(),
                Some(CallOtherClass::NoOp) => "noop".to_owned(),
                Some(CallOtherClass::Call(a)) => format!(
                    "call reads={:?} writes={:?} mem={} noret={}",
                    a.implicit_reads, a.implicit_writes, a.clobbers_memory, a.no_return
                ),
            }
        };
        writeln!(out, "{name} {class}").unwrap();
    }
    out
}

#[test]
fn call_other_classification_snapshot() {
    for &preset in ArchPreset::ALL {
        let path = format!("snapshots/call_other/{}.txt", preset.name());
        expect_file![path].assert_eq(&render(preset));
    }
}
