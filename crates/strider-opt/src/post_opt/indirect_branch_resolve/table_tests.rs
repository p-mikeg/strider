use super::super::tests::{ext_26_6, srl_sltiu};
use super::*;
use crate::test_support::{COLLAPSE, STANDARD, at, pipeline, run_passes};
use crate::{ConstantFold, KnownBits, Optimizer, PhiCollapse, ReadOnlyMemory, RegionCollapse};
use rsleigh::VnSpace;
use std::sync::Mutex;
use strider_ir::node::ValueType;
use strider_ir::{
    ExtendOp, Function, FunctionBuilder, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntCmpOp,
};
use strider_ir_test_utils::{
    IrBuilderEx, IrWalkerEx, MockRom, RegisterSet, reg_vn, sp_frame, stack_vn_aarch64 as sp64,
    stack_vn_x86 as sp32_vn,
};

/// The folding a stack-table fixture gets before classification: no
/// `LoadForward`, so the dispatch load survives.
const FOLD: &[&dyn Optimizer] = &[&ConstantFold, &KnownBits, &PhiCollapse, &RegionCollapse];

pub(super) struct RecordingRom {
    inner: MockRom,
    log: Mutex<Vec<(u64, usize)>>,
}

impl ReadOnlyMemory for RecordingRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.log.lock().unwrap().push((addr, buf.len()));
        self.inner.read(addr, buf)
    }
}

/// An entry region terminated by a placeholder `IndirectBranch(target)`; the
/// closure builds the target's producer subtree.
fn build_with_target(
    target_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
) -> (Function, ValueId) {
    let mut builder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let target = target_inputs(&mut builder);
    builder
        .build_indirect_branch(target)
        .expect("build_indirect_branch");
    builder.set_lift_addr(None);
    (builder.build().expect("build"), target)
}

/// `regs`' function guarded by `if (cond)`, where `guard` builds `cond` and a
/// carry-over in the entry and `dispatch` the branch target in the region
/// the edge `taken` names; the other edge returns.  Run through `passes`.
fn guarded<T>(
    regs: RegisterSet,
    guard: impl FnOnce(&mut FunctionBuilder) -> (ValueId, T),
    taken: bool,
    dispatch: impl FnOnce(&mut FunctionBuilder, T) -> ValueId,
    passes: &[&'static dyn Optimizer],
) -> Function {
    let mut b = regs.build_fn().unwrap();
    let [entry, at, exit] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let (cond, carried) = guard(&mut b);
    let (t, e) = if taken { (at, exit) } else { (exit, at) };
    b.build_if(cond, t, e).unwrap();
    b.set_region(at);
    let target = dispatch(&mut b, carried);
    b.build_indirect_branch(target).unwrap();
    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut function = b.build().unwrap();
    run_passes(&mut function, passes).unwrap();
    function
}

/// `x < k` at `x`'s type.
fn below(b: &mut FunctionBuilder, x: ValueId, k: u64) -> ValueId {
    let ty = b.value_type_opt(x).unwrap();
    let k = b.build_int_const(k, ty).unwrap();
    b.build_int_cmp_operation(x, k, IntCmpOp::Less, ty).unwrap()
}

/// The ISA mode and target of an interworking `word`: `word & 1` and
/// `word & ~1`, at `I32`.
fn split_mode(b: &mut FunctionBuilder, word: ValueId) -> (ValueId, ValueId) {
    let i32t = ValueType::int(32);
    let mode = b
        .build_const_binop(1u64, word, IntBinaryOp::And, i32t)
        .unwrap();
    let target = b
        .build_const_binop(0xFFFF_FFFEu64, word, IntBinaryOp::And, i32t)
        .unwrap();
    (target, mode)
}

fn sole_indirect_branch(f: &Function) -> NodeId {
    f.sole_kind(|k| matches!(k, NodeKind::IndirectBranch))
}

/// A load the optimiser cannot fold away, so the dispatch genuinely depends
/// on it.
fn build_non_const_idx(fb: &mut FunctionBuilder) -> ValueId {
    let addr = fb.build_int_const(0x9000u64, ValueType::int(32)).unwrap();
    fb.build_load(addr, VnSpace::RAM, ValueType::int(32))
        .expect("u32 load (idx)")
}

/// `Load(base + idx * stride):I32`, the tail every table dispatch ends in.
fn table_target(fb: &mut FunctionBuilder, idx: ValueId, stride: u64, base: u64) -> ValueId {
    let i32t = ValueType::int(32);
    let mul = fb
        .build_const_binop(stride, idx, IntBinaryOp::Mul, i32t)
        .expect("mul");
    let addr = fb
        .build_const_binop(base, mul, IntBinaryOp::Add, i32t)
        .expect("add");
    fb.build_load(addr, VnSpace::RAM, i32t)
        .expect("dispatch load")
}

/// [`table_target`] over `idx & mask`.
fn masked_table_target(
    fb: &mut FunctionBuilder,
    idx: ValueId,
    mask: u64,
    stride: u64,
    base: u64,
) -> ValueId {
    let masked = fb
        .build_const_binop(mask, idx, IntBinaryOp::And, ValueType::int(32))
        .expect("and");
    table_target(fb, masked, stride, base)
}

/// [`classify_dispatch_value`] on the sole `IndirectBranch`, over freshly
/// computed ranges, on a copy of `f` claiming `assumptions` as well.
fn classify_with(
    f: &Function,
    rom: Option<&dyn ReadOnlyMemory>,
    assumptions: &crate::AssumptionOptions,
    mode_value: Option<ValueId>,
) -> Option<ResolvedTargets> {
    let mut f = f.clone();
    f.assume(assumptions);
    let branch = sole_indirect_branch(&f);
    let target = f.indirect_branch_target(branch);
    SiteClassifier::new(&f, rom).classify_dispatch_value(branch, target, mode_value)
}

/// [`classify_with`] adding no claim, with no ISA mode: at the sound floor, so
/// a case that needs a claim passes it through [`classify_with`] instead of
/// inheriting one here.
fn classify(f: &Function, rom: Option<&dyn ReadOnlyMemory>) -> Option<ResolvedTargets> {
    classify_with(f, rom, &crate::AssumptionOptions::none(), None)
}

/// A table's arm addresses, `None` for a deferral.
#[track_caller]
fn arms(r: Option<ResolvedTargets>) -> Option<Vec<u64>> {
    match r? {
        ResolvedTargets::Multiple(ts) => Some(ts.iter().map(|t| t.addr).collect()),
        other => panic!("expected a table, got {other:?}"),
    }
}

#[test]
fn classify_table_dispatch_with_known_bits_bound_returns_multiple() {
    // KnownBits bounds the `& 0x7` index to 8 entries.
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        masked_table_target(fb, raw, 0x7u64, 4u64, 0x4000u64)
    });
    let rom = MockRom::strided(
        0x4000,
        4,
        vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
        4,
    );
    let result = classify(&g, Some(&rom));
    assert_eq!(
        arms(result),
        Some(vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80])
    );
}

#[test]
fn classify_table_dispatch_duplicate_targets_are_deduped() {
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        masked_table_target(fb, raw, 0x3u64, 4u64, 0x4000u64)
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x10, 0x20], 4);
    let result = classify(&g, Some(&rom));
    assert_eq!(
        arms(result),
        Some(vec![0x10, 0x20]),
        "four indices producing [0x10,0x20,0x10,0x20] dedup to two targets"
    );
}

#[test]
fn classify_table_dispatch_single_entry_bound_returns_multiple_of_one() {
    // Masking with 0 proves idx is always 0, so exactly one entry is read.
    let (g, _target) = build_with_target(|fb| {
        let idx_src = build_non_const_idx(fb);
        masked_table_target(fb, idx_src, 0u64, 4u64, 0x4000u64)
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    let result = classify(&g, Some(&rom));
    assert_eq!(
        arms(result),
        Some(vec![0x10]),
        "bound 1 reads exactly the first entry"
    );
}

/// The deferrals of a masked table: past the entry cap
/// (`MAX_TABLE_ENTRIES` is 4096) under a ROM that serves every entry, with
/// no ROM, and with no mask; the cap itself still resolves.
/// Mutation: drop the cap clause of `index_bound_ok`.
#[test]
fn classify_table_dispatch_defers_over_cap_resolves_under_cap() {
    let entries: Vec<u64> = (0..8192).map(|i| 0x10_0000 + 4 * i).collect();
    let rom = MockRom::strided(0x4000, 4, entries.clone(), 4);
    let rows: [(&str, Option<u64>, bool, Option<usize>); 5] = [
        ("under the cap", Some(0x3), true, Some(4)),
        ("at the cap", Some(0xFFF), true, Some(4096)),
        ("over the cap", Some(0x1FFF), true, None),
        ("no rom", Some(0x3), false, None),
        ("unbounded index", None, true, None),
    ];
    for (name, mask, with_rom, want) in rows {
        let (g, _) = build_with_target(|fb| {
            let idx = build_non_const_idx(fb);
            match mask {
                Some(m) => masked_table_target(fb, idx, m, 4, 0x4000),
                None => table_target(fb, idx, 4, 0x4000),
            }
        });
        let rom: Option<&dyn ReadOnlyMemory> = if with_rom { Some(&rom) } else { None };
        let got: Option<Vec<u64>> = match classify(&g, rom) {
            Some(ResolvedTargets::Multiple(ts)) => Some(ts.iter().map(|t| t.addr).collect()),
            None => None,
            other => panic!("{name}: {other:?}"),
        };
        assert!(
            got.as_deref() == want.map(|n| &entries[..n]),
            "{name}: {:?} entries",
            got.map(|ts| ts.len())
        );
    }
}

/// `zext32(load(0x9000):ty)`, a table cell read as the dispatch would.
fn cell(fb: &mut FunctionBuilder, ty: ValueType) -> ValueId {
    let addr = fb.build_int_const(0x9000u64, ValueType::int(32)).unwrap();
    let cell = fb.build_load(addr, VnSpace::RAM, ty).expect("cell load");
    fb.extend_if_needed(cell, ValueType::int(32), ExtendOp::ZeroExtend)
        .expect("zero-extend the cell to I32")
}

/// A table ENTRY, scaled or narrowed in ways that keep every value of the
/// cell, is table DATA, not the index: each row defers under a ROM serving
/// every address the enumeration would touch, so the deferral can only come
/// from the width-only exclusion, not a fold failure.
///
/// - ARM `tbb`: a byte zero-extended to `I32` with no mask and no guard;
///   `[0,255]` passes the `hi < type_mask` check against `I32`.
/// - PowerPC `slwi`, `And(entry << 2, ~3)`: the mask clears only bits the
///   shift zeroed.
/// - x86-64 `movzbl %al` after a 32-bit load, `zext(trunc8(load32))`.
///   Mutation: leave the set unchanged at a truncate in `is_width_only`.
/// - `entry >> 2`: a right shift is still a scaling of the raw cell.
/// - `(entry << 2) >> 1`, `entry * 2`: the widening runs BELOW the lossy
///   shift, so counting only the lossy factor expects 128 values.
#[test]
fn classify_table_dispatch_excludes_a_table_entry_as_the_index() {
    type Idx = fn(&mut FunctionBuilder) -> ValueId;
    let rows: [(&str, Idx, u64); 5] = [
        ("tbb", |fb| cell(fb, ValueType::int(8)), 4),
        (
            "identity mask",
            |fb| {
                let wide = cell(fb, ValueType::int(8));
                let up = fb
                    .build_const_binop(2u64, wide, IntBinaryOp::ShiftLeft, ValueType::int(32))
                    .unwrap();
                fb.build_const_binop(0xffff_fffcu64, up, IntBinaryOp::And, ValueType::int(32))
                    .unwrap()
            },
            1,
        ),
        (
            "low byte of a word",
            |fb| {
                let word = cell(fb, ValueType::int(32));
                let byte = fb.truncate_if_needed(word, ValueType::int(8)).unwrap();
                fb.extend_if_needed(byte, ValueType::int(32), ExtendOp::ZeroExtend)
                    .unwrap()
            },
            4,
        ),
        (
            "right shift",
            |fb| {
                let wide = cell(fb, ValueType::int(8));
                fb.build_const_binop(2u64, wide, IntBinaryOp::ShiftRight, ValueType::int(32))
                    .unwrap()
            },
            4,
        ),
        (
            "widened then shifted",
            |fb| {
                let wide = cell(fb, ValueType::int(8));
                let up = fb
                    .build_const_binop(2u64, wide, IntBinaryOp::ShiftLeft, ValueType::int(32))
                    .unwrap();
                fb.build_const_binop(1u64, up, IntBinaryOp::ShiftRight, ValueType::int(32))
                    .unwrap()
            },
            4,
        ),
    ];
    let rom = MockRom::strided(0x4000, 4, (0..512).map(|i| 0x5000 + i).collect(), 4);
    for (name, idx, stride) in rows {
        let (g, _) = build_with_target(|fb| {
            let idx = idx(fb);
            table_target(fb, idx, stride, 0x4000)
        });
        assert_eq!(classify(&g, Some(&rom)), None, "{name}");
    }
}

/// `0x5000 + 4 * zext(load8(base + offset))`, a jump computed from a byte as
/// AArch64 `adr; ldrb; add x, x, w, lsl #2; br` does.  `base` is a register,
/// or with `bounded` a byte loaded from RAM, bounded by its width alone.
fn byte_at(fb: &mut FunctionBuilder, bounded: bool, offset: u64) -> ValueId {
    let base = if bounded {
        let at = fb.build_int_const(0x9000u64, ValueType::int(32)).unwrap();
        let index = fb
            .build_load(at, VnSpace::RAM, ValueType::int(8))
            .expect("index byte");
        fb.extend_if_needed(index, ValueType::int(32), ExtendOp::ZeroExtend)
            .expect("zero-extend")
    } else {
        build_non_const_idx(fb)
    };
    let off = fb.build_int_const(offset, ValueType::int(32)).unwrap();
    let addr = fb
        .build_int_binary_operation(base, off, IntBinaryOp::Add, ValueType::int(32))
        .expect("add");
    let byte = fb
        .build_load(addr, VnSpace::RAM, ValueType::int(8))
        .expect("byte load");
    let wide = fb
        .extend_if_needed(byte, ValueType::int(32), ExtendOp::ZeroExtend)
        .expect("zero-extend");
    let two = fb.build_int_const(2u64, ValueType::int(32)).unwrap();
    let scaled = fb
        .build_int_binary_operation(wide, two, IntBinaryOp::ShiftLeft, ValueType::int(32))
        .expect("shl");
    let code = fb.build_int_const(0x5000u64, ValueType::int(32)).unwrap();
    fb.build_int_binary_operation(scaled, code, IntBinaryOp::Add, ValueType::int(32))
        .expect("add")
}

/// A byte at a table's base plus a bounded index is a cell of that table,
/// whether or not the ROM serves the table: its width bounds nothing.
/// Mutation: take no term of the address for a bounded index.
#[test]
fn a_width_bounded_byte_of_a_table_is_excluded_as_the_index() {
    let (g, _) = build_with_target(|fb| byte_at(fb, true, 0xA000));
    assert_eq!(classify(&g, None), None);
}

/// A byte at a ROM-served table base plus an unbounded register is a cell of
/// that table, as a Thumb `tbb` whose index bound is missed reads it.
/// Mutation: read no constant term as ROM in `loads_a_field`.
#[test]
fn a_width_bounded_byte_of_a_rom_table_is_excluded_as_the_index() {
    let (g, _) = build_with_target(|fb| byte_at(fb, false, 0xA000));
    let rom = MockRom::raw_bytes(0xA000, vec![0; 256]);
    assert_eq!(classify(&g, Some(&rom)), None);
}

/// A byte at an unbounded register plus an offset no ROM serves is a field
/// read through a pointer: any of its 256 values may pick the target.
/// Mutation: take every term of the address for a bounded index, or read
/// every constant term as ROM.
/// A byte at an unbounded register plus a first-page offset a ROM serves (a
/// PIE's headers) is still a field read through a pointer.
/// Mutation: read a first-page constant term as a table base.
#[test]
fn a_width_bounded_byte_at_a_mapped_first_page_offset_is_the_index() {
    let (g, _) = build_with_target(|fb| byte_at(fb, false, 8));
    let rom = MockRom::raw_bytes(0, vec![0; 0x1000]);
    let want: Vec<u64> = (0..256).map(|i| 0x5000 + 4 * i).collect();
    assert_eq!(arms(classify(&g, Some(&rom))), Some(want));
}

#[test]
fn a_width_bounded_byte_through_a_pointer_is_the_index() {
    let (g, _) = build_with_target(|fb| byte_at(fb, false, 0xcb));
    let rom = MockRom::raw_bytes(0xA000, vec![0; 256]);
    let want: Vec<u64> = (0..256).map(|i| 0x5000 + 4 * i).collect();
    assert_eq!(arms(classify(&g, Some(&rom))), Some(want));
}

#[test]
fn classify_table_dispatch_resolves_guarded_shift_narrowed_loaded_index() {
    // x86 instruction-decoder shape: `loaded_byte >> 5`, the top 3 bits, under
    // `if (field < 6)`.  The shift alone is a scaling of the raw cell; the GUARD
    // is what narrows it below the cell's 8-value image and makes it a real
    // index.
    let g = guarded(
        RegisterSet::new(),
        |b| {
            let byte = cell(b, ValueType::int(8));
            let idx = b
                .build_const_binop(5u64, byte, IntBinaryOp::ShiftRight, ValueType::int(32))
                .expect("byte >> 5 -> [0,7]");
            (below(b, idx, 6), idx)
        },
        true,
        |b, idx| table_target(b, idx, 4, 0x4000),
        COLLAPSE,
    );
    let rom = MockRom::strided(
        0x4000,
        4,
        vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
        4,
    );
    assert_eq!(
        arms(classify(&g, Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60])
    );
}

#[test]
fn classify_table_dispatch_masked_full_byte_i32_resolves() {
    // Adversarial pair to the width-only exclusion: this ALSO spans [0,255],
    // but it is `x & 0xFF` typed I32 with no byte-typed producer to strip to,
    // so the range does not fill its type width.  A genuine 256-entry index.
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb); // Load:I32, unbounded
        masked_table_target(fb, raw, 0xFFu64, 4u64, 0x4000u64)
    });
    let entries: Vec<u64> = (0..256).map(|i| 0x5000 + i).collect();
    let rom = MockRom::strided(0x4000, 4, entries.clone(), 4);
    let result = classify(&g, Some(&rom));
    assert_eq!(arms(result), Some(entries));
}

