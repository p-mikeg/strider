# Round 13 — Emphasis A axis 1: code-vs-code self-consistency

Branch: `review/ai7`.

## Verdict

**Zero findings at confidence ≥ 80.**  All 9 categories internally consistent.  Doc-vs-code drift on Layer B reachability scoping noted in 1A; the code itself is self-consistent.

## Categories verified consistent

✓ **SP decomposition** — `decompose_sp` (`crates/opt/src/sp_expr.rs`) is the single entry.  All consumers (`stack_store/detect.rs`, `stack_load_forward/mod.rs`, `function_args/mod.rs`, `step_through_store`) call through it.  `step_through_stack_store` and `step_through_stack_store_phi` receive already-decomposed offset params; no consumer re-decomposes.

✓ **CallOther dispatch** — Both call sites derive preset from the same `SleighArch` via `arch.preset()`:
- CFG-time (`region_builder.rs:409-410`): `preset = self.builder.preset` set by `Builder::for_arch(&arch, ...)`.
- IR-emission-time (`strider/insn/mod.rs:132`): `classify(self.strider.arch.preset(), name)`.

Both reference `opts.strider.arch`.  Orchestrator at `orchestrator.rs:958` uses `Builder::for_arch(&opts.strider.arch, ...)` so preset+endianness are atomic.

✓ **Varnode aliasing** — `pcode-lift/src/vn_io.rs` read+write share `calculate_reg_shift_from_container` formula.  Read: shift right + truncate.  Write: extend, shift left, positioned-mask + complement-mask + OR.  Widths 1/2/4/8/10/16/32/64 covered; 32/64 short-circuit via `container_reg == *reg` early-return.  Wide-container guard in both paths.

✓ **Commutativity** — `pattern/src/matcher/commutativity.rs` table matches per-op declarations in `pat/ctor/int.rs` + `pat/ctor/float.rs`:
- Int binary: `{Add, Mul, And, Or, Xor}` commute.
- Bool binary: `{And, Or, Xor}` commute.
- Float binary: `{Add, Mul}` commute.
- IntCmp: `{Equal, Carry, Scarry}` commute.
- FloatCmp: `{Equal}` only.

✓ **Lift-time canonicalisations (8/8)**:
| Lifter | IR shape | Pattern alias |
|---|---|---|
| `handle_int_sub` | `Add(a, Neg(b))` | `sub(a,b)` |
| `handle_ptr_sub` | `Add(a, Neg(b))` | `sub` |
| `handle_int_less_equal` | `BoolNeg(Less(b,a))` | `int_le(a,b)` swap |
| `handle_int_sless_equal` | `BoolNeg(Sless(b,a))` | `int_sle(a,b)` swap |
| `handle_int_not_equal` | `BoolNeg(Equal(a,b))` | (composed) |
| `handle_float_sub` | `FloatAdd(a, FloatNeg(b))` | `float_sub(a,b)` |
| `handle_float_not_equal` | `BoolNeg(FloatEqual(a,b))` | `float_ne(a,b)` |
| `handle_float_less_equal` | `Or(Less(a,b), Equal(a,b))` | `float_le(a,b)` |

✓ **Asm-fingerprint contract** — Two propagation paths:
- `pattern::rewrite_rule` BFS over freshly-allocated interior nodes (rewrite.rs:88-136).
- `OptimizationResult::after_replace` calls `extend_asm_fingerprint_from(new_node, old_node)` before replacement.
- Hand-rolled passes (constant_fold, flag_cmp_canonicalize, if_cond_inversion, known_bits, dead_branch, redundant_phis, stack_store/detect, stack_load_forward, function_args, indirect_branch_resolve/inplace) all call `extend_asm_fingerprint_from` adjacent to `create_node`.  No reachable non-exempt node created without propagation.

✓ **Validator Layer A/B/C reachability scoping** — validate/mod.rs:96-117:
- Layer A: reachable-only (`if !reachable.contains(node) { continue; }`).
- Layer B (layer_b.rs:39-85): both backward + forward checks reachability-gated.
- Layer C uniqueness: intentionally scans all nodes; other Layer-C checks reachable-only.

CLAUDE.md still says "Layer B: bidirectional use-list consistency" without the scoping qualifier (doc-only drift; code consistent).

✓ **`is_addr_tail_call` bounded-lift semantics** — Two call sites pass identical 4 args:
1. `region_builder.rs:222-228` (`is_branch_tail_call_nocheck`).
2. `orchestrator.rs:656-663` (`is_tail_call`, comment "stay in lockstep").

Both delegate to `cfg::is_addr_tail_call` in `query.rs`.

✓ **Memory-edge clobbering** — Three node kinds:
- `Store`: unconditionally advances memory (`builder/nodes.rs:726`).
- `Call`: advances IFF `!no_memory_clobber` (`builder/call.rs:142-144`); `x86_64_all_preserving` sets `no_memory_clobber: true` intentionally.
- `CallOther` terminal: terminates region; output dangles.
- `CallOther` modeled (`build_call_other_modeled`): does NOT advance memory; strider's `handle_call_other` advances IFF `abi.memory_edge` (`strider/insn/mod.rs:211-214`).  Single call site — no divergence opportunity.

## Summary

Zero divergence found across 9 categories.  Round 12's axis-1 audit was also zero-finding; round 13 confirms the codebase is still internally consistent on these axes.
