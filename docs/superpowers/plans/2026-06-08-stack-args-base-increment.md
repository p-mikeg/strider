# Unbounded stack-argument layout (base + increment) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the calling convention's fixed `stack_arg_offsets: Vec<i64>` list with an unbounded `StackArgs { base_offset, increment }` formula, and rewrite stack-argument detection (incoming args + outgoing call args) to classify *any* number of slots from that formula.

**Architecture:** A calling convention's stack-passed arguments form a uniform arithmetic series — slot N sits at `base_offset + N*increment` bytes from the call-time SP (verified: every preset's offset list is exactly such a series, stride = ABI word size). Model that directly as `Option<StackArgs>` on the CC (None = no stack args). `FunctionArgDetect` then classifies every load based at the entry SP whose byte range lies inside one slot and whose nearest memory clobber (resolved via the existing `SpAliasOracle` + its knobs) is `InitialMemory`; `CallStackArgCollect` mirrors it for stores to a call's pre-call SP. Both keep strict contiguity from arg 0.

**Tech Stack:** Rust workspace — `strider-target` (CC + `PositionalArgLayout`), `strider-ir` (builder/Function accessors), `strider-opt` (the two passes + `SpAliasOracle`), `strider-py` (`CallingConvention` binding + `.pyi`). Tests: `cargo test -p <crate>`; full gate `cargo test --workspace` + `cargo clippy --workspace --all-targets`; rebuild `.so` (`cargo build -p strider-py && cp target/debug/libstrider_py.so crates/strider-py/strider/strider.abi3.so`) + `uv run pytest`.

**Per-preset values (verified from the current lists):**

| preset | base_offset | increment |
|---|---|---|
| x86_64_systemv | 8 | 8 |
| x86_64_all_preserving | — | `None` |
| aarch64_aapcs64 | 0 | 8 |
| arm_aapcs | 0 | 4 |
| mips_o32 | 16 | 4 |
| mips_n64 | 0 | 8 |
| powerpc_sysv32 | 8 | 4 |
| powerpc64_elf_v1 | 48 | 8 |
| powerpc64_elf_v2 | 32 | 8 |
| x86_cdecl | 4 | 4 |
| x86_linux_kernel | 4 | 4 |

---

## File Structure

- **`crates/strider-target/src/calling_convention/mod.rs`** — new `StackArgs` type + its `offset_of`/`index_of`; replace the `stack_arg_offsets: &[i64]` (in `CallingConvention`) and `stack_arg_offsets: Vec<i64>` (in `BuiltCallingConvention`) fields with `stack_args: Option<StackArgs>`; `try_new` param change; `positional_arg_layout` returns a `PositionalArgLayout` struct (register `Vec` + `Option<StackArgs>`); delete the `PositionalArg` enum; update all 11 `CC_PRESETS` rows + the `tests.rs` round-trip table.
- **`crates/strider-ir/src/builder/mod.rs` + `function/data.rs` + `function/edit.rs`** — `set_stack_arg_offsets(Vec<i64>)` → `set_stack_args(Option<StackArgs>)`; `call_stack_arg_offsets_override` → `call_stack_args_override(NodeId) -> Option<StackArgs>`; `BuiltCallingConvention` field rename ripples into the `Default` path (`edit.rs:690`).
- **`crates/strider-opt/src/function_args/mod.rs`** — rewrite `detect_stack_args` to enumerate entry-SP loads, classify via `StackArgs::index_of`, keep the oracle clobber-is-InitialMemory test, strict contiguity.
- **`crates/strider-opt/src/call_stack_args/mod.rs`** — rewrite `collect_stack_args_in_chain_order` / `try_collect_stack_args` to map a relative store offset to an index via `StackArgs::index_of`, growing the slot set, dense prefix.
- **`crates/strider-opt/src/sp_expr/walk.rs`** — `SpAliasOracle` already supports the InitialMemory walk; expose (if needed) a helper that returns the nearest clobbering def so `function_args` can test it is `InitialMemory`.
- **`crates/strider-py/src/cc.rs` + `strider/__init__.pyi`** — `CallingConvention(...)`'s `stack_arg_offsets: list[int]` param → `stack_arg_base: int | None, stack_arg_increment: int` (or a 2-tuple); build `Option<StackArgs>`.
- **`CLAUDE.md`** — update the `PositionalArgLayout` / stack-arg description.

