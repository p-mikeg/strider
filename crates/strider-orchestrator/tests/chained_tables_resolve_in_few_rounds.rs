//! A chain of jump tables each reachable only through the previous table's
//! arms, the shape gcc -O2 emits for consecutive fully-covered switches.
//!
//! ```text
//! L_k:  mov eax, edi ; shr eax, k ; and eax, 3 ; jmp [T_k + rax*8]
//!       inc edx ; jmp L_k+1        (x4 arms, one per table slot)
//! L_n:  ret
//! ```
//!
//! Every level is discovered only once the level before it is seated, so a
//! loop that lifts the whole function to classify each newly exposed table
//! pays a full round per level.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use strider_ir::IRWalker;
use strider_ir::node::NodeKind;
use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::{EditFunction, OptCtx, OptOptions, PostOptimizer};

mod common;

const BASE: u64 = 0x40_1000;
const TABLES: u64 = 0x50_0000;
const ARMS: u64 = 4;
/// `mov` (2) + `shr` (3) + `and` (3) + `jmp [rax*8 + disp32]` (7).
const DISPATCH_LEN: u64 = 15;
/// `inc edx` (2) + `jmp rel32` (5).
const ARM_LEN: u64 = 7;
const LEVEL_LEN: u64 = DISPATCH_LEN + ARMS * ARM_LEN;

fn level_addr(k: u64) -> u64 {
    BASE + k * LEVEL_LEN
}

/// `levels` chained tables, then `ret`, and the tables themselves.
fn chain(levels: u64) -> (Vec<u8>, Vec<u8>) {
    let mut text = Vec::new();
    let mut tables = Vec::new();
    for k in 0..levels {
        let table = u32::try_from(TABLES + k * ARMS * 8).expect("32-bit table address");
        text.extend_from_slice(&[0x89, 0xf8, 0xc1, 0xe8]);
        text.push(u8::try_from(k % 30).expect("shift"));
        text.extend_from_slice(&[0x83, 0xe0, 0x03, 0xff, 0x24, 0xc5]);
        text.extend_from_slice(&table.to_le_bytes());
        for arm in 0..ARMS {
            let at = level_addr(k) + DISPATCH_LEN + arm * ARM_LEN;
            tables.extend_from_slice(&at.to_le_bytes());
            let next =
                i32::try_from(level_addr(k + 1) as i64 - (at + ARM_LEN) as i64).expect("rel32");
            text.extend_from_slice(&[0xff, 0xc2, 0xe9]);
            text.extend_from_slice(&next.to_le_bytes());
        }
    }
    text.push(0xc3);
    (text, tables)
}

struct Rom {
    text: Vec<u8>,
    tables: Vec<u8>,
}

impl strider_orchestrator::opt::ReadOnlyMemory for Rom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        for (base, bytes) in [(BASE, &self.text), (TABLES, &self.tables)] {
            if let Some(off) = addr.checked_sub(base)
                && let Ok(off) = usize::try_from(off)
                && let Some(src) = bytes.get(off..off + buf.len())
            {
                buf.copy_from_slice(src);
                return Ok(());
            }
        }
        anyhow::bail!("unmapped {addr:#x}")
    }
}

/// Counts pipeline runs over a function holding more than one dispatch site:
/// every full round after the first, and nothing narrower.
#[derive(Clone)]
struct CountWideRuns(Arc<AtomicUsize>);

