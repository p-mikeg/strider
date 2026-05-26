//! Memory-chain matchers — `LoadPat::mem_in`, `StorePat::mem_in`,
//! `StorePat::next_mem`, `CallOtherPat::next_mem` — exercise the
//! backward (`mem_in`) and forward (`next_mem`) walks along the
//! per-region memory chain.  `next_mem` is deterministic: it returns no
//! match when the mem output has zero or multiple consumers, mirroring
//! the `match_unique_output_consumer` helper.

use strider_analyze::pattern::{Matcher, any, call_other, int_const, load, store};
use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;

// ── LoadPat::mem_in ──────────────────────────────────────────────────────────

#[test]
fn load_mem_in_matches_preceding_store() {
    // Build: Store(addr=0x100, data=42) ; v = Load(addr=0x200) ; ret v
    // The Load's value is consumed by Return so the Load is reachable;
    // its inputs[0] (memory) is the Store's mem_out.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let val = b
        .build_int_const(42u64, NodeOutputType::U32)
        .expect("val");
    b.build_store(addr1, val, rsleigh::VnSpace::RAM)
        .expect("store");
    let addr2 = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("addr2");
    let load_val = b
        .build_load(addr2, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_return(Some(load_val), &[]).expect("ret");
    let function = b.build().expect("build");

    let pat = load()
        .addr(int_const(0x200u64))
        .mem_in(store().addr(int_const(0x100u64)));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(
        hits.len(),
        1,
        "load whose mem_in is the prior store should match exactly once"
    );
}

// ── StorePat::next_mem ───────────────────────────────────────────────────────

#[test]
fn store_next_mem_matches_following_store() {
    // Build: Store_A(addr=0x100) → Store_B(addr=0x200) → Return.
    // Both Stores advance the memory chain; Store_A.mem_out has exactly
    // one consumer (Store_B's input[0]).
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let v1 = b
        .build_int_const(1u64, NodeOutputType::U32)
        .expect("v1");
    b.build_store(addr1, v1, rsleigh::VnSpace::RAM)
        .expect("store_a");
    let addr2 = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("addr2");
    let v2 = b
        .build_int_const(2u64, NodeOutputType::U32)
        .expect("v2");
    b.build_store(addr2, v2, rsleigh::VnSpace::RAM)
        .expect("store_b");
    b.build_return(None, &[]).expect("ret");
    let function = b.build().expect("build");

    let pat = store()
        .addr(int_const(0x100u64))
        .next_mem(store().addr(int_const(0x200u64)));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(hits.len(), 1);
}

#[test]
fn next_mem_returns_no_match_when_multi_consumer() {
    // Store_A(0x100) ; Load(0x200) ; Store_B(0x300).
    // Load reads memory but doesn't advance it, so Store_A.mem_out has
    // TWO consumers (Load + Store_B).  `.next_mem(any())` on Store_A must
    // return zero matches (deterministic).
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let v1 = b
        .build_int_const(1u64, NodeOutputType::U32)
        .expect("v1");
    b.build_store(addr1, v1, rsleigh::VnSpace::RAM)
        .expect("store_a");
    let addr_l = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("addr_l");
    let load_val = b
        .build_load(addr_l, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    let addr2 = b
        .build_int_const(0x300u64, NodeOutputType::U64)
        .expect("addr2");
    let v2 = b
        .build_int_const(2u64, NodeOutputType::U32)
        .expect("v2");
    b.build_store(addr2, v2, rsleigh::VnSpace::RAM)
        .expect("store_b");
    b.build_return(Some(load_val), &[]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::try_new(&function).unwrap();

    // Sanity: store_a matches without `.next_mem` (baseline).
    let h_baseline = m.find_all(&store().addr(int_const(0x100u64)).into());
    assert_eq!(h_baseline.len(), 1, "baseline: store_a matches");

    // Multi-consumer case: deterministic no-match.
    let h_next = m.find_all(
        &store()
            .addr(int_const(0x100u64))
            .next_mem(any())
            .into(),
    );
    assert_eq!(
        h_next.len(),
        0,
        ".next_mem on a multi-consumer mem output returns deterministic no-match"
    );
}

#[test]
fn next_mem_returns_match_when_single_consumer() {
    // Single-consumer baseline: Store_A's mem_out has exactly one
    // consumer (Store_B's mem_in).  `.next_mem(any())` matches.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let v1 = b
        .build_int_const(1u64, NodeOutputType::U32)
        .expect("v1");
    b.build_store(addr1, v1, rsleigh::VnSpace::RAM)
        .expect("store_a");
    let addr2 = b
        .build_int_const(0x200u64, NodeOutputType::U64)
        .expect("addr2");
    let v2 = b
        .build_int_const(2u64, NodeOutputType::U32)
        .expect("v2");
    b.build_store(addr2, v2, rsleigh::VnSpace::RAM)
        .expect("store_b");
    b.build_return(None, &[]).expect("ret");
    let function = b.build().expect("build");

    let h = Matcher::try_new(&function).unwrap().find_all(
        &store()
            .addr(int_const(0x100u64))
            .next_mem(any())
            .into(),
    );
    assert_eq!(h.len(), 1);
}

// ── CallOtherPat::next_mem ───────────────────────────────────────────────────

#[test]
fn callother_next_mem_matches_following_store() {
    // Build: LOCK ; Store(addr=0x100, data=42).  LOCK is
    // PURE_WITH_MEM_EDGE in target::call_other_abi, so it sits on the
    // memory chain and its mem_out's unique consumer is the Store.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let (lock, _lock_val, _lock_clobbers) = b
        .build_call_other_modeled(1, "LOCK", &[], None, &[], &[], &[])
        .expect("LOCK");
    // CallOther outputs = [ctrl, mem, ...].  Advance the region memory
    // cursor to LOCK's mem_out so the next Store's mem_in is LOCK's
    // mem_out.
    let lock_mem_out = b.function().node_outputs(lock)[1];
    b.advance_cur_region_memory(lock_mem_out)
        .expect("advance mem to LOCK mem_out");
    let addr = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr");
    let val = b
        .build_int_const(42u64, NodeOutputType::U32)
        .expect("val");
    b.build_store(addr, val, rsleigh::VnSpace::RAM)
        .expect("store");
    b.build_return(None, &[]).expect("ret");
    let function = b.build().expect("build");

    let pat = call_other()
        .name("LOCK")
        .next_mem(store().addr(int_const(0x100u64)));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(
        hits.len(),
        1,
        "LOCK whose mem_out's unique consumer is the Store should match"
    );
}

// ── LOCK / Store / UNLOCK canonical use case ────────────────────────────────

#[test]
fn matches_store_bracketed_by_lock_and_unlock() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");

    // LOCK (PURE_WITH_MEM_EDGE — produces a memory output we must
    // promote to cur_region_memory before the Store).
    let (lock, _, _) = b
        .build_call_other_modeled(1, "LOCK", &[], None, &[], &[], &[])
        .expect("LOCK");
    let lock_mem_out = b.function().node_outputs(lock)[1];
    b.advance_cur_region_memory(lock_mem_out)
        .expect("advance to LOCK mem_out");

    // Store inside the bracket.
    let addr = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr");
    let val = b
        .build_int_const(42u64, NodeOutputType::U32)
        .expect("val");
    b.build_store(addr, val, rsleigh::VnSpace::RAM)
        .expect("store");

    // UNLOCK (also PURE_WITH_MEM_EDGE).  Its mem_in is read from
    // cur_region_memory, which is now the Store's mem_out, so the
    // Store's mem_out's unique consumer is UNLOCK's mem input.
    let (unlock, _, _) = b
        .build_call_other_modeled(2, "UNLOCK", &[], None, &[], &[], &[])
        .expect("UNLOCK");
    let unlock_mem_out = b.function().node_outputs(unlock)[1];
    b.advance_cur_region_memory(unlock_mem_out)
        .expect("advance to UNLOCK mem_out");

    b.build_return(None, &[]).expect("ret");
    let function = b.build().expect("build");

    let pat = store()
        .addr(int_const(0x100u64))
        .mem_in(call_other().name("LOCK"))
        .next_mem(call_other().name("UNLOCK"));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(
        hits.len(),
        1,
        "atomic Store inside LOCK ... UNLOCK should match exactly once"
    );
}