---

### Task 1: `StackArgs` type + CC field replacement (strider-target)

**Files:**
- Modify: `crates/strider-target/src/calling_convention/mod.rs`
- Test: `crates/strider-target/src/calling_convention/tests.rs`

- [ ] **Step 1: Write the failing test** (append to `crates/strider-target/src/calling_convention/tests.rs`)

```rust
#[test]
fn stack_args_offset_and_index() {
    use crate::calling_convention::StackArgs;
    let s = StackArgs { base_offset: 8, increment: 8 };
    // offset_of: slot N at base + N*increment.
    assert_eq!(s.offset_of(0), 8);
    assert_eq!(s.offset_of(3), 32);
    // index_of: a load fully inside slot N maps to N.
    assert_eq!(s.index_of(8, 8), Some(0));
    assert_eq!(s.index_of(32, 4), Some(3)); // 4-byte load inside the 8-byte slot 3
    // below base → not a stack arg.
    assert_eq!(s.index_of(0, 8), None);
    // straddles two slots → rejected.
    assert_eq!(s.index_of(12, 8), None); // [12,20) crosses the 8|16 boundary
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p strider-target stack_args_offset_and_index`
Expected: FAIL — `StackArgs` does not exist (compile error).

- [ ] **Step 3: Add the `StackArgs` type** (near `PositionalArg`, ~line 316 of `calling_convention/mod.rs`)

```rust
/// Layout of stack-passed arguments: an unbounded arithmetic series of
/// slots.  The N-th stack argument (0-indexed among the stack args)
/// occupies `base_offset + N * increment` bytes from the call-time stack
/// pointer.  Captures every supported ABI (each stack-arg series has a
/// uniform stride equal to the ABI word size).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StackArgs {
    /// Byte offset from call-time SP of the first stack-passed argument.
    pub base_offset: i64,
    /// Byte stride between consecutive stack-arg slots (the ABI word size);
    /// always `> 0`.
    pub increment: i64,
}

impl StackArgs {
    /// Byte offset (from call-time SP) of the `n`-th stack argument.
    #[must_use]
    pub fn offset_of(&self, n: usize) -> i64 {
        self.base_offset + (n as i64) * self.increment
    }

    /// The stack-arg index whose slot fully contains a `size`-byte access
    /// starting at `offset` (from call-time SP), or `None` when `offset`
    /// is below `base_offset` or the access straddles a slot boundary.
    #[must_use]
    pub fn index_of(&self, offset: i64, size: i64) -> Option<usize> {
        if offset < self.base_offset {
            return None;
        }
        let rel = offset - self.base_offset;
        let idx = (rel / self.increment) as usize;
        let slot_start = self.base_offset + (idx as i64) * self.increment;
        // Range inside one slot: [offset, offset+size) ⊆ [slot_start, slot_start+increment).
        (offset + size <= slot_start + self.increment).then_some(idx)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p strider-target stack_args_offset_and_index`
Expected: PASS

- [ ] **Step 5: Replace the CC fields + `try_new` + presets + `positional_arg_layout`**

In `crates/strider-target/src/calling_convention/mod.rs`:

(a) In `struct CallingConvention` (the `&'static` names DSL) replace `stack_arg_offsets: &'static [i64]` with `stack_args: Option<StackArgs>`.

(b) In `struct BuiltCallingConvention` (line ~113) replace `pub stack_arg_offsets: Vec<i64>,` with:
```rust
    /// Stack-passed-argument layout (`base_offset` + `increment`, unbounded),
    /// or `None` when the convention passes no arguments on the stack.
    pub stack_args: Option<StackArgs>,
```