/// `count(raw)` (or `count(raw | 1)` when `low_bit`) of an `I32` load,
/// zero-extended to `I64` and indexing a 4-byte table at `0x4000` whose 64
/// slots all hold distinct targets; the arms seated.
fn count_indexed_arms(kind: NodeKind, low_bit: bool) -> Option<Vec<u64>> {
    let (i32t, i64t) = (ValueType::int(32), ValueType::int(64));
    let (g, _target) = build_with_target(|fb| {
        let mut raw = build_non_const_idx(fb);
        if low_bit {
            raw = fb
                .build_const_binop(1u64, raw, IntBinaryOp::Or, i32t)
                .unwrap();
        }
        let count = match kind {
            NodeKind::Lzcount => fb.build_lzcount(raw, i32t),
            NodeKind::Popcount => fb.build_popcount(raw, i32t),
            other => panic!("not a count: {other:?}"),
        }
        .unwrap();
        let idx = fb
            .extend_if_needed(count, i64t, ExtendOp::ZeroExtend)
            .unwrap();
        let mul = fb
            .build_const_binop(4u64, idx, IntBinaryOp::Mul, i64t)
            .unwrap();
        let addr = fb
            .build_const_binop(0x4000u64, mul, IntBinaryOp::Add, i64t)
            .unwrap();
        let cell = fb.build_load(addr, VnSpace::RAM, i32t).unwrap();
        fb.extend_if_needed(cell, i64t, ExtendOp::ZeroExtend)
            .unwrap()
    });
    let rom = MockRom::strided(0x4000, 4, (0..64).map(|i| 0x5000 + 4 * i).collect(), 4);
    arms(classify(&g, Some(&rom)))
}

#[test]
fn a_bit_count_index_reads_only_the_counts_its_input_width_allows() {
    let slots = |n: u64| Some((0..n).map(|i| 0x5000 + 4 * i).collect::<Vec<_>>());
    assert_eq!(count_indexed_arms(NodeKind::Lzcount, false), slots(33));
    assert_eq!(count_indexed_arms(NodeKind::Popcount, false), slots(33));
    // A known-one low bit leaves at most 31 leading zeros.
    assert_eq!(count_indexed_arms(NodeKind::Lzcount, true), slots(32));
}

#[test]
fn decompose_index_picks_shallowest_narrowed_index() {
    // Two bounded dominators of one dispatch: a deep `reg & 0x3F` and the
    // shallow `(reg & 0x3F) & 0x7` the address actually scales.  The shallow one
    // must win, or a guard-tightened dispatch over-enumerates and defers.
    let (g, _target) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        let m63 = fb.build_int_const(0x3Fu64, ValueType::int(32)).unwrap();
        let wide = fb
            .build_int_binary_operation(raw, m63, IntBinaryOp::And, ValueType::int(32))
            .expect("reg & 0x3F -> [0,63] (deep)");
        masked_table_target(fb, wide, 0x7u64, 4u64, 0x4000u64)
    });
    let branch = sole_indirect_branch(&g);
    let target_value = g.indirect_branch_target(branch);
    let chain = dominator_chain(&g, target_value).expect("a dispatch cone");
    match SiteClassifier::new(&g, None).decompose_index(&chain, branch, false) {
        Some(hit) => assert_eq!(
            (hit.range.lo(), hit.range.hi(), hit.rebase),
            (0, 7, 0),
            "must pick the shallow narrowed [0,7] index, not the deeper [0,63]"
        ),
        None => panic!("expected an index candidate"),
    }
}

#[test]
fn classify_table_dispatch_defers_nonloaded_full_byte_index_conservatively() {
    // A 256-case switch on a byte REGISTER is a real index, but its [0,255]
    // fills the byte width and is indistinguishable by range from a loaded byte
    // entry, so it defers.  Real byte indices arrive
    // through a load/mask/shift that narrows below the width.
    let bl = reg_vn(0x0, 1);
    let mut b = RegisterSet::new()
        .tracked(bl)
        .build_fn_single_region()
        .unwrap();
    let byte = b.read_variable(&bl).unwrap();
    let idx = b
        .extend_if_needed(byte, ValueType::int(32), ExtendOp::ZeroExtend)
        .expect("zext byte register");
    let loaded = table_target(&mut b, idx, 4u64, 0x4000u64);
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let function = b.build().unwrap();

    let rom = MockRom::strided(0x4000, 4, (0..256).map(|i| 0x5000 + i).collect(), 4);
    assert_eq!(
        classify(&function, Some(&rom)),
        None,
        "a width-filling byte index is conservatively deferred (indistinguishable from an entry by range)"
    );
}

