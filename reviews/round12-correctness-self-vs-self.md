# Round 12 — Emphasis A axis 1: code-vs-code self-consistency audit

Branch: `review/ai6` · HEAD `008f530` · Trust model: strict (no prior-round reviews; no other round-12 reports read).

## Verdict

**No findings at confidence ≥ 80.** All 9 self-consistency categories verified by direct source reading. The codebase is internally coherent on these axes.

## Categories verified

### 1. SP decomposition — consistent
Single entry point `decompose_sp` in `crates/opt/src/sp_expr.rs` consumed by `StackStoreDetect`, `StackLoadForward`, `FunctionArgDetect`. Input-slot indices match node documentation everywhere: `StackStore [MEM=0, SP=1, DATA=2]` → `inputs[0]` for `prev_mem` in `step_through_stack_store`; `StackStorePhi [PHI=0, MEM=1, DATA=2]` → `inputs[1]` for `prev_mem` in `step_through_stack_store_phi`; raw `Store [MEM=0, ADDR=1, DATA=2]` → `inputs[0]` for `prev_mem`, `inputs[1]` for the address. The `int_const_signed` peephole for `Neg(IntConst(K))` uses `wrapping_neg` matching `ConstantFold`'s evaluator — pre-fold and post-fold offsets agree. `And`-alignment opaque-base arm requires the non-mask operand to decompose to an SP root.

### 2. CallOther dispatch — consistent
Both dispatch sites call `target::call_other_abi::classify(preset, name)`:
- `crates/cfg/src/cfg/builder/region_builder.rs` uses `self.builder.preset`; only acts on `NoReturn` (terminates region). `NoOp` and `Call(abi)` fall through.
- `crates/strider/src/strider/insn/mod.rs::handle_call_other` handles all three: `NoOp` skips emission, `NoReturn` → `build_call_other_terminal`, `Call(abi)` → `build_call_other_modeled` + advance memory iff `abi.memory_edge`.

Both sites receive the preset from `cfg::Builder::for_arch` (set from `SleighArch`). No case mismatch.

### 3. Varnode aliasing — read ↔ write inverses
`crates/pcode-lift/src/vn_io.rs`:
- `vn_mask` widths: 1, 2, 4, 8, 10, 16, 32, 64 bytes (32/64 return `u128::MAX` with wide-container guard).
- `read_reg_vn`: container → shift-right `calculate_reg_shift_from_container(reg, container, endian)` → truncate to `reg.size`.
- `write_reg_vn`: extend to container width → shift-left by the *same* formula → mask + OR.
- Shift formula: LE = `8*(reg.off - container.off)`, BE = `8*(container.size - reg.size - (reg.off - container.off))`. Read and write are true inverses. Wide-container guard (size > 16) fires identically in both paths.

### 4. Commutativity tables — single source of truth
`crates/pattern/src/matcher/commutativity.rs` is the only table. Pattern constructors (`add`, `mul`, `and`, `or`, `xor`, `bool_*`, etc.) build `Pat` nodes; the matcher calls `is_commutative_*_op` and tries both orderings if `true`. Non-commutative ops (`shl`, `shr`, `div`, `Less`, `Sless`, `Sborrow`) excluded. `IntCmpOp::Carry` and `IntCmpOp::Scarry` included (addition commutativity).

### 5. Lift-time canonicalisations — all 8 consistent
| # | Lifter | Pattern alias | Match |
|---|--------|---------------|-------|
| 1 | `IntSub(a,b) → Add(a, Neg(b))` (`arithmetic.rs`) | `sub(l,r)` (`ctor/int.rs`) | ✓ |
| 2 | `IntLessEqual(a,b) → BoolNeg(IntLess(b,a))` | `int_le(l,r)` swap | ✓ |
| 3 | `IntSlessEqual(a,b) → BoolNeg(IntSless(b,a))` | `int_sle(l,r)` swap | ✓ |
| 4 | `IntNotEqual(a,b) → BoolNeg(IntEqual(a,b))` | (no alias — compose directly) | ✓ |
| 5 | `FloatSub(a,b) → FloatAdd(a, FloatNeg(b))` | `float_sub(l,r)` | ✓ |
| 6 | `FloatNotEqual(a,b) → BoolNeg(FloatEqual(a,b))` | `float_ne(l,r)` | ✓ |
| 7 | `FloatLessEqual(a,b) → Or(FloatLess(a,b), FloatEqual(a,b))` | `float_le(l,r)` | ✓ |
| 8 | `FLOAT_NAN(x) → BoolNeg(FloatEqual(x,x))` | (no alias — compose directly) | ✓ |

