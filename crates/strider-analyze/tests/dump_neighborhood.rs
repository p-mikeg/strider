//! Integration test: `strider_analyze::dump_neighborhood` emits an HTML
//! viewer for the subgraph within N hops of an anchor node.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::mem_readers::BufMemReader;
use rsleigh::Sleigh;
use strider_lift::cfg::{Builder, OptionsBuilder};
use strider_target::SleighArch;

mod common;

#[test]
fn dump_neighborhood_writes_one_html_for_the_anchor() {
    let strider = common::strider_x86_64();
    let arch = SleighArch::x86_64();

    let bytes = vec![0xc3u8]; // ret
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let cfg = Builder::for_arch(&arch, sleigh, entry, OptionsBuilder::new().build())
        .build()
        .expect("cfg");

    let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
    let entry_node = outcome.graph.entry().expect("entry should be set after analyze_cfg");

    let tmp = std::env::temp_dir().join(format!(
        "strider-dump-neighborhood-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp).expect("create tmp dir");
    let out = tmp.join("focus.html");

    strider_analyze::dump_neighborhood(
        &outcome.graph,
        entry_node,
        /* depth */ 1,
        cfg.sleigh(),
        &out,
    )
    .expect("dump_neighborhood");

    let html = std::fs::read_to_string(&out).expect("read html");
    assert!(
        html.contains("<script type=\"application/json\" id=\"dot-src\">"),
        "viewer JSON script missing from {}",
        out.display()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