/// `(tbl[i] & m) + base`: the mask puts the CELL's range below its load width,
/// so the cell reads as a narrowed index. Enumerating it seats one arm per
/// masked value where the runtime has one per `i`.
#[test]
fn classify_table_dispatch_masked_cell_prefers_the_index_in_its_address() {
    let idx_var = reg_vn(0x10, 4);
    let mut b = RegisterSet::new()
        .tracked(idx_var)
        .build_fn_single_region()
        .unwrap();
    let raw = b.read_variable(&idx_var).unwrap();
    let three = b.build_int_const(3u64, ValueType::int(32)).unwrap();
    let idx = b
        .build_int_binary_operation(raw, three, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    let cell = {
        let wide = b
            .extend_if_needed(idx, ValueType::int(64), ExtendOp::ZeroExtend)
            .unwrap();
        let two = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let scaled = b
            .build_int_binary_operation(wide, two, IntBinaryOp::Mul, ValueType::int(64))
            .unwrap();
        let base = b.build_int_const(0x4000u64, ValueType::int(64)).unwrap();
        let addr = b
            .build_int_binary_operation(scaled, base, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_load(addr, VnSpace::RAM, ValueType::int(16))
            .unwrap()
    };
    // The lifted shape: the i16 cell widened to the register, then narrowed
    // to the width the mask is applied at.
    let cell64 = b
        .extend_if_needed(cell, ValueType::int(64), ExtendOp::ZeroExtend)
        .unwrap();
    let narrow = b.truncate_if_needed(cell64, ValueType::int(32)).unwrap();
    let m63 = b.build_int_const(0x3Fu64, ValueType::int(32)).unwrap();
    let masked = b
        .build_int_binary_operation(narrow, m63, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    let offset = b
        .extend_if_needed(masked, ValueType::int(64), ExtendOp::ZeroExtend)
        .unwrap();
    let code = b.build_int_const(0x5000u64, ValueType::int(64)).unwrap();
    let target = b
        .build_int_binary_operation(offset, code, IntBinaryOp::Add, ValueType::int(64))
        .unwrap();
    b.build_indirect_branch(target).unwrap();
    b.set_lift_addr(None);
    let function = b.build().unwrap();

    let rom = MockRom::strided(0x4000, 2, vec![0, 6, 12, 18], 2);
    assert_eq!(
        arms(classify(&function, Some(&rom))),
        Some(vec![0x5000, 0x5006, 0x500c, 0x5012])
    );
}

/// The same narrowing mask over a cell the evaluator CANNOT fold: nothing
/// deeper is an index, so the masked value is the one to enumerate.
#[test]
fn classify_table_dispatch_masked_opaque_cell_stays_the_index() {
    let ptr_var = reg_vn(0x10, 8);
    let mut b = RegisterSet::new()
        .tracked(ptr_var)
        .build_fn_single_region()
        .unwrap();
    let ptr = b.read_variable(&ptr_var).unwrap();
    let byte = b.build_load(ptr, VnSpace::RAM, ValueType::int(8)).unwrap();
    let wide = b
        .extend_if_needed(byte, ValueType::int(32), ExtendOp::ZeroExtend)
        .unwrap();
    let three = b.build_int_const(3u64, ValueType::int(32)).unwrap();
    let masked = b
        .build_int_binary_operation(wide, three, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    let loaded = table_target(&mut b, masked, 4u64, 0x4000u64);
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let function = b.build().unwrap();

    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    assert_eq!(
        arms(classify(&function, Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40])
    );
}

/// `switch (x)` over a negative case base: gcc and clang guard `x + k` and
/// ConstantFold then merges `k` into the table base, so the guarded value is
/// nowhere in the dispatch cone and only the sibling carries the bound.
#[test]
fn classify_table_dispatch_resolves_a_bound_held_by_a_guarded_sibling() {
    let idx_var = reg_vn(0x10, 4);
    let function = guarded(
        RegisterSet::new().tracked(idx_var),
        |b| {
            let idx = b.read_variable(&idx_var).unwrap();
            // `idx + 2 < 4`, so idx is one of -2, -1, 0, 1.
            let shifted = at(b, idx, 2, ValueType::int(32)).unwrap();
            (below(b, shifted, 4), ())
        },
        true,
        |b, ()| {
            let idx = b.read_variable(&idx_var).unwrap();
            // The base carries the +2 the guard sits on: 0x4008 + 4*idx walks
            // 0x4000..0x400c over those four values.
            table_target(b, idx, 4, 0x4008)
        },
        COLLAPSE,
    );
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    assert_eq!(
        arms(classify(&function, Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40])
    );
}

/// A bounded operand under a constant offset enumerates only while the offset
/// keeps its range one unsigned interval: `(x & 7) + 1` names slots 1..8, while
/// `(x & 7) - 2` reaches below the table, where only guards the classifier
/// could not read would stop it, so it defers.
#[test]
fn classify_table_dispatch_defers_an_offset_that_wraps_the_bounded_operand() {
    let build = |offset: u64| {
        build_with_target(move |fb| {
            let raw = build_non_const_idx(fb);
            let seven = fb.build_int_const(7u64, ValueType::int(32)).unwrap();
            let masked = fb
                .build_int_binary_operation(raw, seven, IntBinaryOp::And, ValueType::int(32))
                .expect("and");
            let offset_c = fb.build_int_const(offset, ValueType::int(32)).unwrap();
            let index = fb
                .build_int_binary_operation(masked, offset_c, IntBinaryOp::Add, ValueType::int(32))
                .expect("add");
            table_target(fb, index, 4u64, 0x4000u64)
        })
    };
    let rom = MockRom::strided(0x3ff8, 4, (0..16).map(|i| 0x100 + i).collect(), 4);

    let (g, _) = build(1);
    assert_eq!(
        arms(classify(&g, Some(&rom))),
        Some((3..11).map(|i| 0x100 + i).collect::<Vec<_>>())
    );

    let (g, _) = build(0xffff_fffe);
    assert_eq!(classify(&g, Some(&rom)), None);
}

#[test]
fn classify_table_dispatch_with_if_guard_bound_returns_multiple() {
    // The guard path: idx is an unmasked register read bounded only by a
    // dominating `if (idx < 4)`.  `index_range` assumes converged IR, so the
    // single-predecessor dispatch region and trivial phis collapse first;
    // otherwise the guard never reaches the dispatch index.
    let idx_var = reg_vn(0x10, 4);
    let function = guarded(
        RegisterSet::new().tracked(idx_var),
        |b| {
            let idx = b.read_variable(&idx_var).unwrap();
            (below(b, idx, 4), ())
        },
        true,
        |b, ()| {
            let idx = b.read_variable(&idx_var).unwrap();
            table_target(b, idx, 4, 0x4000)
        },
        COLLAPSE,
    );
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    assert_eq!(
        arms(classify(&function, Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40])
    );
}

/// `idx` read from a register, dispatched through a four-entry table in a
/// region both arms of `if (idx == 0)` reach: `path_a` under `if (idx < 4)`,
/// `path_b` under the same guard when `guard_b`, unconditionally otherwise.
fn diamond(guard_b: bool) -> Function {
    let idx_var = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_var).build_fn().unwrap();
    let [entry, path_a, path_b, dispatch, exit_a, exit_b] =
        std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let idx_e = b.read_variable(&idx_var).unwrap();
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    let flag = b
        .build_int_cmp_operation(idx_e, zero, IntCmpOp::Equal, ValueType::int(32))
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    let guard = |b: &mut FunctionBuilder, exit| {
        let idx = b.read_variable(&idx_var).unwrap();
        let cond = below(b, idx, 4);
        b.build_if(cond, dispatch, exit).unwrap();
    };
    b.set_region(path_a);
    guard(&mut b, exit_a);
    b.set_region(path_b);
    if guard_b {
        guard(&mut b, exit_b);
    } else {
        b.build_branch(dispatch).unwrap();
    }

    b.set_region(dispatch);
    let idx_d = b.read_variable(&idx_var).unwrap();
    let loaded = table_target(&mut b, idx_d, 4, 0x4000);
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit_a);
    b.build_return(None, &[]).unwrap();
    if guard_b {
        b.set_region(exit_b);
        b.build_return(None, &[]).unwrap();
    }
    b.set_lift_addr(None);
    b.build().unwrap()
}

#[test]
fn classify_table_dispatch_diamond_both_paths_guarded_resolves() {
    // Both paths guard `idx < 4` and both true edges feed the SAME 2-pred
    // merge, so no single guard node dominates the dispatch.  The bound is
    // still proven: each edge carries it for the arm arriving on it.
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    assert_eq!(
        arms(classify(&diamond(true), Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40]),
        "exactly the four table entries, not a fifth read past them"
    );
}

#[test]
fn classify_table_dispatch_one_path_unguarded_does_not_resolve() {
    // Only ONE incoming path guards `idx < 4`; the other is unconditional.  The
    // index can exceed 4 there, so reading 4 entries would be out of bounds and
    // the dispatch must defer.  The guard's true successor IS `dispatch`, so
    // relying on reflexive dominance there makes both phi arms look bounded
    // and yields an out-of-bounds `Multiple`.
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    assert_eq!(classify(&diamond(false), Some(&rom)), None);
}

/// A loaded value fed DIRECTLY to the branch, with no address arithmetic, but
/// under a dominating `if (entry < 4)` guard.
///
/// Because the guard suppresses the width-only exclusion, this load-derived
/// target could be enumerated as the index, making the dispatch value literally
/// 0,1,2,3: bogus sequential targets that are not code addresses.  Returning
/// `Multiple([0,1,2,3])` here would be a wrong-edge bug; `None` means the
/// safety margin holds.
#[test]
fn classify_table_dispatch_guarded_direct_load_target() {
    let function = guarded(
        RegisterSet::new(),
        |b| {
            let loaded = build_non_const_idx(b);
            (below(b, loaded, 4), loaded)
        },
        true,
        // The SAME loaded value goes straight to the branch.
        |_, loaded| loaded,
        COLLAPSE,
    );
    assert_eq!(
        classify(&function, None),
        None,
        "a guarded direct-load target must not enumerate its index values as \
         branch targets"
    );
}

/// A cone containing a DECOY finite-range value off the addressing path
/// alongside the real masked index.  The classifier must pick the real index
/// or defer, never emit decoy-derived targets.
///
/// A candidate that does not fully determine the address fails to fold for at
/// least one value and is rejected.
#[test]
fn classify_table_dispatch_decoy_offpath_value_not_enumerated() {
    let idx_var = reg_vn(0x10, 4);
    let decoy_var = reg_vn(0x20, 4);
    // Guard the DECOY, not the real index.
    let function = guarded(
        RegisterSet::new().tracked(idx_var).tracked(decoy_var),
        |b| {
            let decoy = b.read_variable(&decoy_var).unwrap();
            (below(b, decoy, 4), ())
        },
        true,
        |b, ()| {
            let idx = b.read_variable(&idx_var).unwrap();
            masked_table_target(b, idx, 0x3, 4, 0x4000)
        },
        FOLD,
    );
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    // Substituting the decoy cannot change the dispatch: the real index
    // resolves, and no decoy-derived target escapes.
    assert_eq!(
        arms(classify(&function, Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40])
    );
}

fn finish_stack_array(mut fg: strider_ir::Function) -> (strider_ir::Function, ValueId) {
    run_passes(&mut fg, FOLD).unwrap();
    let load = fg.first_kind(|k| matches!(k, NodeKind::Load(_)));
    let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
    (fg, load_value)
}

/// The argument register a stack-array fixture indexes by.
fn stack_arg() -> rsleigh::Vn {
    reg_vn(0x38, 8)
}

/// A single-region 64-bit stack frame tracking [`stack_arg`], and its SP.
fn stack_frame() -> (FunctionBuilder, ValueId) {
    let b = sp_frame(sp64())
        .tracked(stack_arg())
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp64()).unwrap();
    (b, sp_val)
}

/// `frame_base` is the SP-derived base for every store and the load address:
/// either bare `sp_val` or an alignment-masked `And(sp_val, mask)`.
///
/// `targets.len()` must be a power of two: the index is masked with
/// `len - 1`, which is what bounds it to the table.
fn wire_stack_array(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
    frame_base: ValueId,
    b: &mut strider_ir::FunctionBuilder,
    arg_vn: rsleigh::Vn,
) {
    wire_stack_stores(targets, base_offset, stride, frame_base, b);
    wire_stack_dispatch(targets.len(), base_offset, stride, frame_base, b, arg_vn);
}

fn wire_stack_stores(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
    frame_base: ValueId,
    b: &mut strider_ir::FunctionBuilder,
) {
    for (i, &target_addr) in targets.iter().enumerate() {
        let off = base_offset + (i as i64) * (stride as i64);
        let addr = at(b, frame_base, off, ValueType::int(64)).unwrap();
        let target = b.build_int_const(target_addr, ValueType::int(64)).unwrap();
        b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
    }
}

/// `branch *(frame_base + base_offset + (arg & (entries - 1)) * stride)`,
/// terminating the current region.
fn wire_stack_dispatch(
    entries: usize,
    base_offset: i64,
    stride: u64,
    frame_base: ValueId,
    b: &mut strider_ir::FunctionBuilder,
    arg_vn: rsleigh::Vn,
) {
    let (i32t, i64t) = (ValueType::int(32), ValueType::int(64));
    let arg_val = b.read_variable(&arg_vn).unwrap();
    let arg_u32 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Truncate,
        [arg_val],
        [strider_ir::node::ValueKind::Typed(i32t)],
    );
    let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
    let masked = b
        .build_const_binop(entries as u64 - 1, arg_u32_value, IntBinaryOp::And, i32t)
        .unwrap();
    let idx_u64 = strider_ir_test_utils::sentinel_node(
        b.function_mut(),
        NodeKind::Extend(ExtendOp::ZeroExtend),
        [masked],
        [strider_ir::node::ValueKind::Typed(i64t)],
    );
    let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
    let idx_scaled = b
        .build_const_binop(stride, idx_u64_value, IntBinaryOp::Mul, i64t)
        .unwrap();
    let sp_plus_base = at(b, frame_base, base_offset, i64t).unwrap();
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, i64t)
        .unwrap();
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, i64t)
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
}

fn build_stack_array(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
) -> (strider_ir::Function, ValueId) {
    let (mut b, sp_val) = stack_frame();
    wire_stack_array(targets, base_offset, stride, sp_val, &mut b, stack_arg());
    finish_stack_array(b.build().unwrap())
}

/// Like `build_stack_array` but with an alignment-masked frame base, which
/// `decompose` treats as a fresh opaque stack base.
fn build_stack_array_aligned(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
) -> (strider_ir::Function, ValueId) {
    let (mut b, sp_val) = stack_frame();
    let frame_base = b
        .build_const_binop(!0xFu64, sp_val, IntBinaryOp::And, ValueType::int(64))
        .unwrap();
    wire_stack_array(
        targets,
        base_offset,
        stride,
        frame_base,
        &mut b,
        stack_arg(),
    );
    finish_stack_array(b.build().unwrap())
}

/// A two-entry stack label array resolves to its labels: off the bare SP,
/// off an `And`-aligned one (the evaluator must take the masked base as an
/// SP terminal so the store probe matches the prologue stores), and with
/// the dispatch load below a `MemPhi` (the set must not move when the map
/// crosses the join).
#[test]
fn classify_table_dispatch_two_stack_targets_resolves() {
    type Build = fn(&[u64], i64, u64) -> (Function, ValueId);
    let targets = [0x401190u64, 0x401180u64];
    let rows: [(&str, Build); 3] = [
        ("bare", build_stack_array),
        ("aligned", build_stack_array_aligned),
        ("across a MemPhi", build_stack_array_across_mem_phi),
    ];
    for (name, build) in rows {
        let (fg, _load_value) = build(&targets, -24, 8);
        assert_eq!(
            classify(&fg, None),
            Some(ResolvedTargets::table(
                targets.map(strider_cfg::ResolvedTarget::from)
            )),
            "{name}"
        );
    }
}