(c) `try_new` (line ~225): replace the `stack_arg_offsets: Vec<i64>,` parameter with `stack_args: Option<StackArgs>,`; add a validation after the `ret_stack_pop` check:
```rust
        if let Some(sa) = stack_args
            && sa.increment <= 0
        {
            return Err(anyhow::anyhow!(
                "BuiltCallingConvention: stack-arg increment must be > 0, got {}",
                sa.increment,
            ));
        }
```
and store `stack_args` in the returned struct (replace the `stack_arg_offsets,` field init).

(d) `positional_arg_layout` (line ~184): change its return type and body. Replace the whole method with:
```rust
    /// The convention's positional-argument layout: the register slots in
    /// ABI order plus the unbounded stack-arg formula.
    #[must_use]
    pub fn positional_arg_layout(&self) -> PositionalArgLayout {
        PositionalArgLayout {
            registers: self.arg_passing_regs.clone(),
            stack: self.stack_args,
        }
    }
```

(e) Replace the `PositionalArg` enum (line ~324) with the `PositionalArgLayout` struct:
```rust
/// A convention's positional-argument layout: register slots first (indices
/// `0..registers.len()`), then unbounded stack slots (indices
/// `registers.len()..`) addressed by [`StackArgs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionalArgLayout {
    /// Argument-passing register varnodes, in ABI order.
    pub registers: Vec<rsleigh::Vn>,
    /// Stack-arg formula; `None` when no arguments are passed on the stack.
    pub stack: Option<StackArgs>,
}

impl PositionalArgLayout {
    /// The positional index of the first stack argument (= number of
    /// register args).
    #[must_use]
    pub fn first_stack_index(&self) -> usize {
        self.registers.len()
    }

    /// Byte offset (from call-time SP) of the positional arg at `index`, or
    /// `None` when `index` is a register slot or the CC has no stack args.
    #[must_use]
    pub fn stack_offset_of(&self, index: usize) -> Option<i64> {
        let first = self.registers.len();
        let stack = self.stack?;
        (index >= first).then(|| stack.offset_of(index - first))
    }
}
```

(f) Update every `CC_PRESETS` row's `stack_arg_offsets: &[...]` to `stack_args: <value>` per the per-preset table above, e.g. `stack_args: Some(StackArgs { base_offset: 8, increment: 8 })` for x86_64_systemv and `stack_args: None` for x86_64_all_preserving. There are 11 rows with non-empty lists + the empty all_preserving row (`stack_arg_offsets: &[]` → `stack_args: None`).

Export `StackArgs` + `PositionalArgLayout` from the crate root (`crates/strider-target/src/lib.rs`) wherever `PositionalArg` was exported; remove the `PositionalArg` export.

- [ ] **Step 6: Update the `tests.rs` round-trip table + the positional-layout tests**

In `crates/strider-target/src/calling_convention/tests.rs`: the `Case` struct's `stack_arg_offsets: &'static [i64]` field and every case row must change to a `stack_args: Option<StackArgs>` value matching the per-preset table; the round-trip assertion `built.stack_arg_offsets == c.stack_arg_offsets` becomes `built.stack_args == c.stack_args`. The `positional_arg_layout_x86_64_systemv` / `_x86_cdecl_stack_only` / `_empty` tests (which assert on `PositionalArg::Stack`/`Register`) rewrite against the new `PositionalArgLayout` shape:
```rust
#[test]
fn positional_arg_layout_x86_64_systemv() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention::x86_64_systemv().unwrap().build(&regs).unwrap();
    let layout = cc.positional_arg_layout();
    assert_eq!(layout.registers.len(), 6);
    assert_eq!(layout.first_stack_index(), 6);
    // First stack arg (positional index 6) at offset 8; the 8th at 24.
    assert_eq!(layout.stack_offset_of(6), Some(8));
    assert_eq!(layout.stack_offset_of(8), Some(24));
    assert_eq!(layout.stack_offset_of(0), None); // register slot
    assert_eq!(layout.registers[0], regs.name_to_vn("RDI").unwrap());
}

#[test]
fn positional_arg_layout_empty_has_no_stack() {
    let regs = regs_for(crate::arch::SleighArch::x86_64());
    let cc = CallingConvention::x86_64_all_preserving().unwrap().build(&regs).unwrap();
    let layout = cc.positional_arg_layout();
    assert!(layout.registers.is_empty());
    assert!(layout.stack.is_none());
    assert_eq!(layout.stack_offset_of(0), None);
}
```
Delete the old `_x86_cdecl_stack_only` body that asserted a finite stack-slot Vec; replace with a `stack_offset_of` check (cdecl: index 0 → offset 4, index 1 → 8).