impl PostOptimizer for CountWideRuns {
    fn apply(&self, edit: &mut EditFunction<'_>, _ctx: &mut OptCtx<'_>) -> anyhow::Result<()> {
        let sites = edit
            .function()
            .walk_kind(|k| matches!(k, NodeKind::IndirectBranch | NodeKind::Switch(_)))
            .count();
        if sites > 1 {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[test]
fn a_chain_of_tables_seats_in_a_constant_number_of_rounds() {
    let levels = 24;
    let (text, tables) = chain(levels);
    let (mut strider, cc) = common::strider_over_bytes(
        common::Arch::X64,
        text.clone(),
        BASE,
        Some(Box::new(Rom { text, tables })),
    );
    let wide_runs = Arc::new(AtomicUsize::new(0));
    let mut pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.add_post_pass(strider_orchestrator::opt::IndirectBranchClassify);
    pipeline.add_post_pass(CountWideRuns(Arc::clone(&wide_runs)));
    let result = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            Some(pipeline),
        )
        .expect("analyze");

    assert!(
        result.is_complete(),
        "unresolved {:?}",
        result.unresolved_indirect_branches
    );
    let seated = result
        .cfg
        .regions()
        .filter(|r| {
            matches!(&r.terminator, strider_cfg::RegionTerminator::Switch { targets, .. }
                if targets.len() == 4)
        })
        .count();
    assert_eq!(seated, 24, "every level seats its four arms");
    let rounds = wide_runs.load(Ordering::Relaxed);
    assert!(
        rounds <= 2,
        "{rounds} full rounds for {levels} chained tables; one per level is the quadratic loop"
    );
}

/// A second table whose own code bounds its index less tightly than the whole
/// function does:
///
/// ```text
/// E:   cmp edi, 3 ; ja END
/// L0:  mov eax, edi ; and eax, 7 ; jmp [T0 + rax*8]    T0 = a0..a3, x, x, x, x
/// a_i: inc edx ; jmp L1
/// L1:  mov eax, edi ; and eax, 7 ; jmp [T1 + rax*8]    T1 = b0..b3, y0..y3
/// b_i, y_i, x: ret
/// END: ret
/// ```
///
/// Lifted from `L1` on its own, the `and eax, 7` spans all eight slots; the
/// entry guard, which dominates `L1`, proves four. The result is the one the
/// whole function proves.
#[test]
fn a_table_the_whole_function_bounds_tighter_seats_the_tighter_answer() {
    const E: u64 = BASE;
    // `cmp edi, 3` (3) + `ja rel32` (6).
    const L0: u64 = E + 9;
    // `mov eax, edi` (2) + `and eax, 7` (3) + `jmp [rax*8 + disp32]` (7).
    const TABLE_JUMP_LEN: u64 = 12;
    const A: u64 = L0 + TABLE_JUMP_LEN;
    const L1: u64 = A + ARMS * ARM_LEN;
    const RETS: u64 = L1 + TABLE_JUMP_LEN;
    // b0..b3, y0..y3, x, END: one `ret` each.
    const END: u64 = RETS + 9;
    const T0: u64 = TABLES;
    const T1: u64 = TABLES + 64;

    let table_jump = |table: u64| {
        let mut bytes = vec![0x89, 0xf8, 0x83, 0xe0, 0x07, 0xff, 0x24, 0xc5];
        bytes.extend_from_slice(&u32::try_from(table).expect("disp32").to_le_bytes());
        bytes
    };
    let mut text = vec![0x83, 0xff, 0x03, 0x0f, 0x87];
    text.extend_from_slice(&i32::try_from(END - L0).expect("rel32").to_le_bytes());
    text.extend(table_jump(T0));
    for arm in 0..ARMS {
        let at = A + arm * ARM_LEN;
        text.extend_from_slice(&[0xff, 0xc2, 0xe9]);
        text.extend_from_slice(
            &i32::try_from(L1 as i64 - (at + ARM_LEN) as i64)
                .expect("rel32")
                .to_le_bytes(),
        );
    }
    text.extend(table_jump(T1));
    text.extend(std::iter::repeat_n(0xc3u8, 10));
    assert_eq!(BASE + text.len() as u64, END + 1);

    let x = RETS + 8;
    let mut tables = Vec::new();
    for slot in 0..8 {
        let target = if slot < ARMS { A + slot * ARM_LEN } else { x };
        tables.extend_from_slice(&target.to_le_bytes());
    }
    for slot in 0..8 {
        tables.extend_from_slice(&(RETS + slot).to_le_bytes());
    }

    let (mut strider, cc) = common::strider_over_bytes(
        common::Arch::X64,
        text.clone(),
        BASE,
        Some(Box::new(Rom { text, tables })),
    );
    let result = strider
        .analyze(
            E,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    let arms_at = |site: u64| -> Vec<u64> {
        let mut arms: Vec<u64> = result
            .cfg
            .regions()
            .filter_map(|r| match &r.terminator {
                strider_cfg::RegionTerminator::Switch { targets, addr, .. }
                    if addr.machine_addr.addr == site =>
                {
                    Some(targets.iter().map(|t| t.addr).collect::<Vec<_>>())
                }
                _ => None,
            })
            .flatten()
            .collect();
        arms.sort_unstable();
        arms
    };
    assert_eq!(
        arms_at(L0 + 5),
        (0..ARMS).map(|i| A + i * ARM_LEN).collect::<Vec<_>>()
    );
    assert_eq!(
        arms_at(L1 + 5),
        (0..ARMS).map(|i| RETS + i).collect::<Vec<_>>()
    );
    assert!(
        result.is_complete(),
        "unresolved {:?}",
        result.unresolved_indirect_branches
    );
}