/// A stack label array whose dispatch load sits below a `MemPhi`: at a join the
/// reaching store is path-dependent, so the straight-line segment ends at the
/// phi and the map has to descend into its arms.
fn build_stack_array_across_mem_phi(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
) -> (strider_ir::Function, ValueId) {
    let sp = sp64();
    let arg_vn = stack_arg();
    let mut b = sp_frame(sp).tracked(arg_vn).build_fn().unwrap();
    let [entry, arm, join] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let sp_entry = b.read_variable(&sp).unwrap();
    wire_stack_stores(targets, base_offset, stride, sp_entry, &mut b);
    let arg_entry = b.read_variable(&arg_vn).unwrap();
    let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
    let flag = b
        .build_int_cmp_operation(arg_entry, zero, IntCmpOp::Equal, ValueType::int(64))
        .unwrap();
    b.build_if(flag, arm, join).unwrap();

    // One arm writes the slot just below the array, so the arms disagree and
    // the `MemPhi` survives while both still reach the label stores.
    b.set_region(arm);
    let sp_arm = b.read_variable(&sp).unwrap();
    let spill_addr = at(&mut b, sp_arm, base_offset - 8, ValueType::int(64)).unwrap();
    let junk = b.build_int_const(0xDEADu64, ValueType::int(64)).unwrap();
    b.build_store(spill_addr, junk, rsleigh::VnSpace::RAM)
        .unwrap();
    b.build_branch(join).unwrap();

    b.set_region(join);
    let sp_join = b.read_variable(&sp).unwrap();
    wire_stack_dispatch(targets.len(), base_offset, stride, sp_join, &mut b, arg_vn);

    let (fg, load_value) = finish_stack_array(b.build().unwrap());
    assert!(
        mem_chain_crosses_phi(&fg, fg.producer(load_value)),
        "the dispatch load must sit below a MemPhi for this to test the fallback"
    );
    (fg, load_value)
}

fn mem_chain_crosses_phi(fg: &Function, load: NodeId) -> bool {
    let mut cur = fg.memory_input_of(load);
    while let Some(mem) = cur {
        let node = fg.producer(mem);
        match fg.node_kind(node) {
            NodeKind::MemPhi => return true,
            NodeKind::InitialMemory => return false,
            _ => cur = fg.memory_input_of(node),
        }
    }
    false
}

/// A global store between the prologue stores and the dispatch load is exactly
/// what [`AssumptionOptions::stack_global_disjoint`](crate::AssumptionOptions)
/// governs: set, it proves the store disjoint from the SP-rooted array and
/// resolves; clear, it cannot and defers.
///
/// Both assertions run on copies of the SAME graph.
#[test]
fn classify_table_dispatch_global_store_between_resolves_only_under_disjoint() {
    let (mut b, sp_val) = stack_frame();
    let targets = [0x401190u64, 0x401180u64];
    wire_stack_stores(&targets, -24, 8, sp_val, &mut b);
    // A constant absolute address, unrelated to the stack pointer.
    let global_addr = b
        .build_int_const(0x0060_0000u64, ValueType::int(64))
        .unwrap();
    let global_val = b
        .build_int_const(0x0000_DEADu64, ValueType::int(64))
        .unwrap();
    b.build_store(global_addr, global_val, rsleigh::VnSpace::RAM)
        .unwrap();
    wire_stack_dispatch(targets.len(), -24, 8, sp_val, &mut b, stack_arg());
    let (fg, _load_value) = finish_stack_array(b.build().unwrap());

    // Claimed, the global store is proven disjoint.
    let claimed = crate::AssumptionOptions {
        stack_global_disjoint: true,
        ..crate::AssumptionOptions::none()
    };
    assert_eq!(
        classify_with(&fg, None, &claimed, None),
        Some(ResolvedTargets::table(
            targets.map(strider_cfg::ResolvedTarget::from)
        )),
        "StackGlobalDisjoint proves the global store disjoint from the \
         SP-rooted array; the table must resolve",
    );

    // Cleared, it cannot, so the store surfaces as a clobber.
    assert_eq!(
        classify(&fg, None),
        None,
        "cleared, it cannot prove the global store disjoint from the SP-rooted \
         array; the intervening store is a clobber and the branch must defer",
    );
}

/// A `Call` between the prologue stores and the dispatch load is a clobber
/// boundary: the slots are no longer provably the stored constants, so the
/// classifier must defer.
#[test]
fn classify_table_dispatch_returns_none_when_call_clobbers_between_stores_and_load() {
    let (mut b, sp_val) = stack_frame();
    let targets = [0x401190u64, 0x401180u64];
    wire_stack_stores(&targets, -24, 8, sp_val, &mut b);
    // Clobbers memory, including the slots the callee can reach through SP.
    let call_target_const = b
        .build_int_const(0x0040_1000u64, ValueType::int(64))
        .unwrap();
    b.build_call_cc(call_target_const, None).unwrap();
    // Re-read sp: the call may have advanced the stack.
    let sp_val_after = b.read_variable(&sp64()).unwrap();
    wire_stack_dispatch(targets.len(), -24, 8, sp_val_after, &mut b, stack_arg());
    let (fg, _load_value) = finish_stack_array(b.build().unwrap());
    assert_eq!(
        classify(&fg, None),
        None,
        "Call between stores and dispatch load is a clobber boundary; \
         classifier must return None (conservative)"
    );
}

#[test]
fn classify_table_dispatch_returns_none_on_non_indexed_load() {
    let sp = sp64();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let off = b.build_int_const(24u64, ValueType::int(64)).unwrap();
    let addr = b
        .build_sub_as_add_neg(sp_val, off, ValueType::int(64))
        .unwrap();
    let v = b.build_int_const(0xCAFEu64, ValueType::int(64)).unwrap();
    b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let (fg, _load_value) = finish_stack_array(b.build().unwrap());
    assert_eq!(classify(&fg, None), None);
}

#[test]
fn classify_table_dispatch_returns_none_on_unbounded_stack_idx() {
    let i64t = ValueType::int(64);
    let (mut b, sp_val) = stack_frame();
    let addr_24 = at(&mut b, sp_val, -24, i64t).unwrap();
    let v = b.build_int_const(0x1234u64, i64t).unwrap();
    b.build_store(addr_24, v, rsleigh::VnSpace::RAM).unwrap();
    let arg_val = b.read_variable(&stack_arg()).unwrap();
    let idx_scaled = b
        .build_const_binop(8u64, arg_val, IntBinaryOp::Mul, i64t)
        .unwrap();
    let sp_plus_base = at(&mut b, sp_val, -24, i64t).unwrap();
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, i64t)
        .unwrap();
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, i64t)
        .unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
    let (fg, _load_value) = finish_stack_array(b.build().unwrap());
    assert_eq!(classify(&fg, None), None);
}

/// A one-element stack array: ConstantFold folds `arg & 0` away, leaving a
/// load at a fixed slot with no index to enumerate, so the table path defers
/// (LoadForward is what forwards that slot).
/// Mutation: enumerate an index-free cone once at `0`.
#[test]
fn classify_table_dispatch_one_stack_target_defers() {
    let (fg, _load_value) = build_stack_array(&[0x401200], -8, 8);
    assert_eq!(classify(&fg, None), None);
}

/// `branch *(table + (arg & 1) * 4)` at `I32`, `arg` read from `arg_vn`.
fn two_entry_dispatch(b: &mut FunctionBuilder, arg_vn: rsleigh::Vn, table: ValueId) {
    let i32t = ValueType::int(32);
    let arg = b.read_variable(&arg_vn).unwrap();
    let idx = b
        .build_const_binop(1u64, arg, IntBinaryOp::And, i32t)
        .unwrap();
    let scaled = b
        .build_const_binop(4u64, idx, IntBinaryOp::Mul, i32t)
        .unwrap();
    let addr = b
        .build_int_binary_operation(table, scaled, IntBinaryOp::Add, i32t)
        .unwrap();
    let loaded = b.build_load(addr, VnSpace::RAM, i32t).unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_lift_addr(None);
}

/// A single-region 32-bit stack frame tracking the argument `reg_vn(0x8, 4)`,
/// its SP and the stores of the labels `0x401190`, `0x401180` at `sp + off`
/// for each of `offs`.
fn stack_frame32(offs: [i64; 2]) -> (FunctionBuilder, ValueId, rsleigh::Vn) {
    let sp = sp32_vn();
    let arg_vn = reg_vn(0x8, 4);
    let mut b = sp_frame(sp)
        .tracked(arg_vn)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    for (off, target) in offs.into_iter().zip([0x401190u64, 0x401180]) {
        let addr = at(&mut b, sp_val, off, ValueType::int(32)).unwrap();
        let data = b.build_int_const(target, ValueType::int(32)).unwrap();
        b.build_store(addr, data, VnSpace::RAM).unwrap();
    }
    (b, sp_val, arg_vn)
}

/// The two labels [`stack_frame32`] stores, as a table.
fn two_labels() -> Option<ResolvedTargets> {
    Some(ResolvedTargets::table([
        0x401180u64.into(),
        0x401190u64.into(),
    ]))
}

/// A 32-bit two-entry stack table at `sp + table`, then, for `newer` of
/// `(off, ty)`, a newer `ty` store at `sp + off`.
fn build_stack_table_across_the_wrap(table: i64, newer: Option<(i64, ValueType)>) -> Function {
    let (mut b, sp_val, arg_vn) = stack_frame32([table, table + 4]);
    if let Some((off, ty)) = newer {
        let addr = at(&mut b, sp_val, off, ValueType::int(32)).unwrap();
        let data = b.build_int_const(0xDEAD_BEEFu64, ty).unwrap();
        b.build_store(addr, data, VnSpace::RAM).unwrap();
    }
    let table = at(&mut b, sp_val, table, ValueType::int(32)).unwrap();
    two_entry_dispatch(&mut b, arg_vn, table);
    finish_stack_array(b.build().unwrap()).0
}

/// A two-entry stack table across the 32-bit wrap, under a newer store.
///
/// - At `sp + 0x8000_0000`, which the evaluator holds at `-2^31`, a store at
///   `sp + 0x7FFF_FFFE` has bytes past `2^31` that are entry 0's mod 2^32.
/// - At `sp + 0x7FFF_FFFA`, entry 1 runs across the wrap, and a 2-byte store
///   at `sp + 0x8000_0000` overwrites its top half mod 2^32.
/// - A store six bytes above the last entry is disjoint from both, which only
///   a modular offset comparison places correctly.
#[test]
fn classify_table_dispatch_sees_a_store_wrapping_onto_a_slot() {
    let i32t = ValueType::int(32);
    let rows = [
        ("control across the signed wrap", 0x8000_0000, None, true),
        (
            "onto entry 0",
            0x8000_0000,
            Some((0x7FFF_FFFE, i32t)),
            false,
        ),
        ("control straddling the wrap", 0x7FFF_FFFA, None, true),
        (
            "onto entry 1",
            0x7FFF_FFFA,
            Some((0x8000_0000, ValueType::int(16))),
            false,
        ),
        ("disjoint", 0x7FFF_FFFA, Some((0x8000_0004, i32t)), true),
    ];
    for (name, table, newer, resolves) in rows {
        assert_eq!(
            classify(&build_stack_table_across_the_wrap(table, newer), None),
            if resolves { two_labels() } else { None },
            "{name}"
        );
    }
}

/// The same shape at the probe level: the store probe reaches each table
/// store through the newer one, which `offset_order` places on the ring six
/// bytes above the last entry instead of 2^32 - 6 below it.
#[test]
fn the_shared_climb_reaches_past_a_disjoint_store_across_the_wrap() {
    use crate::mem_analysis::{MemQuery, Policy, decompose};

    let f = build_stack_table_across_the_wrap(0x7FFF_FFFA, Some((0x8000_0004, ValueType::int(32))));
    let load = f.sole_kind(|k| matches!(k, NodeKind::Load(_)));
    let mem = f.node_inputs(load)[NodeKind::ACCESS_MEM_SLOT];
    let newest = f
        .walk_kind(|k| matches!(k, NodeKind::Store(_)))
        .last()
        .expect("the newer store");
    let base = decompose(&f, f.store_addr(newest))
        .expect("an SP-rooted store")
        .base;
    let query = MemQuery::default();
    // The two entries, reduced: `sp + 0x7FFF_FFFA` and `sp + 0x7FFF_FFFE`.
    for (offset, target) in [(0x7FFF_FFFAi128, 0x401190u128), (0x7FFF_FFFE, 0x401180)] {
        let clobber =
            query.nearest_sp_clobber(&f, Policy::strict(&f), mem, base, offset, 4, VnSpace::RAM);
        let hit =
            crate::mem_analysis::anchored_store(&f, clobber, base).expect("the entry's own store");
        assert_eq!(hit.store_offset, offset, "anchored at the probed entry");
        assert_eq!(
            f.int_const_u128(hit.data(&f)),
            Some(target),
            "entry at {offset:#x} still holds its target"
        );
    }
}