### 6. Asm-fingerprint contract — consistent
Automatic path (`pattern::rewrite_rule` in `crates/pattern/src/rewrite.rs`): snapshots `pre_build_node_id`, BFS-walks all newly-allocated nodes (id >= snapshot), calls `extend_asm_fingerprint_from(new_node, lhs_root)` on each.

Manual path (hand-written passes):
- `StackStoreDetect` (`crates/opt/src/stack_store/detect.rs:45, 68`): `ctx.extend_asm_fingerprint_from(...)` immediately after `create_node` for `StackStore` and `StackStorePhi`.
- `FlagCmpCanonicalize` (`flag_cmp_canonicalize/mod.rs`): `build_int_cmp` and `build_bool_neg` each call `graph.extend_asm_fingerprint_from(...)` immediately after `create_node`.
- `IfCondInversion` (`if_cond_inversion/mod.rs:111`): `extend_asm_fingerprint_from(inner, bool_neg)` before redirecting inputs.

No pass creates nodes without fingerprint propagation.

### 7. Validator Layer A/B/C reachability scoping — consistent
`crates/ir/src/validate/`:
- **Layer A** (`layer_a.rs`): reachable-only. Zombie nodes intentionally skipped (passes leave zero-input zombies after detaching).
- **Layer B** (`layer_b.rs`): reachable-only; outputs of zombies with empty use-lists skipped.
- **Layer C** (`layer_c.rs`): `check_layer_c_uniqueness` (Entry/InitialMemory) scans ALL nodes (zombie rogue Entry IS a real invariant violation). All other Layer C checks (`control_state`, `phis`, `function_arg_uniqueness`, `wide_consts`, `asm_fingerprints`) are reachability-scoped.
- Asm-fingerprint exempt set: `Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`, `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi`.

### 8. `is_addr_tail_call` bounded-lift semantics — consistent
Single implementation in `crates/cfg/src/cfg/query.rs`. `lower_bound_strict = fn_max_size.is_some() || !allow_code_before_start_addr`. Returns true when `target < lower` or `target >= start_addr + fn_max_size`.

Two call sites pass identical 4 arguments:
- `region_builder.rs::is_branch_tail_call_nocheck` — fields from `self.builder`.
- `strider/src/orchestrator.rs::is_tail_call` — delegates directly with the same 4 params from `RunConfig`.

### 9. Memory-edge clobbering — consistent
- **`Store`**: always advances memory chain (`build_store` produces new memory output unconditionally).
- **`Call`**: advances iff `!no_memory_clobber`. `build_call_with_cc` (`crates/ir/src/builder/call.rs`) checks the CC flag. Only `x86_64_all_preserving` sets `no_memory_clobber: true`.
- **`CallOther` terminal (NoReturn)**: `build_call_other_terminal` does NOT advance memory (function does not return).
- **`CallOther` modeled (`Call(abi)`)**: `build_call_other_modeled` advances ctrl only; strider's `handle_call_other` calls `advance_cur_region_memory` iff `abi.memory_edge`.

No inconsistency between caller-site responsibility and IR builder contract.

## Files reviewed

- `crates/opt/src/sp_expr.rs`, `crates/opt/src/{stack_store,stack_load_forward,function_args}/*.rs`
- `crates/cfg/src/cfg/{builder/{mod,region_builder}.rs,query.rs}`
- `crates/strider/src/strider/insn/mod.rs`, `crates/strider/src/orchestrator.rs`
- `crates/pcode-lift/src/{vn_io.rs,value/*.rs}`
- `crates/pattern/src/{matcher/commutativity.rs,pat/ctor/{int,float}.rs,rewrite.rs}`
- `crates/opt/src/{flag_cmp_canonicalize,if_cond_inversion}/mod.rs`
- `crates/ir/src/validate/{layer_a,layer_b,layer_c}.rs`
- `crates/ir/src/builder/{call,nodes}.rs`
- `crates/target/src/{call_other_abi.rs,calling_convention/mod.rs}`