- [ ] **Step 7: Run target tests + clippy**

Run: `cargo test -p strider-target` → 0 failures. `cargo clippy -p strider-target --all-targets` → clean.

- [ ] **Step 8: Commit**

```bash
git add crates/strider-target/src/calling_convention/mod.rs \
        crates/strider-target/src/calling_convention/tests.rs \
        crates/strider-target/src/lib.rs
git commit -m "feat(target): model stack args as unbounded base+increment

Replace CallingConvention/BuiltCallingConvention's fixed stack_arg_offsets
list with Option<StackArgs { base_offset, increment }>; positional_arg_layout
returns a PositionalArgLayout struct (register Vec + Option<StackArgs>); the
PositionalArg enum is removed. Every preset's offset list is a uniform
arithmetic series, so this is exact.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Thread `StackArgs` through strider-ir + strider-py

**Files:**
- Modify: `crates/strider-ir/src/builder/mod.rs` (`set_stack_arg_offsets`), `crates/strider-ir/src/function/data.rs` (`call_stack_arg_offsets_override`), `crates/strider-ir/src/function/edit.rs` (Default path ~690)
- Modify: `crates/strider-py/src/cc.rs`, `crates/strider-py/strider/__init__.pyi`
- Test: `crates/strider-ir/src/builder/tests.rs`

- [ ] **Step 1: Write the failing test** (`crates/strider-ir/src/builder/tests.rs`)

```rust
#[test]
fn set_stack_args_round_trips_on_default_cc() -> Result<()> {
    use strider_target::StackArgs;
    let sp = reg_vn(0x7000, 8);
    let mut b = raw_builder(vec![], &[], &[], &[], Some(sp), 0, strider_target::Endianness::Little)?;
    b.set_stack_args(Some(StackArgs { base_offset: 8, increment: 8 }));
    assert_eq!(
        b.function().default_cc().stack_args,
        Some(StackArgs { base_offset: 8, increment: 8 }),
    );
    Ok(())
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p strider-ir set_stack_args_round_trips_on_default_cc`
Expected: FAIL — `set_stack_args` does not exist / `stack_args` field unknown (compile error).

- [ ] **Step 3: Rename the builder setter** (`crates/strider-ir/src/builder/mod.rs:365`)

```rust
    pub fn set_stack_args(&mut self, stack_args: Option<strider_target::StackArgs>) {
        self.function.default_cc.stack_args = stack_args;
    }
```
(delete the old `set_stack_arg_offsets`.)

- [ ] **Step 4: Rename the per-call override accessor** (`crates/strider-ir/src/function/data.rs:609`)

```rust
    /// The per-`Call` override convention's stack-arg layout, if this Call
    /// node was built with a CC override; `None` for default calls.
    pub fn call_stack_args_override(&self, node_id: NodeId) -> Option<strider_target::StackArgs> {
        match self.call_descriptor.get(&node_id)? {
            crate::CallDescriptor::Call(cc) => cc.stack_args,
            _ => None,
        }
    }
```
Update its two call sites in `function/data.rs` (the tests at ~1663/1679 assert against `cc.stack_args`).

- [ ] **Step 5: Fix the `Default`/edit path** (`crates/strider-ir/src/function/edit.rs:690`): the synthetic `BuiltCallingConvention` literal sets `stack_arg_offsets: Vec::new()` → change to `stack_args: None`. Do the same for any other `BuiltCallingConvention { .. }` struct literal the compiler flags (grep `stack_arg_offsets:` workspace-wide and convert each).

- [ ] **Step 6: Update strider-py** — `crates/strider-py/src/cc.rs`: the `CallingConvention` constructor's `stack_arg_offsets: Vec<i64>` parameter becomes `stack_arg_base: Option<i64>` + `stack_arg_increment: i64` (default increment ignored when base is None); build `Option<StackArgs>`:
```rust
        let stack_args = stack_arg_base.map(|base_offset| strider_target::StackArgs {
            base_offset,
            increment: stack_arg_increment,
        });
```
pass `stack_args` to `BuiltCallingConvention::try_new`. Update the docstring + `crates/strider-py/strider/__init__.pyi` signature (`stack_arg_offsets: list[int]` → `stack_arg_base: int | None, stack_arg_increment: int`).

- [ ] **Step 7: Run tests + clippy**

Run: `cargo test -p strider-ir set_stack_args_round_trips_on_default_cc` → PASS; `cargo test -p strider-ir` → 0 failures; `cargo build -p strider-py` → clean; `cargo clippy -p strider-ir -p strider-py --all-targets` → clean.

- [ ] **Step 8: Commit**

```bash
git add crates/strider-ir/src/builder/mod.rs crates/strider-ir/src/function/data.rs \
        crates/strider-ir/src/function/edit.rs crates/strider-ir/src/builder/tests.rs \
        crates/strider-py/src/cc.rs crates/strider-py/strider/__init__.pyi
git commit -m "refactor(ir,py): thread Option<StackArgs> through builder/Function/py

set_stack_arg_offsets -> set_stack_args; call_stack_arg_offsets_override ->
call_stack_args_override (returns Option<StackArgs>); the py CallingConvention
binding takes stack_arg_base/stack_arg_increment.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Unbounded incoming-arg detection (`FunctionArgDetect`)

Rewrite detection to enumerate entry-SP loads, classify each via `StackArgs::index_of`, keep the existing oracle clobber-is-InitialMemory test, and apply strict contiguity from arg 0.

**Files:**
- Modify: `crates/strider-opt/src/function_args/mod.rs`
- Test: `crates/strider-opt/src/function_args/tests.rs`

- [ ] **Step 1: Write the failing test** — a function reading 10 stack args (more than any preset's old offset-list length) must detect all 10. Model it on the existing `function_args/tests.rs` builders (use the test harness's stack-load helper; build `Load[sp + 8*(i+1)]` for i in 0..10, each reading InitialMemory, then a Return). Assert `function.arg_index_to_values()` has entries for the 10 stack indices. (Write the concrete builder following the existing tests in that file — they show the exact `build_load` + entry-region setup pattern; set the CC's `stack_args` to `Some(StackArgs { base_offset: 8, increment: 8 })` via `set_stack_args`.)

```rust
#[test]
fn detects_more_stack_args_than_old_offset_list() -> Result<()> {
    // Build: 10 incoming stack args at sp+8, sp+16, ..., sp+80 (entry SP,
    // each load reads InitialMemory). The old fixed offset list capped at 6
    // for x86_64; the unbounded model must find all 10.
    // ... (construct via the file's existing stack-arg load helper; set
    //      cc.stack_args = Some(StackArgs{ base_offset: 8, increment: 8 }))
    // Assert: every index 6..16 (after 6 register args) has a carrier value,
    // OR — if the test builds a stack-only CC — indices 0..10.
    Ok(())
}
```
> Fill in the builder body by copying the construction shape from the nearest existing `function_args/tests.rs` test (e.g. `calls_clobber_stack_arguments_toggle_gates_arg_across_call`) and extending it to 10 slots. The assertion is the contract: all 10 detected.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p strider-opt detects_more_stack_args_than_old_offset_list`
Expected: FAIL — the current offset-list detection caps at the preset's list length (or the new `stack_args` field isn't consulted yet).

- [ ] **Step 3: Rewrite `detect_stack_args`** (`crates/strider-opt/src/function_args/mod.rs`). Replace the `stack_arg_offsets: &[i64]` derivation in `apply` and the `detect_stack_args` signature/body. New `apply` derivation:
```rust
        let layout = ctx.function().default_cc().positional_arg_layout();
        let stack_vn = ctx.function().default_cc().stack_vn;
        let first_stack_arg = layout.first_stack_index();
        let Some(stack_args) = layout.stack else {
            // No stack args under this convention.
            return Ok(OptimizationResult::NoChange);
        };
```
(keep the `clear_arg_values_from(first_stack_arg as u32)` call). New `detect_stack_args` (full body):
```rust
#[allow(clippy::too_many_arguments)]
fn detect_stack_args(
    ctx: &mut crate::EditFunction<'_>,
    stack_args: strider_target::StackArgs,
    first_stack_arg: usize,
    alias_mode: crate::AliasMode,
    calls_clobber_stack_arguments: bool,
    args_assume_distinct_sp_bases_disjoint: bool,
    memo: &mut SpExprMemo,
) -> Result<()> {
    let Some(initial_sp) = ctx.function().initial_sp_value() else {
        return Ok(());
    };
    let mut shadow_memo: ShadowMemo = ShadowMemo::default();
    // Group qualifying loads by stack-arg index. A load qualifies when:
    //   (a) its address decomposes to `initial_sp + K`,
    //   (b) `K` maps to a stack-arg slot (StackArgs::index_of), and
    //   (c) nothing on its memory chain clobbers that slot (the nearest
    //       clobber resolved via the SpAliasOracle is InitialMemory).
    let mut groups: rustc_hash::FxHashMap<usize, Vec<NodeId>> = rustc_hash::FxHashMap::default();
    let mut disqualified: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut work = seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
    while let Some(node_id) = work.dequeue() {
        let [memory, addr] = ctx
            .graph_ref()
            .node_inputs_exact::<2>(node_id)
            .expect("Load has 2 inputs per node signature");
        let [load_value] = ctx
            .node_outputs_exact::<1>(node_id)
            .expect("Load has 1 output per node signature");
        let load_ty = ctx.value_kind(load_value).as_value();
        let Some(load_ty) = load_ty else { continue };
        let load_size = load_ty.byte_size() as i64;
        // (a) decompose to initial_sp + K.
        let Some(crate::sp_expr::SpExpr { base, offset }) =
            decompose_sp(ctx.function(), addr, stack_args_stack_vn(ctx), memo)
        else {
            continue;
        };
        if base != initial_sp {
            continue;
        }
        // (b) K maps to a slot, range inside one slot.
        let Some(slot) = stack_args.index_of(offset, load_size) else {
            continue;
        };
        if disqualified.contains(&slot) {
            continue;
        }
        // (c) memory chain clean (nearest clobber is InitialMemory).
        let dirty = mem_chain_is_dirty(
            ctx,
            node_id,
            memory,
            base,
            offset,
            load_size,
            memo,
            &mut shadow_memo,
            alias_mode,
            calls_clobber_stack_arguments,
            args_assume_distinct_sp_bases_disjoint,
        )?;
        if dirty {
            disqualified.insert(slot);
            groups.remove(&slot);
            continue;
        }
        groups.entry(slot).or_default().push(node_id);
    }

    // Strict contiguity from slot 0 — first gap truncates.
    let mut max_slot_plus_one = 0usize;
    while groups.contains_key(&max_slot_plus_one) && !disqualified.contains(&max_slot_plus_one) {
        max_slot_plus_one += 1;
    }
    for slot in 0..max_slot_plus_one {
        let index = (first_stack_arg + slot) as u32;
        let Some(loads) = groups.remove(&slot) else { continue };
        // Same-space guard (unchanged from the previous implementation).
        let first = loads[0];
        let NodeKind::Load(space) = *ctx.node_kind(first) else {
            unreachable!("group members are seeded from Load nodes");
        };
        if loads.iter().any(|&l| !matches!(*ctx.node_kind(l), NodeKind::Load(s) if s == space)) {
            continue;
        }
        for load in loads {
            let [load_value] = ctx
                .node_outputs_exact::<1>(load)
                .expect("Load has 1 output per node signature");
            ctx.register_arg_value(index, load_value);
        }
    }
    Ok(())
}
```
> `stack_args_stack_vn(ctx)` is shorthand — pass the `stack_vn` already in scope (thread it as a parameter exactly as the old signature did; the snippet omits it for brevity, restore the `stack_vn: rsleigh::Vn` parameter and use it in the `decompose_sp` call). The `mem_chain_is_dirty` helper, `seeded_kind`, `ShadowMemo`, and `register_arg_value` are unchanged from the current file.

Update the `detect_stack_args(...)` call in `apply` to pass `stack_args` + `stack_vn` instead of `&stack_arg_offsets`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p strider-opt detects_more_stack_args_than_old_offset_list` → PASS. Then `cargo test -p strider-opt` (the existing `function_args` tests must stay green — the per-preset cases produce identical results since the formula reproduces their old offsets).

- [ ] **Step 5: Clippy**

Run: `cargo clippy -p strider-opt --all-targets` → clean.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-opt/src/function_args/mod.rs crates/strider-opt/src/function_args/tests.rs
git commit -m "feat(opt): unbounded incoming stack-arg detection via StackArgs

FunctionArgDetect now classifies every entry-SP load whose range lies in one
StackArgs slot and whose nearest memory clobber is InitialMemory, indexing by
(offset-base)/increment with strict contiguity from arg 0 — detecting any
number of stack args instead of a fixed offset list.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Unbounded outgoing call-arg collection (`CallStackArgCollect`)

**Files:**
- Modify: `crates/strider-opt/src/call_stack_args/mod.rs`
- Test: `crates/strider-opt/src/call_stack_args/tests.rs`

- [ ] **Step 1: Write the failing test** — a `Call` preceded by 10 stack-arg stores (`Store[sp + 8*i]`) must wire all 10 into the Call node. Build it following the existing `call_stack_args/tests.rs` construction pattern, setting the CC's `stack_args` to `Some(StackArgs{ base_offset: 0, increment: 8 })`; assert the Call node gains 10 stack-arg inputs (the file's existing tests show how to count/inspect Call stack-arg inputs).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p strider-opt <new_test_name>`
Expected: FAIL — the fixed-size `slots` vec caps at the offset-list length.

- [ ] **Step 3: Rewrite the offset→index mapping.** In `collect_stack_args_in_chain_order` (`crates/strider-opt/src/call_stack_args/mod.rs:101`), change the `stack_arg_offsets: &[i64]` parameter to `stack_args: strider_target::StackArgs`. Replace the fixed `slots: Vec<Option<ValueId>>` (sized to the list) with a growable map and the `stack_arg_offsets.iter().position(|&o| o == rel)` lookup (line 207) with `stack_args.index_of(rel, store_size)` (compute `store_size` from the store's data value width, mirroring `function_args`). Keep the prefix-monotonicity / dense-prefix logic, but back it with:
```rust
    let mut slots: rustc_hash::FxHashMap<usize, ValueId> = rustc_hash::FxHashMap::default();
```
and at the end return the dense prefix `0..k` (first missing index stops it) as a `Vec<ValueId>`. Update `try_collect_stack_args` (line 291) signature + the `apply` derivation (line ~378) to source `stack_args` from `layout.stack` (and the per-call override from `call_stack_args_override`).

> The full rewrite mirrors Task 3's structure: map a relative offset to a slot via `StackArgs::index_of`, accumulate into a `FxHashMap<usize, ValueId>`, then emit the contiguous `0..k` prefix. Preserve every chain-termination condition already in the function (base mismatch, space mismatch, MemPhi, Strict-mode cross-class store).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p strider-opt <new_test_name>` → PASS; `cargo test -p strider-opt` → 0 failures.

- [ ] **Step 5: Clippy + commit**

```bash
cargo clippy -p strider-opt --all-targets   # clean
git add crates/strider-opt/src/call_stack_args/mod.rs crates/strider-opt/src/call_stack_args/tests.rs
git commit -m "feat(opt): unbounded outgoing call stack-arg collection via StackArgs

CallStackArgCollect maps each pre-call-SP store offset to a slot index via
StackArgs::index_of into a growable map, emitting the contiguous prefix —
collecting any number of call stack args instead of a fixed offset list.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Docs + full gate

- [ ] **Step 1: Update CLAUDE.md** — the `PositionalArgLayout` bullet (under `strider-target`) and any "stack_arg_offsets" mention: describe `StackArgs { base_offset, increment }` (unbounded), `PositionalArgLayout { registers, stack: Option<StackArgs> }`, and that `FunctionArgDetect` / `CallStackArgCollect` classify any number of slots via `StackArgs::index_of` with strict contiguity (clobber resolution still via `SpAliasOracle` + its knobs).

- [ ] **Step 2: Full Rust workspace test** — `cargo test --workspace` → expect 102 suites, 0 failures.

- [ ] **Step 3: Full clippy** — `cargo clippy --workspace --all-targets` → clean.

- [ ] **Step 4: Rebuild the Python extension + pytest**
```bash
cargo build -p strider-py
cp target/debug/libstrider_py.so crates/strider-py/strider/strider.abi3.so
cd crates/strider-py && uv run pytest -q
```
Expect all pass. If a Python test constructed a `CallingConvention(stack_arg_offsets=[...])`, update it to `stack_arg_base=...`, `stack_arg_increment=...`.

- [ ] **Step 5: Commit**
```bash
git add CLAUDE.md
git commit -m "docs: unbounded stack-arg layout (StackArgs base+increment)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- CC `{base_offset, increment}` representation → Task 1 (`StackArgs`, `Option<StackArgs>` field, presets). ✓
- Unbounded → `StackArgs::offset_of`/`index_of` (no finite list); Tasks 3 & 4 use them. ✓
- Function-arg detection = entry-SP loads + range-in-one-slot + nearest-clobber-is-InitialMemory + sort/contiguity → Task 3. ✓
- Clobber resolution stays on the `SpAliasOracle` + knobs (`calls_clobber_stack_arguments`, `args_assume_distinct_sp_bases_disjoint`) → Task 3 keeps `mem_chain_is_dirty` with both knobs. ✓
- Calls "similar idea" → Task 4. ✓
- Strict contiguity kept → Tasks 3 & 4 emit the contiguous `0..k` prefix. ✓
- "Inside one argument" = within-slot (not boundary-exact) → `StackArgs::index_of` floors to a slot and checks `[offset, offset+size) ⊆ slot`. ✓

**Placeholder scan:** Tasks 3 & 4's test *builders* are described by reference to the existing test files' construction helpers rather than transcribed line-for-line (the assertion contracts are explicit). This is deliberate — the construction boilerplate is large and the existing tests are the authoritative template; the implementer copies the nearest existing builder and extends slot count. The `stack_args_stack_vn(ctx)` shorthand in Task 3's snippet is flagged inline to restore the real `stack_vn` parameter. No TBD/“handle edge cases”.

**Type consistency:** `StackArgs { base_offset, increment }`, `StackArgs::offset_of(usize)`, `StackArgs::index_of(i64, i64) -> Option<usize>`, `PositionalArgLayout { registers, stack }`, `first_stack_index()`, `stack_offset_of(usize)`, `set_stack_args(Option<StackArgs>)`, `call_stack_args_override(NodeId) -> Option<StackArgs>` — used consistently across Tasks 1–4. The CC field is `stack_args: Option<StackArgs>` everywhere.