/// A two-entry 32-bit table at `(sp + 0x7FFF_FFF0) + 0x20`, the second hop
/// carrying the probe past `2^31`, stored through one constant each.  No
/// `ConstantFold`, which would merge the two hops into one constant.
fn stack_table_built_in_two_hops() -> Function {
    let (mut b, sp_val, arg_vn) = stack_frame32([0x8000_0010, 0x8000_0014]);
    let near_top = at(&mut b, sp_val, 0x7FFF_FFF0, ValueType::int(32)).unwrap();
    let table = at(&mut b, near_top, 0x20, ValueType::int(32)).unwrap();
    two_entry_dispatch(&mut b, arg_vn, table);
    let mut fg = b.build().unwrap();
    run_passes(&mut fg, &[&KnownBits, &PhiCollapse, &RegionCollapse]).unwrap();
    fg
}

/// The evaluator reduces each hop, so a probe built in two hops past `2^31`
/// names the slot the store's single constant does.
#[test]
fn classify_table_dispatch_reduces_a_probe_built_in_two_hops() {
    assert_eq!(
        classify(&stack_table_built_in_two_hops(), None),
        two_labels()
    );
}

#[test]
fn lock_barrier_prevents_stack_load_forwarding() -> crate::Result<()> {
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    let sp = sp32_vn();
    let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, ValueType::int(32))?;
        let addr = b.build_sub_as_add_neg(sp_val, four, ValueType::int(32))?;
        let data = b.build_int_const(0x99u64, ValueType::int(32))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        // LOCK is a FullClobber, so the Stack chain must break here.
        let (lock_node, _outputs) =
            strider_ir_test_utils::Tb::named_call_other(b, 0x1234, "LOCK", &[], &[], false, false)?;
        let lock_mem_value = b.function().memory_output_of(lock_node)?;
        b.advance_cur_region_memory(lock_mem_value)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    run_passes(&mut fg, STANDARD)?;

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp-4] must NOT be forwarded across a LOCK barrier; \
         LOCK is FullClobber and breaks the Stack chain"
    );
    Ok(())
}

/// The interval carrier is a `u128`, so a wider type's "top" is not top and
/// every interval at that type looks narrowed.  Enumerating one would pin an
/// index whose real value set the range never covered.  `bounded_index` keeps
/// one out by taking the type mask, which a wider type has none of.
#[test]
fn an_index_bound_needs_a_type_inside_the_u128_carrier() {
    let iv = super::super::index_range::Interval::dense(0, 7);
    let mask = |bits| ValueType::int(bits).int_mask_u128();
    assert!(mask(32).is_some_and(|mask| index_bound_ok(mask, iv)));
    assert_eq!(mask(256), None);
    assert_eq!(mask(512), None);
}

/// Values `spacing` apart taken modulo `2^bits`.
/// Mutation: keep `spacing` as the shared power of two.
#[test]
fn wrap_counts_a_progression_modulo_its_width() {
    assert_eq!(wrap(4, 3, 0xff), Some((4, 3)), "fits");
    assert_eq!(wrap(1 << 32, 1, 0xff), Some((256, 1)), "a truncate");
    assert_eq!(wrap(256, 4, 0xff), Some((64, 4)), "a widening shift");
    assert_eq!(
        wrap(256, 3, 0xff),
        Some((256, 1)),
        "odd steps cover every value"
    );
    assert_eq!(wrap(256, 6, 0xff), Some((128, 2)));
    assert_eq!(wrap(100, 3, 0xff), None, "wraps before covering the cycle");
    assert_eq!(wrap(256, 1, u128::MAX), Some((256, 1)), "past the carrier");
}

/// `entry / 3` holds 86 values; a guard `entry / 3 < 200` leaves it a range
/// of 200, looser than the cell's own value count, which is no narrowing.
/// Mutation: count only a range exactly as wide as the cell.
#[test]
fn classify_table_dispatch_excludes_a_table_entry_under_a_looser_guard() {
    let function = guarded(
        RegisterSet::new(),
        |b| {
            let wide = cell(b, ValueType::int(8));
            let third = b
                .build_const_binop(3u64, wide, IntBinaryOp::Div, ValueType::int(32))
                .unwrap();
            (below(b, third, 200), third)
        },
        true,
        |b, third| table_target(b, third, 4, 0x4000),
        COLLAPSE,
    );
    let rom = MockRom::strided(0x4000, 4, (0..256).map(|i| 0x5000 + i).collect(), 4);
    assert_eq!(classify(&function, Some(&rom)), None);
}

#[test]
fn classify_table_dispatch_resolves_a_bitfield_index_under_its_guard() {
    // ARM `ubfx r3, r0, #27, #2; cmp r3, #3; ldrls pc, [pc, r3, lsl #2]`
    // (linux 6.1 `arch_bp_generic_fields`): the index lifts as
    // `(r0 << 3) >> 30`, four values of a register, not a table cell.
    let r0 = reg_vn(0x20, 4);
    let i32t = ValueType::int(32);
    let g = guarded(
        RegisterSet::new().tracked(r0).arg(r0),
        |b| {
            let arg = b.read_variable(&r0).unwrap();
            let up = b
                .build_const_binop(3u64, arg, IntBinaryOp::ShiftLeft, i32t)
                .unwrap();
            let idx = b
                .build_const_binop(30u64, up, IntBinaryOp::ShiftRight, i32t)
                .unwrap();
            let three = b.build_int_const(3u64, i32t).unwrap();
            let cond = b
                .build_int_cmp_operation(three, idx, IntCmpOp::Less, i32t)
                .unwrap();
            (cond, idx)
        },
        false,
        |b, idx| table_target(b, idx, 4, 0x4000),
        COLLAPSE,
    );
    let rom = MockRom::strided(0x4000, 4, (0..32).map(|i| 0x5000 + 0x10 * i).collect(), 4);
    assert_eq!(
        classify(&g, Some(&rom)),
        Some(ResolvedTargets::table(
            [0x5000u64, 0x5010, 0x5020, 0x5030].map(strider_cfg::ResolvedTarget::from)
        )),
    );
}

/// One guard of [`classify_guarded_dispatch`]: the `If` condition over the
/// `I32` word.
type WordCond<'a> = &'a dyn Fn(&mut FunctionBuilder, ValueId) -> ValueId;

/// `ld pc, 0x4000(index(w) << 3)` over a 64-slot table at 0x4000, reached
/// only once every guard in `guards`, in order, holds on the `I32` word `w`.
fn classify_guarded_dispatch(
    guards: &[WordCond<'_>],
    index: fn(&mut FunctionBuilder, ValueId) -> ValueId,
) -> Option<ResolvedTargets> {
    let i64t = ValueType::int(64);
    let w = reg_vn(0x20, 4);
    let mut b = RegisterSet::new().tracked(w).arg(w).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let word = b.read_variable(&w).unwrap();
    for guard in guards {
        let next = b.create_region_all().unwrap();
        let cond = guard(&mut b, word);
        b.build_if(cond, next, exit).unwrap();
        b.set_region(next);
    }
    let idx = index(&mut b, word);
    let scaled = b
        .build_const_binop(3u64, idx, IntBinaryOp::ShiftLeft, i64t)
        .unwrap();
    let addr = b
        .build_const_binop(0x4000u64, scaled, IntBinaryOp::Add, i64t)
        .unwrap();
    let loaded = b.build_load(addr, VnSpace::RAM, i64t).unwrap();
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut g = b.build().unwrap();
    // `index_range` assumes converged IR.
    run_passes(&mut g, COLLAPSE).unwrap();
    let rom = MockRom::strided(0x4000, 8, (0..64).map(|i| 0x5000 + 0x10 * i).collect(), 8);
    classify(&g, Some(&rom))
}

/// [`classify_guarded_dispatch`] of the `ext` index under one `srl` guard.
fn classify_bit_field_dispatch(lo: u64, bound: u64) -> Option<ResolvedTargets> {
    classify_guarded_dispatch(&[&|b, w| srl_sltiu(b, w, lo, bound)], ext_26_6)
}

/// `zext64((w >> lo) & 7)`, a field of three bits.
fn bits_of(b: &mut FunctionBuilder, w: ValueId, lo: u64) -> ValueId {
    let i32t = ValueType::int(32);
    let up = b
        .build_const_binop(lo, w, IntBinaryOp::ShiftRight, i32t)
        .unwrap();
    let field = b
        .build_const_binop(7u64, up, IntBinaryOp::And, i32t)
        .unwrap();
    b.extend_if_needed(field, ValueType::int(64), ExtendOp::ZeroExtend)
        .unwrap()
}

/// [`bits_of`] at 4.
fn bits_4_to_7(b: &mut FunctionBuilder, w: ValueId) -> ValueId {
    bits_of(b, w, 4)
}

/// The arms of slots `0..n` of [`classify_guarded_dispatch`]'s table.
fn first_slots(n: u64) -> Option<ResolvedTargets> {
    slots(&(0..n).collect::<Vec<_>>())
}

/// `if (w < 100000) switch ((w >> 4) & 7)`: the guard on the whole word
/// allows every value of the field, so it sizes nothing and the field's own
/// eight slots are the table.  `w >> 4` itself spans more slots than a table
/// may, so only the field bounds the index.
#[test]
fn classify_table_dispatch_reads_a_field_under_a_looser_word_guard() {
    assert_eq!(
        classify_guarded_dispatch(&[&|b, w| below(b, w, 100_000)], bits_4_to_7),
        first_slots(8),
    );
}

/// `if (w < 20) switch ((w >> 4) & 7)`: the field is at most `19 >> 4`, so
/// a two-entry table, and its eight-value field bound reads six slots past it.
#[test]
fn classify_table_dispatch_projects_a_word_guard_onto_its_field() {
    assert_eq!(
        classify_guarded_dispatch(&[&|b, w| below(b, w, 20)], bits_4_to_7),
        first_slots(2),
    );
}

/// `x - lo < span` at the type of `x`: `x` in `[lo, lo + span)`.
fn in_span(b: &mut FunctionBuilder, x: ValueId, lo: u64, span: u64) -> ValueId {
    let ty = b.value_type_opt(x).unwrap();
    let sum = at(b, x, -(lo as i64), ty).unwrap();
    below(b, sum, span)
}

/// [`bits_of`] at 1.
fn bits_1_to_3(b: &mut FunctionBuilder, w: ValueId) -> ValueId {
    bits_of(b, w, 1)
}

/// ppc64 `rldicl r3,r3,36,32; cmplwi r3,10; bgt; rldic r3,r3,2,58`: the guard
/// reads `trunc32(rot)` and the index `rot & 0xffffffff`, both of
/// `rot = Or(w << 36, w >> 28)`, so the guard's 11 slots bound the table.
/// Mutation: leave `Truncate(Or)` rooted at the `Or` (16 slots).
#[test]
fn classify_table_dispatch_bounds_a_masked_rotate_by_its_truncated_guard() {
    let i64t = ValueType::int(64);
    let w = reg_vn(0x20, 8);
    let mut b = RegisterSet::new().tracked(w).arg(w).build_fn().unwrap();
    let [entry, exit, next] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let word = b.read_variable(&w).unwrap();
    let hi = b
        .build_const_binop(36u64, word, IntBinaryOp::ShiftLeft, i64t)
        .unwrap();
    let lo = b
        .build_const_binop(28u64, word, IntBinaryOp::ShiftRight, i64t)
        .unwrap();
    let rot = b
        .build_int_binary_operation(hi, lo, IntBinaryOp::Or, i64t)
        .unwrap();
    let low32 = b.truncate_if_needed(rot, ValueType::int(32)).unwrap();
    let wide = b
        .extend_if_needed(low32, i64t, ExtendOp::ZeroExtend)
        .unwrap();
    let cond = below(&mut b, wide, 11);
    b.build_if(cond, next, exit).unwrap();
    b.set_region(next);
    let masked = b
        .build_const_binop(0xffff_ffffu64, rot, IntBinaryOp::And, i64t)
        .unwrap();
    let idx = b
        .build_const_binop(1u64, masked, IntBinaryOp::ShiftLeft, i64t)
        .unwrap();
    let idx = b
        .build_const_binop(0x1eu64, idx, IntBinaryOp::And, i64t)
        .unwrap();
    let scaled = b
        .build_const_binop(2u64, idx, IntBinaryOp::ShiftLeft, i64t)
        .unwrap();
    let addr = b
        .build_const_binop(0x4000u64, scaled, IntBinaryOp::Add, i64t)
        .unwrap();
    let loaded = b.build_load(addr, VnSpace::RAM, i64t).unwrap();
    b.build_indirect_branch(loaded).unwrap();
    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    run_passes(&mut f, COLLAPSE).unwrap();
    let rom = MockRom::strided(0x4000, 8, (0..64).map(|i| 0x5000 + 0x10 * i).collect(), 8);
    assert_eq!(classify(&f, Some(&rom)), first_slots(11));
}

/// [`classify_guarded_dispatch`]'s arms for these slots.
fn slots(of: &[u64]) -> Option<ResolvedTargets> {
    Some(ResolvedTargets::table(of.iter().map(|i| {
        strider_cfg::ResolvedTarget::from(0x5000 + 0x10 * i)
    })))
}

/// `w` in `[10, 17]` puts `(w >> 1) & 7` in `{5, 6, 7, 0}`: the guard's low
/// end does not carry to a field below the top of its bits, whose values wrap,
/// so slots 1 to 4 are never read.  `w >> 1` in `[5, 8]` is what is
/// enumerated.
#[test]
fn classify_table_dispatch_reads_no_slot_below_a_wrapping_field() {
    assert_eq!(
        classify_guarded_dispatch(&[&|b, w| in_span(b, w, 10, 8)], bits_1_to_3),
        slots(&[0, 5, 6, 7]),
    );
}

/// The same field under `trunc16(w)` in `[10, 17]`: nothing below the field
/// is bounded either, so the guard bounds no candidate and the site defers.
#[test]
fn classify_table_dispatch_defers_a_field_under_a_guard_low_end_it_cannot_carry() {
    let low_half = |b: &mut FunctionBuilder, w: ValueId| {
        let half = b.truncate_if_needed(w, ValueType::int(16)).unwrap();
        in_span(b, half, 10, 8)
    };
    assert_eq!(classify_guarded_dispatch(&[&low_half], bits_1_to_3), None);
}

/// `w & 15` in `[10, 13]` puts its top two bits, `(w & 15) >> 2`, in
/// `[2, 3]`: at the top of the guarded bits the low end carries.
#[test]
fn classify_table_dispatch_carries_a_guard_low_end_to_the_top_of_its_bits() {
    let nibble = |b: &mut FunctionBuilder, w: ValueId| {
        b.build_const_binop(15u64, w, IntBinaryOp::And, ValueType::int(32))
            .unwrap()
    };
    let top_two = |b: &mut FunctionBuilder, w: ValueId| {
        let low = b
            .build_const_binop(15u64, w, IntBinaryOp::And, ValueType::int(32))
            .unwrap();
        let top = b
            .build_const_binop(2u64, low, IntBinaryOp::ShiftRight, ValueType::int(32))
            .unwrap();
        b.extend_if_needed(top, ValueType::int(64), ExtendOp::ZeroExtend)
            .unwrap()
    };
    assert_eq!(
        classify_guarded_dispatch(
            &[&|b, w| {
                let low = nibble(b, w);
                in_span(b, low, 10, 4)
            }],
            top_two
        ),
        slots(&[2, 3]),
    );
}

/// gcc's `__mm_isBranchInstr` on mips64: the `srl` guard sizes the table at 62
/// entries and the `ext` index is the same six bits, so slots 62 and 63, the
/// next table's first entries, are never read.
#[test]
fn classify_table_dispatch_bounds_an_ext_index_by_its_srl_guard() {
    assert_eq!(classify_bit_field_dispatch(26, 62), first_slots(62));
}

/// A guard on bits 27.. of the word bounds bits the index shares, in a shape
/// nothing connects to it, so the index has only its 64-value field bound.
/// The guard may be the one sizing the table: the site defers rather than
/// read slots past it.
#[test]
fn classify_table_dispatch_defers_a_field_bound_beside_an_unconnected_guard() {
    assert_eq!(classify_bit_field_dispatch(27, 30), None);
}

/// A guard the index's own field carries but that bounds nothing, `w >> 26 <
/// 64`, leaves the index its 64-value field bound beside the unconnected
/// guard on bits 27..: the site still defers.
#[test]
fn classify_table_dispatch_defers_beside_an_unconnected_guard_under_a_loose_one() {
    assert_eq!(
        classify_guarded_dispatch(
            &[&|b, w| srl_sltiu(b, w, 26, 64), &|b, w| srl_sltiu(
                b, w, 27, 30
            ),],
            ext_26_6,
        ),
        None,
    );
}

/// A read guard on the index's own field, `w >> 26 < 63`, narrows it below
/// its 64-value bound but not to what the unconnected guard on bits 27..,
/// `w >> 27 < 30`, allows: slots 60 to 62 are never read, so the site
/// defers rather than seat them.
/// Mutation: consult an unconnected guard only beside a KnownBits bound.
#[test]
fn classify_table_dispatch_defers_beside_an_unconnected_guard_under_a_read_one() {
    assert_eq!(
        classify_guarded_dispatch(
            &[&|b, w| srl_sltiu(b, w, 26, 63), &|b, w| srl_sltiu(
                b, w, 27, 30
            ),],
            ext_26_6,
        ),
        None,
    );
}

/// `w >> 26 < 4` leaves the index only bits 26 and 27 to vary, which the
/// unconnected guard on bits 29.., `w >> 29 < 5`, does not touch.
/// Mutation: weigh an unconnected guard against the whole field.
#[test]
fn classify_table_dispatch_seats_beside_a_guard_on_bits_a_read_one_clears() {
    assert_eq!(
        classify_guarded_dispatch(
            &[&|b, w| srl_sltiu(b, w, 26, 4), &|b, w| srl_sltiu(
                b, w, 29, 5
            ),],
            ext_26_6,
        ),
        first_slots(4),
    );
}

/// `t[idx & mask]` over interworking words at 0x4000: the masked target and
/// the mode the branch commits.
fn interworking_table(mask: u64) -> (Function, Option<ValueId>) {
    let mut mode = None;
    let (g, _) = build_with_target(|fb| {
        let raw = build_non_const_idx(fb);
        let word = masked_table_target(fb, raw, mask, 4, 0x4000);
        let (target, m) = split_mode(fb, word);
        mode = Some(m);
        target
    });
    (g, mode)
}

#[test]
fn classify_table_dispatch_conflicting_isa_modes_on_one_addr_defers() {
    // An interworking table whose two words are `X` and `X | 1`: the target is
    // the mode-bit-masked word, so both entries land on ONE address carrying
    // opposite committed modes.  Keeping either would decode the arm in a mode
    // half the table contradicts, and no mode at all is the FLOWING mode, which
    // a mode-switching branch contradicts by construction.
    let (g, mode_value) = interworking_table(1);
    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x1001], 4);
    assert_eq!(
        classify_with(
            &g,
            Some(&rom),
            &crate::AssumptionOptions::default(),
            mode_value
        ),
        None,
        "conflicting modes on one address must defer the whole site"
    );
}

