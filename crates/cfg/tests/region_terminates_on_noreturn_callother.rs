//! cfg::region_builder terminates a region as RegionTerminator::NoReturn
//! when it sees a CallOther whose target::user_ops::classify is NoReturn.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::make_sleigh_with_bytes;

use cfg::{Builder, OptionsBuilder, RegionTerminator};

#[test]
fn ud2_region_finishes_as_noreturn() {
    // x86_64 ud2 = 0x0F 0x0B.  Sleigh emits:
    //   pcode[0]: CallOther [user_op = invalidInstructionException]
    //   pcode[1]: BranchIndirect
    let bytes = vec![0x0fu8, 0x0b];
    let entry = 0x1000u64;
    let cfg = Builder::new(
        make_sleigh_with_bytes(bytes, entry),
        entry,
        OptionsBuilder::new().build(),
    )
    .build()
    .expect("cfg");

    let any_noreturn = cfg
        .graph
        .node_weights()
        .any(|r| matches!(r.terminator, RegionTerminator::NoReturn));
    assert!(
        any_noreturn,
        "expected at least one NoReturn region; got terminators: {:?}",
        cfg.graph
            .node_weights()
            .map(|r| &r.terminator)
            .collect::<Vec<_>>(),
    );
    // The trailing BranchIndirect must NOT have been processed —
    // there should be no UnresolvedIndirectBranch terminator.
    let any_unresolved = cfg.graph.node_weights().any(|r| {
        matches!(
            r.terminator,
            RegionTerminator::UnresolvedIndirectBranch { .. }
        )
    });
    assert!(
        !any_unresolved,
        "trap region should terminate as NoReturn before its trailing \
         BranchIndirect routes through process_branch_indirect",
    );
}
