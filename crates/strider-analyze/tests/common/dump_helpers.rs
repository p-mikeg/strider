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
use std::path::{Path, PathBuf};
use strider_analyze::AnalyzeOutcome;
use strider_lift::cfg::{Builder, Cfg, OptionsBuilder};
use strider_target::SleighArch;

use super::strider_x86_64;

/// Shared scaffold: assemble `bytes` as an x86_64 snippet at entry
/// `0x1000`, build a CFG via `Builder::for_arch`, then run `analyze_cfg`.
/// All three `lift_*_snippet_x86_64` helpers are thin wrappers that
/// differ only in the bytes vector.
fn lift_x86_64_bytes(bytes: Vec<u8>) -> (AnalyzeOutcome, Cfg<BufMemReader<Vec<u8>>>) {
    let strider = strider_x86_64();
    let arch = SleighArch::x86_64();

    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let cfg = Builder::for_arch(&arch, sleigh, entry, OptionsBuilder::new().build())
        .build()
        .expect("cfg");

    let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
    (outcome, cfg)
}

/// Build an x86_64 [`Cfg`] for the trivial single-region snippet
/// `0xc3` (`ret`), then run `analyze_cfg` on it.  Returns the lifted
/// outcome alongside the original CFG (which owns the Sleigh handle
/// that the dump helpers need for register name labelling).
pub fn lift_ret_snippet_x86_64() -> (AnalyzeOutcome, Cfg<BufMemReader<Vec<u8>>>) {
    lift_x86_64_bytes(vec![0xc3u8]) // ret
}

/// Build an x86_64 [`Cfg`] for a tiny conditional-branch snippet that
/// fans into two regions, each ending in `ret`:
///
/// ```text
///   1000:  48 85 c0          test rax, rax
///   1003:  74 01             jz   0x1006        ; taken    → region B
///   1005:  c3                ret                ; fall-thr → region A
///   1006:  c3                ret                ;          → region B
/// ```
///
/// Both rets land at distinct machine addresses, so the lifter
/// produces two `RegionLiftHandles` entries.  Used by
/// `dump_per_region_writes_one_html_per_region` to verify
/// multi-region emission without depending on a built ELF fixture.
pub fn lift_branch_snippet_x86_64() -> (AnalyzeOutcome, Cfg<BufMemReader<Vec<u8>>>) {
    // test rax, rax ; jz +1 ; ret ; ret
    lift_x86_64_bytes(vec![0x48u8, 0x85, 0xc0, 0x74, 0x01, 0xc3, 0xc3])
}

/// Build an x86_64 [`Cfg`] for a straight-line snippet that produces
/// a deep value chain ending in `ret`:
///
/// ```text
///   1000:  48 01 c0          add rax, rax   ; rax = rax + rax (insn 1)
///   1003:  48 01 c0          add rax, rax   ;                 (insn 2)
///   1006:  48 01 c0          add rax, rax   ;                 (insn 3)
///   1009:  48 01 c0          add rax, rax   ;                 (insn 4)
///   100c:  c3                ret
/// ```
///
/// Each `add` consumes the previous insn's `rax` value (after register
/// aliasing lifts it through Phi(Some(rax))), so the IR is a chain of
/// `IntBinaryOp(Add)` nodes feeding `Return` via the SystemV ret-val
/// register read.  Used by the depth-3 neighborhood test to verify
/// that `dump_neighborhood` walks more than one hop.
pub fn lift_add_chain_snippet_x86_64() -> (AnalyzeOutcome, Cfg<BufMemReader<Vec<u8>>>) {
    lift_x86_64_bytes(vec![
        0x48, 0x01, 0xc0, // add rax, rax
        0x48, 0x01, 0xc0, // add rax, rax
        0x48, 0x01, 0xc0, // add rax, rax
        0x48, 0x01, 0xc0, // add rax, rax
        0xc3, // ret
    ])
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

/// RAII scratch directory: allocates a per-test temp dir under the
/// system temp dir (via [`unique_tmp_dir`]) and recursively removes it
/// on drop.  Replaces the hand-rolled
/// `let tmp = unique_tmp_dir(...); ... let _ = remove_dir_all(&tmp);`
/// pattern across the dump integration tests; the `Drop` impl runs
/// even on panic, so a failing assertion no longer leaks the temp dir.
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    /// Create a new per-test scratch directory tagged with `tag`.
    pub fn new(tag: &str) -> Self {
        Self(unique_tmp_dir(tag))
    }

    /// The directory's path.  Borrowed (not owned) so callers can use
    /// it for `.join(...)` against the temp dir without consuming the
    /// guard.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The vendored viewer's JSON-payload anchor string.  Both dump
/// helpers embed the DOT source inside a
/// `<script type="application/json" id="dot-src">` element; finding
/// this substring in the output HTML is sufficient evidence that the
/// render path completed (vs. an empty / partial file).
pub const VIEWER_JSON_ANCHOR: &str =
    "<script type=\"application/json\" id=\"dot-src\">";