/// `s = arg == 1 ? k0 : (arg == 2 ? k1 : k2)` over `ks`, then
/// `dispatch(s)` in the join, under a guard `(op, c, taken)` where one is
/// given: `s op c` on the edge `taken` names.  `dispatch` builds the branch
/// target and, optionally, the ISA mode the branch commits.
fn phi_of_constants_dispatch(
    ks: [u64; 3],
    guard: Option<(IntCmpOp, u64, bool)>,
    dispatch: impl FnOnce(&mut FunctionBuilder, ValueId) -> (ValueId, Option<ValueId>),
) -> Function {
    let s_vn = reg_vn(0x10, 4);
    let arg_vn = reg_vn(0x20, 4);
    let i32t = ValueType::int(32);
    let mut b = RegisterSet::new()
        .tracked(s_vn)
        .tracked(arg_vn)
        .arg(arg_vn)
        .build_fn()
        .unwrap();
    let [entry, mid, a, arm_b, arm_c, join, at, exit] =
        std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    let arg_is = |b: &mut FunctionBuilder, k: u64| {
        let arg = b.read_variable(&arg_vn).unwrap();
        let k = b.build_int_const(k, i32t).unwrap();
        b.build_int_cmp_operation(arg, k, IntCmpOp::Equal, i32t)
            .unwrap()
    };
    b.set_region(entry);
    let x = arg_is(&mut b, 1);
    b.build_if(x, a, mid).unwrap();
    b.set_region(mid);
    let y = arg_is(&mut b, 2);
    b.build_if(y, arm_b, arm_c).unwrap();
    for (region, k) in [(a, ks[0]), (arm_b, ks[1]), (arm_c, ks[2])] {
        b.set_region(region);
        let k = b.build_int_const(k, i32t).unwrap();
        b.write_variable(&s_vn, k).unwrap();
        b.build_branch(join).unwrap();
    }
    b.set_region(join);
    match guard {
        Some((op, c, taken)) => {
            let s = b.read_variable(&s_vn).unwrap();
            let c = b.build_int_const(c, i32t).unwrap();
            let cond = b.build_int_cmp_operation(s, c, op, i32t).unwrap();
            let (t, e) = if taken { (at, exit) } else { (exit, at) };
            b.build_if(cond, t, e).unwrap();
        }
        None => b.build_branch(at).unwrap(),
    }
    b.set_region(at);
    let s = b.read_variable(&s_vn).unwrap();
    let (target, mode) = dispatch(&mut b, s);
    b.build_indirect_branch_with_mode(target, mode).unwrap();
    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    run_passes(&mut f, COLLAPSE).unwrap();
    f
}

/// The arms of a `t[s]` dispatch over a six-entry table at `0x4000`.
fn phi_index_arms(bound: Option<u64>) -> Option<Vec<u64>> {
    let guard = bound.map(|c| (IntCmpOp::Less, c, true));
    let f = phi_of_constants_dispatch([0, 3, 5], guard, |b, s| {
        (table_target(b, s, 4, 0x4000), None)
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60], 4);
    arms(classify(&f, Some(&rom)))
}

/// `s` in {0, 3, 5}: `t[0]`, `t[3]` and `t[5]`, not the six slots of the
/// range 0..=5.  Mutation: enumerate the range where the phi's constants are
/// known.
#[test]
fn classify_table_dispatch_reads_only_the_slots_a_phi_of_constants_names() {
    assert_eq!(phi_index_arms(None), Some(vec![0x10, 0x40, 0x60]));
}

/// Under `if (s < 4)` the range is 0..=3, which drops the phi's `5`; the
/// slots between its constants stay unread.  Mutation: skip the range filter.
#[test]
fn classify_table_dispatch_drops_a_phi_constant_its_guard_excludes() {
    assert_eq!(phi_index_arms(Some(4)), Some(vec![0x10, 0x40]));
}

/// `t[cell & 3]` with `cell` read from one of three words no ROM serves: the
/// index does not fold under the phi's constants, so its range 0..=3 is
/// enumerated as it is without them.  The words span more slots than a table
/// may, so the phi itself is no index.  Mutation: keep a constant whose index
/// does not fold, which defers the site on the target fold.
#[test]
fn classify_table_dispatch_falls_back_to_the_range_where_the_phi_does_not_fold() {
    let f = phi_of_constants_dispatch([0x9000, 0x9004, 0x1_9000], None, |b, addr| {
        let cell = b
            .build_load(addr, VnSpace::RAM, ValueType::int(32))
            .unwrap();
        (masked_table_target(b, cell, 3, 4, 0x4000), None)
    });
    let rom = MockRom::strided(0x4000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    assert_eq!(
        arms(classify(&f, Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40])
    );
}

/// `s` in {0, 1, 5000} under `if (s < 4100)`: the guarded range holds more
/// slots than a table may, so no index bounds `s`, and the phi's `5000` is
/// one the guard rules out at the dispatch.  Mutation: drop the site-range
/// filter on the phi's constants.
#[test]
fn classify_table_dispatch_drops_a_phi_constant_a_wide_guard_excludes() {
    let f = phi_of_constants_dispatch([0, 1, 5000], Some((IntCmpOp::Less, 4100, true)), |b, s| {
        (table_target(b, s, 4, 0x4000), None)
    });
    let rom = MockRom::strided(0x4000, 4, (0..=5000).map(|i| 0x10 + i).collect(), 4);
    assert_eq!(arms(classify(&f, Some(&rom))), Some(vec![0x10, 0x11]));
}

/// `p = arg == 1 ? L1 : (arg == 2 ? L2 : 0); if (p) goto *p`: the branch
/// is reached only with `p != 0`, so the phi's `0` is no target.
/// Mutation: drop the excluded-constant filter on the phi's constants.
#[test]
fn classify_table_dispatch_drops_a_phi_constant_an_equality_guard_excludes() {
    let f = phi_of_constants_dispatch(
        [0x1000, 0x2000, 0],
        Some((IntCmpOp::Equal, 0, false)),
        |_, p| (p, None),
    );
    assert_eq!(arms(classify(&f, None)), Some(vec![0x1000, 0x2000]));
}

/// `t[s]` over interworking words with `s` in {0, 1, 3}: a bounded index
/// and the phi's constants both, so the phi is pinned and each arm's mode
/// folds off its own word; `t[2]` is never read.  Mutation: enumerate the
/// range where the phi's constants are known.
#[test]
fn classify_table_dispatch_folds_each_mode_of_a_phi_indexed_table() {
    let f = phi_of_constants_dispatch([0, 1, 3], None, |b, s| {
        let word = table_target(b, s, 4, 0x4000);
        let (target, mode) = split_mode(b, word);
        (target, Some(mode))
    });
    let mode = f.indirect_branch_isa_mode(sole_indirect_branch(&f));
    assert!(mode.is_some(), "the branch commits a mode");
    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x2001, 0x3000, 0x4001], 4);
    match classify_with(&f, Some(&rom), &crate::AssumptionOptions::none(), mode) {
        Some(ResolvedTargets::Multiple(ts)) => assert_eq!(
            ts.iter().map(|t| (t.addr, t.isa_bit)).collect::<Vec<_>>(),
            vec![
                (0x1000, Some(false)),
                (0x2000, Some(true)),
                (0x4000, Some(true))
            ],
        ),
        other => panic!("expected three arms; got {other:?}\n{}", f.to_text(false)),
    }
}

