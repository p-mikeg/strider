//! Shared setup for the `dump_per_region` / `dump_neighborhood`
//! integration tests.
//!
//! Both tests lift the same minimal `ret` snippet under x86_64,
//! allocate a per-test temp dir, and assert the same vendored
//! viewer-JSON script.  Extracted here so adding a new dump-helper
//! integration test (or refining the assertion) updates one place.

#![allow(dead_code)] // helpers used selectively by the dump tests

use rsleigh::mem_readers::BufMemReader;
use rsleigh::Sleigh;
use std::path::PathBuf;
use strider_analyze::AnalyzeOutcome;
use strider_lift::cfg::{Builder, Cfg, OptionsBuilder};
use strider_target::SleighArch;

use super::strider_x86_64;

/// Build an x86_64 [`Cfg`] for the trivial single-region snippet
/// `0xc3` (`ret`), then run `analyze_cfg` on it.  Returns the lifted
/// outcome alongside the original CFG (which owns the Sleigh handle
/// that the dump helpers need for register name labelling).
pub fn lift_ret_snippet_x86_64() -> (AnalyzeOutcome, Cfg<BufMemReader<Vec<u8>>>) {
    let strider = strider_x86_64();
    let arch = SleighArch::x86_64();

    let bytes = vec![0xc3u8]; // ret
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let cfg = Builder::for_arch(&arch, sleigh, entry, OptionsBuilder::new().build())
        .build()
        .expect("cfg");

    let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
    (outcome, cfg)
}

/// Allocate a per-test scratch directory under the system temp dir
/// (no `tempfile` crate dependency).  The directory name embeds the
/// process id + a nanosecond clock so concurrent test invocations do
/// not collide.
pub fn unique_tmp_dir(tag: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "strider-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp).expect("create tmp dir");
    tmp
}

/// The vendored viewer's JSON-payload anchor string.  Both dump
/// helpers embed the DOT source inside a
/// `<script type="application/json" id="dot-src">` element; finding
/// this substring in the output HTML is sufficient evidence that the
/// render path completed (vs. an empty / partial file).
pub const VIEWER_JSON_ANCHOR: &str =
    "<script type=\"application/json\" id=\"dot-src\">";