/// A jump to a phi of interworking words: the phi is pinned, not the
/// masked target, so each arm's mode folds off its own word.  The words'
/// range spans more slots than a table may, so no index bounds them.
/// Mutation: pin the dispatch value instead of the phi.
#[test]
fn classify_table_dispatch_seats_a_phi_of_words_with_their_modes() {
    let f = phi_of_constants_dispatch([0x1000, 0x1010, 0x2001], None, |b, word| {
        let (target, mode) = split_mode(b, word);
        (target, Some(mode))
    });
    let mode = f.indirect_branch_isa_mode(sole_indirect_branch(&f));
    assert!(mode.is_some(), "the branch commits a mode");
    let result = classify_with(&f, None, &crate::AssumptionOptions::none(), mode);
    match result {
        Some(ResolvedTargets::Multiple(ts)) => assert_eq!(
            ts.iter().map(|t| (t.addr, t.isa_bit)).collect::<Vec<_>>(),
            vec![
                (0x1000, Some(false)),
                (0x1010, Some(false)),
                (0x2000, Some(true))
            ],
        ),
        other => panic!("expected three arms; got {other:?}\n{}", f.to_text(false)),
    }
}

#[test]
fn classify_table_dispatch_carries_each_arms_own_isa_mode() {
    // An interworking table over words `X` and `Y | 1`: the mode is folded per
    // INDEX off that index's own word, so the two arms land on distinct
    // addresses carrying opposite modes.
    let (g, mode_value) = interworking_table(1);
    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x2001], 4);
    assert_eq!(
        classify_with(
            &g,
            Some(&rom),
            &crate::AssumptionOptions::default(),
            mode_value
        ),
        Some(ResolvedTargets::table([
            strider_cfg::ResolvedTarget::new(0x1000, Some(false)),
            strider_cfg::ResolvedTarget::new(0x2000, Some(true)),
        ])),
    );
}

/// A seated `Switch` is re-derived from its retained selector, reporting no
/// per-arm ISA mode: the node carries no mode input to fold.
#[test]
fn classify_sites_rederives_a_seated_switch_from_its_selector() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let [entry, a0, a1] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let raw = build_non_const_idx(&mut b);
    let selector = masked_table_target(&mut b, raw, 1u64, 4u64, 0x4000u64);
    // Seated on only ONE of the two table slots, as a site resolved before the
    // CFG finished growing would be.
    let switch = b.build_switch(selector, &[(a0, 0x1000)]).unwrap();
    b.set_region(a0);
    b.build_return(None, &[]).unwrap();
    b.set_region(a1);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let g = b.build().unwrap();

    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x2000], 4);
    let found = super::super::classify_sites(&g, Some(&rom));

    assert_eq!(
        found.resolved.get(&switch),
        Some(&ResolvedTargets::table([
            0x1000u64.into(),
            0x2000u64.into()
        ])),
        "the seated switch widens to both slots, with no ISA mode"
    );
}

/// A seated `Switch` whose selector folds to a constant no arm lists jumps
/// there alone: the constant is its one target, as for a placeholder.
/// Mutation: re-derive a seated switch through the table arm alone.
#[test]
fn classify_sites_seats_a_switch_selector_folded_to_a_constant() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let [entry, a0] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let selector = b.build_int_const(0x5000u64, ValueType::int(32)).unwrap();
    let switch = b.build_switch(selector, &[(a0, 0x1000)]).unwrap();
    b.set_region(a0);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let g = b.build().unwrap();

    let found = super::super::classify_sites(&g, None);
    assert_eq!(
        found.resolved.get(&switch),
        Some(&ResolvedTargets::Single(0x5000.into())),
    );
}

#[test]
fn classify_table_dispatch_evaluates_each_dispatch_load_once_per_index() {
    // The mode root `And(word, 1)` and the target root `And(word, ~1)` share
    // every node below `word`.  One memo per INDEX evaluates the dispatch load
    // once; one per ROOT re-walks it, doubling the folding work (and, on a
    // stack table, the store walk behind it).
    let (g, mode_value) = interworking_table(3);
    let rom = RecordingRom {
        inner: MockRom::strided(0x4000, 4, vec![0x1000, 0x1004, 0x1008, 0x100C], 4),
        log: Mutex::new(Vec::new()),
    };
    let result = classify_with(
        &g,
        Some(&rom),
        &crate::AssumptionOptions::default(),
        mode_value,
    );
    assert!(matches!(result, Some(ResolvedTargets::Multiple(ref ts)) if ts.len() == 4));
    assert_eq!(
        rom.log.lock().unwrap().len(),
        4,
        "one dispatch-load read per index, not one per (index, root)"
    );
}

/// An on-stack label array's memory chain is as long as the table itself, so a
/// per-index memory walk costs O(entries^2).
mod stack_table_cost {
    use super::*;
    use crate::mem_analysis::WALK_STEPS;
    use crate::test_support::{assert_near_linear, steps};

    /// The memory-chain steps [`classify_dispatch_value`] spends on `fg`'s
    /// sole dispatch, asserting it resolves to `targets`, so the cost
    /// measurement can never be met by resolving less.
    fn resolve_steps(mut fg: Function, targets: &[u64]) -> crate::Result<u64> {
        fg.assume(&crate::AssumptionOptions::default());
        let mut sc = SiteClassifier::new(&fg, None);
        let branch = sole_indirect_branch(&fg);
        let target = fg.indirect_branch_target(branch);
        steps(&[&WALK_STEPS], || {
            let result = sc.classify_dispatch_value(branch, target, None);
            assert_eq!(
                arms(result).as_deref(),
                Some(targets),
                "every slot resolves to its own label"
            );
            Ok(())
        })
    }

    /// The steps for an `entries`-slot stack array from `build`.
    fn resolve(
        entries: usize,
        build: fn(&[u64], i64, u64) -> (strider_ir::Function, ValueId),
    ) -> crate::Result<u64> {
        let targets: Vec<u64> = (0..entries as u64).map(|i| 0x401000 + i * 0x10).collect();
        let (fg, _load) = build(&targets, -8 * entries as i64, 8);
        resolve_steps(fg, &targets)
    }

    /// A 4097-def chain between the table's stores and the dispatch, far
    /// longer than the table.  The shared index answers each probe without
    /// walking it, so the site resolves at a cost the chain does not set.
    #[test]
    fn a_long_chain_above_the_dispatch_still_resolves() -> crate::Result<()> {
        let filler = 4097usize;
        let targets: Vec<u64> = (0..4u64).map(|i| 0x401000 + i * 0x10).collect();
        let (mut b, sp_val) = stack_frame();
        wire_stack_stores(&targets, -32, 8, sp_val, &mut b);
        // Nearer the load than the table, so every probe reaches past it.
        wire_stack_stores(&vec![0u64; filler], -0x10000, 8, sp_val, &mut b);
        wire_stack_dispatch(targets.len(), -32, 8, sp_val, &mut b, stack_arg());
        let (fg, _load) = finish_stack_array(b.build().unwrap());
        let steps = resolve_steps(fg, &targets)?;
        assert!(
            steps < filler as u64,
            "the probes must not walk the chain: {steps} steps over a \
             {filler}-def chain"
        );
        Ok(())
    }

    /// Doubling the entry count must not quadruple the memory walk, off the
    /// bare SP and across a `MemPhi`, whose empty straight-line segment
    /// makes every probe fall off it: without a memo behind the phi that is
    /// one full chain walk per entry.
    #[test]
    fn is_not_quadratic_in_entries() -> crate::Result<()> {
        assert_near_linear("bare", 64, |n| resolve(n, build_stack_array), 300)?;
        assert_near_linear(
            "across a MemPhi",
            64,
            |n| resolve(n, build_stack_array_across_mem_phi),
            300,
        )
    }
}

/// `mips32le/switch.elf::main`: the table base lives in a register across a
/// `Call` on one arm.  With `clobber`, the callee's clobber of that register
/// reaches the dispatch, so the base is a merge of the literal and an unknown.
fn seated_switch_over_merged_base(clobber: bool) -> (Function, NodeId) {
    let sp = sp32_vn();
    let base_vn = reg_vn(0x20, 4);
    let mut b = sp_frame(sp).tracked(base_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let side = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let arm0 = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let base_c = b.build_int_const(0x4000u64, ValueType::int(32)).unwrap();
    b.write_variable(&base_vn, base_c).unwrap();
    let raw = build_non_const_idx(&mut b);
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(raw, zero, IntCmpOp::Equal, ValueType::int(32))
        .unwrap();
    b.build_if(cond, side, dispatch).unwrap();

    b.set_region(side);
    if clobber {
        let callee = b.build_int_const(0x8000u64, ValueType::int(32)).unwrap();
        let (_, outs) = b.build_call(callee, &[], &[base_vn], 0).unwrap();
        b.write_variable(&base_vn, outs[0]).unwrap();
    }
    b.build_branch(dispatch).unwrap();

    b.set_region(dispatch);
    let base = b.read_variable(&base_vn).unwrap();
    let one = b.build_int_const(1u64, ValueType::int(32)).unwrap();
    let idx = b
        .build_int_binary_operation(raw, one, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    let stride_c = b.build_int_const(4u64, ValueType::int(32)).unwrap();
    let off = b
        .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, ValueType::int(32))
        .unwrap();
    let addr = b
        .build_int_binary_operation(base, off, IntBinaryOp::Add, ValueType::int(32))
        .unwrap();
    let selector = b
        .build_load(addr, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    // Seated on one of the two slots, as a site resolved before the CFG closed.
    let switch = b.build_switch(selector, &[(arm0, 0x1000)]).unwrap();

    b.set_region(arm0);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    (b.build().unwrap(), switch)
}

fn rederive_seated_switch(mut g: Function, switch: NodeId) -> Option<ResolvedTargets> {
    let rom = MockRom::strided(0x4000, 4, vec![0x1000, 0x2000], 4);
    pipeline(&[&PhiCollapse, &ConstantFold, &RegionCollapse])
        .run(&mut g, &crate::OptCtx::new(Some(&rom)))
        .expect("prep");
    let found = super::super::classify_sites(&g, Some(&rom));
    assert!(
        found.sites.contains(&switch),
        "the seated switch is reported"
    );
    found.resolved.get(&switch).cloned()
}

/// The control: with the base reaching the dispatch unchanged on both paths it
/// is the literal, and the seated switch widens to both slots.
#[test]
fn seated_switch_rederives_when_the_table_base_survives_the_merge() {
    let (g, switch) = seated_switch_over_merged_base(false);
    assert_eq!(
        rederive_seated_switch(g, switch),
        Some(ResolvedTargets::table([0x1000u64.into(), 0x2000u64.into()])),
    );
}

/// A callee may leave anything in the base register, so on the path through
/// the `Call` the dispatch reads an unknown address: no target set is proven
/// and the site must defer, keeping its seated arms and reporting unresolved.
///
/// The arms a narrower round seated can still be right, as they are for
/// `mips32le/switch.elf::main` (GCC's interprocedural register allocation knows
/// the local callee leaves the register alone); an ABI-level clobber set cannot
/// know that, and a per-`Call` `preserves_regs` override is what supplies it.
#[test]
fn seated_switch_defers_when_a_call_clobbers_the_table_base() {
    let (g, switch) = seated_switch_over_merged_base(true);
    assert_eq!(rederive_seated_switch(g, switch), None);
}

/// A stack label array with a store through a fresh allocation between the
/// labels and the dispatch.
fn stack_array_behind_a_heap_store(allocator: u64, targets: &[u64]) -> Function {
    let ret = reg_vn(0x00, 8);
    let mut b = sp_frame(sp64())
        .tracked(ret)
        .ret(ret)
        .tracked(stack_arg())
        .build_fn_single_region()
        .unwrap();
    let target = b.build_int_const(allocator, ValueType::int(64)).unwrap();
    let (_call, rets) = b.build_call(target, &[], &[ret], 0).unwrap();
    let sp_val = b.read_variable(&sp64()).unwrap();
    wire_stack_stores(targets, -24, 8, sp_val, &mut b);
    let junk = b.build_int_const(0x55u64, ValueType::int(64)).unwrap();
    b.build_store(rets[0], junk, VnSpace::RAM).unwrap();
    wire_stack_dispatch(targets.len(), -24, 8, sp_val, &mut b, stack_arg());
    b.build().unwrap()
}

/// A heap verdict rests on the allocator claim: without it the store through
/// the allocation is an opaque pointer that may overwrite a label, and once the
/// claim is in force a later run that omits it keeps it.
#[test]
fn a_heap_verdict_opens_the_table_only_under_its_claim() {
    const MALLOC: u64 = 0x1000;
    let targets = [0x401190u64, 0x401180u64];
    let resolve = |fg: &mut Function, allocators: &[u64]| {
        let site = sole_indirect_branch(fg);
        fg.assume(&crate::AssumptionOptions {
            noalias_allocators: allocators.iter().copied().collect(),
            ..crate::AssumptionOptions::none()
        });
        run_passes(fg, FOLD).unwrap();
        super::super::classify_sites(fg, None)
            .resolved
            .get(&site)
            .cloned()
    };
    let table = Some(ResolvedTargets::table(
        targets.map(strider_cfg::ResolvedTarget::from),
    ));
    assert_eq!(
        resolve(&mut stack_array_behind_a_heap_store(MALLOC, &targets), &[]),
        None,
        "without the claim the store may overwrite a label"
    );
    let mut fg = stack_array_behind_a_heap_store(MALLOC, &targets);
    assert_eq!(
        resolve(&mut fg, &[MALLOC]),
        table,
        "a listed allocator's store is disjoint from the labels"
    );
    assert_eq!(
        resolve(&mut fg, &[]),
        table,
        "a later run without the claim keeps it"
    );
}

/// `c = *p:I8 ; i = zext64(trunc8(zext32(c) * 3)) ; if (i < 64 &&
/// (c & 0x80) != 0) goto table[i]`: the guard bounds the product's top
/// bits, which a carry ties to the tested bit of `c`, so only slots 1 mod 3
/// are feasible.
#[test]
#[ignore = "known limit: a read guard looser than an unread test is trusted, so infeasible cells are seated"]
fn classify_table_dispatch_heeds_a_test_a_carry_reaches() {
    let (i8t, i32t, i64t) = (ValueType::int(8), ValueType::int(32), ValueType::int(64));
    let p = reg_vn(0x20, 8);
    let mut b = RegisterSet::new().tracked(p).arg(p).build_fn().unwrap();
    let [entry, tested, dispatch, exit] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let ptr = b.read_variable(&p).unwrap();
    let c = b.build_load(ptr, VnSpace::RAM, i8t).unwrap();
    let wide = b.extend_if_needed(c, i32t, ExtendOp::ZeroExtend).unwrap();
    let m = b
        .build_const_binop(3u64, wide, IntBinaryOp::Mul, i32t)
        .unwrap();
    let low = b.truncate_if_needed(m, i8t).unwrap();
    let idx = b.extend_if_needed(low, i64t, ExtendOp::ZeroExtend).unwrap();
    let n = b.build_int_const(64u64, i64t).unwrap();
    let below = b
        .build_int_cmp_operation(idx, n, IntCmpOp::Less, i64t)
        .unwrap();
    b.build_if(below, tested, exit).unwrap();

    b.set_region(tested);
    let top = b
        .build_const_binop(0x80u64, c, IntBinaryOp::And, i8t)
        .unwrap();
    let zero = b.build_int_const(0u64, i8t).unwrap();
    let clear = b
        .build_int_cmp_operation(top, zero, IntCmpOp::Equal, i8t)
        .unwrap();
    b.build_if(clear, exit, dispatch).unwrap();

    b.set_region(dispatch);
    let scaled = b
        .build_const_binop(3u64, idx, IntBinaryOp::ShiftLeft, i64t)
        .unwrap();
    let addr = b
        .build_const_binop(0x4000u64, scaled, IntBinaryOp::Add, i64t)
        .unwrap();
    let loaded = b.build_load(addr, VnSpace::RAM, i64t).unwrap();
    b.build_indirect_branch(loaded).unwrap();

    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut g = b.build().unwrap();
    run_passes(&mut g, COLLAPSE).unwrap();
    let rom = MockRom::strided(0x4000, 8, (0..256).map(|i| 0x5000 + 0x10 * i).collect(), 8);
    if let Some(ResolvedTargets::Multiple(ts)) = classify(&g, Some(&rom)) {
        for t in ts.iter() {
            let slot = (t.addr - 0x5000) / 0x10;
            assert_eq!(slot % 3, 1, "slot {slot} is infeasible");
        }
    }
}

/// `idx = (load & 3) | 4 ; goto *[0x4000 + (idx << 2)]`: KnownBits proves
/// bit 2 set, so only slots 4 to 7 are read, though the range is `[0, 7]`.
/// Mutation: drop the known-bits filter from the enumeration.
#[test]
fn classify_table_dispatch_reads_only_slots_with_the_known_one_bits() {
    let i32t = ValueType::int(32);
    let (g, _) = build_with_target(|fb| {
        let ld = build_non_const_idx(fb);
        let low = fb
            .build_const_binop(3u64, ld, IntBinaryOp::And, i32t)
            .unwrap();
        let idx = fb
            .build_const_binop(4u64, low, IntBinaryOp::Or, i32t)
            .unwrap();
        let scaled = fb
            .build_const_binop(2u64, idx, IntBinaryOp::ShiftLeft, i32t)
            .unwrap();
        let addr = fb
            .build_const_binop(0x4000u64, scaled, IntBinaryOp::Add, i32t)
            .unwrap();
        fb.build_load(addr, VnSpace::RAM, i32t).unwrap()
    });
    let rom = RecordingRom {
        inner: MockRom::strided(0x4000, 4, (0..8).map(|i| 0x100 * (i + 1)).collect(), 4),
        log: Mutex::new(Vec::new()),
    };
    assert_eq!(
        arms(classify(&g, Some(&rom))),
        Some(vec![0x500, 0x600, 0x700, 0x800])
    );
    assert!(
        rom.log.lock().unwrap().iter().all(|&(a, _)| a >= 0x4010),
        "a slot without bit 2 was read"
    );
}

/// `goto *[0x1000 | ((x & 3) << 2)]`: the address itself is the index, and
/// only the four words at 0x1000 are ones it takes.
/// Mutation: drop the known-bits filter from the enumeration.
#[test]
fn classify_table_dispatch_reads_only_addresses_with_the_known_one_bits() {
    let i32t = ValueType::int(32);
    let (g, _) = build_with_target(|fb| {
        let ld = build_non_const_idx(fb);
        let low = fb
            .build_const_binop(3u64, ld, IntBinaryOp::And, i32t)
            .unwrap();
        let scaled = fb
            .build_const_binop(2u64, low, IntBinaryOp::ShiftLeft, i32t)
            .unwrap();
        let addr = fb
            .build_const_binop(0x1000u64, scaled, IntBinaryOp::Or, i32t)
            .unwrap();
        fb.build_load(addr, VnSpace::RAM, i32t).unwrap()
    });
    let rom = MockRom::strided(0x1000, 4, vec![0x10, 0x20, 0x30, 0x40], 4);
    assert_eq!(
        arms(classify(&g, Some(&rom))),
        Some(vec![0x10, 0x20, 0x30, 0x40])
    );
}

/// `n` switches in sequence, the `i`th on the byte at `p + i`: `if (b !=
/// 0x55) goto table[b]`, else on to the next. Each index is the whole byte,
/// its KnownBits bound, so every dispatch weighs the tests of its cell.
fn sequential_field_switches(n: usize) -> Function {
    let (i8t, i32t, i64t) = (ValueType::int(8), ValueType::int(32), ValueType::int(64));
    let p = reg_vn(0x20, 8);
    let mut b = RegisterSet::new().tracked(p).arg(p).build_fn().unwrap();
    let tests: Vec<_> = (0..=n).map(|_| b.create_region_all().unwrap()).collect();
    b.set_entry_region_all(tests[0]).unwrap();
    b.set_region(tests[0]);
    let ptr = b.read_variable(&p).unwrap();
    for i in 0..n {
        let dispatch = b.create_region_all().unwrap();
        b.set_region(tests[i]);
        let addr = at(&mut b, ptr, i as i64, i64t).unwrap();
        let byte = b.build_load(addr, VnSpace::RAM, i8t).unwrap();
        let idx = b
            .extend_if_needed(byte, i32t, ExtendOp::ZeroExtend)
            .unwrap();
        let skip = b.build_int_const(0x55u64, i32t).unwrap();
        let cond = b
            .build_int_cmp_operation(idx, skip, IntCmpOp::Equal, i32t)
            .unwrap();
        b.build_if(cond, tests[i + 1], dispatch).unwrap();
        b.set_region(dispatch);
        let target = table_target(&mut b, idx, 4, 0x4000);
        b.build_indirect_branch(target).unwrap();
    }
    b.set_region(tests[n]);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    run_passes(&mut f, COLLAPSE).unwrap();
    f
}

/// Each of `n` switches on fields of one pointer looks at the pointer's
/// other offsets only where one may be bounded, and weighs only the tests of
/// its own field, so the guard and cell-test scans stay linear in `n`.
/// Mutation: range every `Add(v, k)` sibling of a chain value at each site.
/// Mutation: weigh every `If` dominating the site.
#[test]
fn sequential_field_switches_scan_linearly() -> crate::Result<()> {
    use super::super::index_range::{CELL_TEST_SCANS, GUARD_SCANS};
    let rom = MockRom::strided(0x4000, 4, (0..256).map(|i| 0x10 * (i + 1)).collect(), 4);
    for (name, counter) in [
        ("guard scans of sequential field switches", &GUARD_SCANS),
        (
            "cell-test scans of sequential field switches",
            &CELL_TEST_SCANS,
        ),
    ] {
        crate::test_support::assert_near_linear(
            name,
            64,
            |n| {
                let f = sequential_field_switches(n);
                crate::test_support::steps(&[counter], || {
                    let sites = super::super::classify_sites(&f, Some(&rom));
                    assert_eq!(sites.resolved.len(), n, "every switch is seated");
                    Ok(())
                })
            },
            250,
        )?;
    }
    Ok(())
}

/// `n` switches on one field byte `x`, the `i`th behind `if (x == k_i)
/// return` and a test of an unrelated word: every equality on `x`
/// dominates every later switch.
fn sequential_equalities_on_one_index(n: usize) -> Function {
    let (i8t, i32t) = (ValueType::int(8), ValueType::int(32));
    let p = reg_vn(0x20, 4);
    let mut b = RegisterSet::new().tracked(p).arg(p).build_fn().unwrap();
    let tests: Vec<_> = (0..=n).map(|_| b.create_region_all().unwrap()).collect();
    b.set_entry_region_all(tests[0]).unwrap();
    b.set_region(tests[0]);
    let ptr = b.read_variable(&p).unwrap();
    let addr = at(&mut b, ptr, 0x10, i32t).unwrap();
    let byte = b.build_load(addr, VnSpace::RAM, i8t).unwrap();
    let x = b
        .extend_if_needed(byte, i32t, ExtendOp::ZeroExtend)
        .unwrap();
    for i in 0..n {
        let [next, dispatch, exit] = std::array::from_fn(|_| b.create_region_all().unwrap());
        b.set_region(tests[i]);
        let k = b.build_int_const((i % 250) as u64, i32t).unwrap();
        let eq = b
            .build_int_cmp_operation(x, k, IntCmpOp::Equal, i32t)
            .unwrap();
        b.build_if(eq, exit, next).unwrap();
        b.set_region(exit);
        b.build_return(None, &[]).unwrap();
        b.set_region(next);
        let word = b.build_int_const(0xa000u64 + i as u64, i32t).unwrap();
        let word = b.build_load(word, VnSpace::RAM, i32t).unwrap();
        let zero = b.build_int_const(0u64, i32t).unwrap();
        let clear = b
            .build_int_cmp_operation(word, zero, IntCmpOp::Equal, i32t)
            .unwrap();
        b.build_if(clear, dispatch, tests[i + 1]).unwrap();
        b.set_region(dispatch);
        let target = table_target(&mut b, x, 4, 0x4000);
        b.build_indirect_branch(target).unwrap();
    }
    b.set_region(tests[n]);
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    run_passes(&mut f, COLLAPSE).unwrap();
    f
}

/// The tests of the shared field are weighed once per index, not once per
/// switch, and each switch weighs only the equalities its own values could
/// be, so the cell-test and equality scans stay linear in `n`.
/// Mutation: weigh every `If` reading the cell at each site.
/// Mutation: weigh every equality on each index value at each site.
#[test]
fn equalities_on_a_shared_index_scan_linearly() -> crate::Result<()> {
    use super::super::index_range::{CELL_TEST_SCANS, EQUALITY_SCANS};
    let rom = MockRom::strided(0x4000, 4, (0..256).map(|i| 0x10 * (i + 1)).collect(), 4);
    for (name, counter) in [
        ("cell tests on a shared index", &CELL_TEST_SCANS),
        ("equalities on a shared index", &EQUALITY_SCANS),
    ] {
        crate::test_support::assert_near_linear(
            name,
            128,
            |n| {
                let f = sequential_equalities_on_one_index(n);
                crate::test_support::steps(&[counter], || {
                    let sites = super::super::classify_sites(&f, Some(&rom));
                    assert_eq!(sites.resolved.len(), n, "every switch is seated");
                    Ok(())
                })
            },
            250,
        )?;
    }
    Ok(())
}
