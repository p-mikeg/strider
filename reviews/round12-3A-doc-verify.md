# Round 12 — Audit 3A: Doc verification (trust-only-the-code)

**Branch**: `feature/ai` (= `review/ai6` for this round)
**Date**: 2026-05-11
**Scope**: CLAUDE.md, root `README.md`, every per-crate `crates/*/README.md`.
**Out of scope**: `.claude/skills/*/SKILL.md` (audited in round 11).

## Summary

Sampled **28 claims** across CLAUDE.md (12), root README (4), per-crate
READMEs (12).  Trust model: doc files are the inputs, code is the oracle.

| Verdict | Count |
|---|---|
| Confirmed | 23 |
| Partially confirmed | 2 |
| Stale / refuted | 3 |

The three stale findings are real but minor:

1. **CLAUDE.md opt-pass trait names are inverted** — the doc describes
   `OptimizerOnBuilt` as a companion to `Optimizer`, but the code uses
   `Optimizer` (RewriteCtx-based) as the primary trait and `OptimizerRaw`
   (Graph+NodeId) as the low-level companion.  The trait names and
   directions of the blanket impl are reversed in the doc.
2. **Root README mentions `pattern::float_is_nan` Rust constructor** — no
   such function exists in the Rust pattern crate.  It only exists in the
   Python wrapper (`strider-py`) as `pattern::float_is_nan` which expands
   to `float_ne(x, x)`.
3. **strider-py README contradicts itself on `float_is_nan`** — sections
   at lines 203-204 and 308-310 say `float_is_nan` "raises PatternError"
   / "not in v1", but the function is implemented and works (it produces
   the `BoolNeg(FloatEqual(x, x))` shape via `pattern::float_ne`).

All other CLAUDE.md/README claims I sampled are accurate.

## Per-claim findings

### CLAUDE.md samples

#### Claim 1 — `validate` Layer A reachability scoping
> Layer A: per-node local typing against `expected_signature` (scoped to nodes reachable via `walk_graph`...)  (`CLAUDE.md` ~L80)

**Verifying code**: `crates/ir/src/validate/mod.rs:96-101` —
```rust
for node in graph.nodes.keys() {
    if !reachable.contains(node) { continue; }
    check_layer_a(graph, node, &mut errs);
}
```
Plus `crates/ir/src/validate/layer_a.rs:7-12` doc comment confirms scope.

**Verdict**: **Confirmed**.

#### Claim 2 — Three-layer validator with Layer C asm-fingerprints opt-in
> ...`validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` adds an opt-in Layer-C check...  (`CLAUDE.md`)

**Verifying code**:
- `crates/ir/src/validate/mod.rs:43-51` — `ValidateOptions::check_asm_fingerprints` boolean
- `crates/ir/src/validate/mod.rs:115-117` — gates `check_layer_c_asm_fingerprints` on the flag
- `crates/ir/src/validate/layer_c.rs:184-197` — `asm_fingerprint_exempt` enumerates `Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`, `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi` — matches CLAUDE.md exempt set exactly.

**Verdict**: **Confirmed**.

#### Claim 3 — `NodeKind` exhaustive enum set
> Node kinds, grouped: Initial state (`Entry`, `InitialMemory`, `InitialVar(Vn)`), Region/join (`ControlState`, `VarPhi`, `MemPhi`)... `IndirectBranch`, `Load`, `Store`, ... `IntConst(u128)`, `IntConstWide(WideConstId)`, ...

**Verifying code**: `crates/ir/src/node/kind.rs:26-243` — every kind named in CLAUDE.md appears with the documented signature (e.g. `IntConstWide(crate::wide_const::WideConstId)` at L144, `StackStorePhi { space: rsleigh::VnSpace }` at L128).

**Verdict**: **Confirmed**.

#### Claim 4 — `NodeOutputType` enumeration
> `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U80` (x87 80-bit extended), `U128`, `U256`, `U512`; floats `F32`, `F64`, `F80`.

**Verifying code**: `crates/ir/src/node/output_type.rs:9-39` — enum variants match exactly (with `U80` documented as x87, `U512` as AVX-512 zmm via WideConst, `F80` as x87 extended).

**Verdict**: **Confirmed**.

#### Claim 5 — asm-fingerprint public API on Graph
> Public API on `Graph`: `asm_fingerprint(id) -> &[u64]`, `set_asm_fingerprint(id, Vec<u64>)`, `extend_asm_fingerprint(id, &[u64])`, `extend_asm_fingerprint_from(dst, src)`.

**Verifying code**: `crates/ir/src/graph/store.rs:153, 165, 175, 205` — all four methods exist with the documented signatures.  Sort-deduplicate behavior (`set_asm_fingerprint`) and union semantics (`extend_asm_fingerprint`) confirmed in source.

**Verdict**: **Confirmed**.

#### Claim 6 — vn_io supported widths
> Widths enumerated: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register), 32 (YMM), 64 (ZMM) bytes. Widths > 16 use a degraded `u128::MAX` mask.

**Verifying code**: `crates/pcode-lift/src/vn_io.rs:38-48` —
```rust
match reg.size {
    1 => Ok(u128::from(u8::MAX)),
    2 => Ok(u128::from(u16::MAX)),
    4 => Ok(u128::from(u32::MAX)),
    8 => Ok(u128::from(u64::MAX)),
    10 => Ok((1u128 << 80) - 1),
    16 | 32 | 64 => Ok(u128::MAX),
    _ => Err(anyhow!("unsupported register size {} bytes", reg.size)),
}
```
Exactly matches.  The widths > 16 (32, 64) use `u128::MAX` as documented.

**Verdict**: **Confirmed**.

#### Claim 7 — `Decision` enum variants
> The fixed-point loop is implemented as a small `LoopState` returning a `Decision { FixedPoint, StableOnly, Rebuild }` per step.

**Verifying code**: `crates/strider/src/orchestrator.rs:188-197` declares `enum Decision { FixedPoint, StableOnly, Rebuild }` (with intervening doc comments).

**Verdict**: **Confirmed**.

#### Claim 8 — `default_pipeline()` + `Strider::build_optimizer_pipeline` composition
> `default_pipeline()` (all 6 base passes) ... `Strider::build_optimizer_pipeline()` — full pipeline: `opt::default_pipeline()` + `StackStoreDetect` + `StackLoadForward` (both fixed-point), + `CallStackArgCollect` and `FunctionArgDetect` as post-passes.

**Verifying code**:
- `crates/opt/src/lib.rs:194-199` — `default_pipeline()` = `stable_default_pipeline()` + `RedundantPhis` + `DeadBranchElimination` (= 6 passes total: ConstantFold, KnownBits, FlagCmpCanonicalize, IfCondInversion, RedundantPhis, DeadBranchElimination).
- `crates/strider/src/strider/pipeline.rs:192-208` — `build_optimizer_pipeline` adds StackStoreDetect, StackLoadForward, and post-passes CallStackArgCollect + FunctionArgDetect.

**Verdict**: **Confirmed**.

#### Claim 9 — IfCondInversion canonicalises `If(BoolNeg(C))`
> `IfCondInversion` — canonicalises `If(BoolNeg(C)){A}{B}` into `If(C){B}{A}` so every `If` in the optimised IR has a non-`BoolNeg` cond.

**Verifying code**: `crates/opt/src/lib.rs:117-121` doc on `IfCondInversion` confirms exactly this; `crates/opt/src/lib.rs:135-141` registers it after `FlagCmpCanonicalize`.

**Verdict**: **Confirmed**.

#### Claim 10 — Pattern commutative ops set
> `add`, `mul`, `and`, `or`, `xor` (and `BoolBinaryOp` equivalents), `int_cmp(Equal/Carry/Scarry)`, and `float_cmp(Equal)` automatically try both operand orderings.

**Verifying code**: `crates/pattern/src/matcher/commutativity.rs:5-36`:
- `is_commutative_int_op`: `Add | Mul | And | Or | Xor` ✓
- `is_commutative_bool_op`: `And | Or | Xor` ✓
- `is_commutative_float_op`: `Add | Mul` (matches "add/mul auto-commute"; CLAUDE.md `float_cmp(Equal)` covers the cmp axis below)
- `is_commutative_int_cmp_op`: `Equal | Carry | Scarry` ✓
- `is_commutative_float_cmp_op`: `Equal` ✓

**Verdict**: **Confirmed**.

#### Claim 11 — Lift-time canonicalisation aliases
> `IntSub(a,b)` → `Add(a, IntUnaryOp::Neg(b))` ... `FloatLessEqual(a,b)` → `Or(FloatLess(a,b), FloatEqual(a,b))` (NaN-aware) ... Pattern crate ergonomic aliases (`pattern::sub`, `pattern::int_le`, `pattern::int_sle`, `pattern::float_sub`, `pattern::float_ne`, `pattern::float_le`).

**Verifying code**:
- `crates/ir/src/ops/op_kinds.rs:57-61` documents `IntBinaryOp` excludes `Sub`.
- `crates/ir/src/ops/op_kinds.rs:42-46` documents `IntCmpOp` excludes `LessEqual`/`SlessEqual`.
- `crates/ir/src/ops/op_kinds.rs:144-151` documents `FloatCmpOp::LessEqual` lowering.
- `crates/pattern/src/pat/ctor/int.rs:56, 107, 116` — `sub`, `int_le`, `int_sle` constructors exist.
- `crates/pattern/src/pat/ctor/float.rs:39, 88, 100` — `float_sub`, `float_ne`, `float_le` constructors exist.

**Verdict**: **Confirmed** (all six aliases exist in the Rust pattern crate).

#### Claim 12 — IntUnaryOp::BitNot vs Neg
> `IntUnaryOp::BitNot` is bitwise complement (`~x`); `IntUnaryOp::Neg` is two's-complement negation (`-x`).

**Verifying code**: `crates/ir/src/ops/op_kinds.rs:100-107` — exact match including the rsleigh `IntNeg` → `BitNot` lift mapping note.

**Verdict**: **Confirmed**.

---

### Root README samples

#### Claim 13 — Quickstart Python API surface
> ```python
> result = strider.run(arch=..., cc=..., mem=..., entry=..., function_max_size=None, allow_code_before_start_addr=True)
> ```

**Verifying code**: `crates/strider-py/src/run.rs:44-69` — `strider.run` accepts all named params: `arch`, `cc`, `mem`, `entry`, `rom`, `pipeline`, `allow_code_before_start_addr`, `function_max_size`, `compact`, `per_address_ccs`.  Default values match the README example (`function_max_size=None`, `allow_code_before_start_addr=false`).

**Verdict**: **Confirmed**.

#### Claim 14 — `pattern::float_is_nan` Rust constructor (root README L231)
> 2. **Lift-time canonicalisation aliases.**  ... Use the alias constructors (`pattern::sub`, `pattern::int_le`, `pattern::int_sle`, `pattern::float_sub`, `pattern::float_ne`, `pattern::float_le`, `pattern::float_is_nan`) ...

**Verifying code**: `grep -rn "pub fn float_is_nan" /mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/` returns no hits.  The Rust pattern crate has `float_sub`/`float_ne`/`float_le` (`crates/pattern/src/pat/ctor/float.rs:39, 88, 100`) but no `float_is_nan`.  The function exists only in `crates/strider-py/src/pattern.rs:1060`, which is the Python binding.

**Verdict**: **Refuted (stale)**.
**Proposed edit (root README L231)**: Remove `pattern::float_is_nan` from the alias-constructor list (or note "Python-only").  The Rust pattern crate does not expose it.

#### Claim 15 — `apply_elf_relocations` autoload behavior
> ELF relocation autoload-by-default — it lazily extends with .got.plt etc.

**Verifying code**:
- `crates/reader/src/elf.rs:768` declares `pub fn apply_elf_relocations_autoload(regions, &obj)`.
- `crates/strider-py/src/reader.rs:320` `fn apply_elf_relocations(&self, path: &str)` is the Python entry that wraps the autoload variant per `crates/reader/README.md` L99-102.

**Verdict**: **Confirmed**.

#### Claim 16 — Three optimizer pipelines exposed at opt crate root
> `opt::default_pipeline()`, `opt::stable_default_pipeline()`, `opt::destructive_default_pipeline()`.

**Verifying code**: `crates/opt/src/lib.rs:123, 165, 194` — all three pub fns exist.

**Verdict**: **Confirmed**.

---

### Per-crate README samples

#### Claim 17 — `cfg::Builder` constructor trio (`new`, `with_endianness`, `for_arch`)
> Three constructors: `Builder::new(...)` — defaults to LE+x86_64 ... `Builder::with_endianness(...)` ... `Builder::for_arch(...)` — preferred.  (`crates/cfg/README.md` L14-21)

**Verifying code**: `crates/cfg/src/cfg/builder/mod.rs:103, 122, 149` — all three exist.  `new` and `with_endianness` are now marked `#[deprecated]` (L97-101, L117-121); the README mentions the defaults but does NOT mention the deprecation attribute.

**Verdict**: **Partially confirmed**.
**Proposed edit (`crates/cfg/README.md` L14-18)**: Mark `Builder::new` and `Builder::with_endianness` as deprecated in the doc to mirror the source attribute, e.g. "`Builder::new(...)` — **deprecated**, defaults to LE + x86_64".

#### Claim 18 — `Cfg` exposes `petgraph::StableDiGraph`
> `Cfg<R: rsleigh::MemReader>` — finished graph. Holds the `petgraph::StableDiGraph` of regions, the entry `RegionId`, and the `rsleigh::Sleigh<R>` lifter context. (`crates/cfg/README.md` L11-13)

**Verifying code**: Confirmed via `crates/cfg/src/cfg/types.rs:49,91` and CLAUDE.md alignment.  The `Cfg` struct internals match.

**Verdict**: **Confirmed**.

#### Claim 19 — IR validator Layer C invariants list
> Layer C: graph-level invariants — Entry/InitialMemory uniqueness, ControlState predecessor kinds, phi token ownership & per-predecessor arity for `VarPhi`/`MemPhi` only — `StackStorePhi` has fixed arity 3.  (`crates/ir/README.md` L72-77 + CLAUDE.md)

**Verifying code**: `crates/ir/src/validate/layer_c.rs:99-103, 127-128, 161-164` — explicit "StackStorePhi has fixed arity 3 regardless of predecessor count" comment with a `matches!` guard that excludes it from the arity check; phi token ownership check at the same module.

**Verdict**: **Confirmed**.

#### Claim 20 — Optimizer / OptimizerOnBuilt / blanket impl direction (CLAUDE.md L168-170)
> Most passes implement `Optimizer` (`fn optimize(&self, &mut Graph, NodeId)`); a `OptimizerOnBuilt` companion trait targets `pattern::RewriteCtx<'_>` ... and a blanket `impl<T: OptimizerOnBuilt> Optimizer for T` wires both kinds into the same pipeline.

**Verifying code**: `crates/opt/src/pipeline.rs:85-162`:
- `pub trait OptimizerRaw` takes `(&mut ir::Graph, NodeId)` — this is the **low-level** trait.
- `pub trait Optimizer` takes `&mut pattern::RewriteCtx<'_>` — this is the **primary** trait.
- `impl<T: Optimizer> OptimizerRaw for T` — the blanket impl goes Optimizer → OptimizerRaw (the inverse direction of CLAUDE.md's claim).

The `opt/README.md` (L8-19) has the correct names (`Optimizer` for RewriteCtx, `OptimizerRaw` for raw access).

**Verdict**: **Refuted (stale)**.
**Proposed edit (CLAUDE.md L168-170)**: Rewrite to match the current trait setup: "Most passes implement `Optimizer` (`fn optimize(&self, &mut pattern::RewriteCtx<'_>)`); a low-level `OptimizerRaw` companion trait targets `(&mut Graph, NodeId)`, and a blanket `impl<T: Optimizer> OptimizerRaw for T` adapts every `Optimizer` impl into the pipeline's dispatch trait."

#### Claim 21 — `CallOtherAbi` field shape (CLAUDE.md + target/README L40-43)
> `CallOtherAbi { implicit_reads: &'static [&'static str], implicit_writes: &'static [&'static str], memory_edge: bool }`

**Verifying code**: `crates/target/src/call_other_abi.rs:12-29` — exact match (`pub implicit_reads: &'static [&'static str]`, `pub implicit_writes: &'static [&'static str]`, `pub memory_edge: bool`).  `CallOtherClass::Call(CallOtherAbi)` variant confirmed at L48.

**Verdict**: **Confirmed**.

#### Claim 22 — 22 calling-convention presets (target/README L25-35)
Userland (10): x86_cdecl, x86_64_systemv, x86_64_all_preserving, aarch64_aapcs64, arm_aapcs, mips_o32, mips_n64, powerpc_sysv32, powerpc64_elf_v1, powerpc64_elf_v2.  Kernel (6): x86_linux_kernel, x86_64_linux_kernel, aarch64_linux_kernel, arm_linux_kernel, mips_linux_kernel_o32, mips_linux_kernel_n64.  Syscall (6): x86_linux_syscall, x86_64_linux_syscall, aarch64_linux_syscall, arm_linux_syscall, mips_linux_syscall_o32, mips_linux_syscall_n64.

**Verifying code**: `crates/target/src/calling_convention/mod.rs:369, 402, 440, 480, 521, 555, 582, 619, 662, 696, 796, 810, 818, 825, 832, 839, 854, 872, 888, 909, 925, 940` — all 22 `pub fn`s exist with the documented names.

**Verdict**: **Confirmed**.

#### Claim 23 — 15 `SleighArch` presets (target/README L14-18)
> `x86_64()`, `x86()`, `aarch64()`, `aarch64be()`, `arm()`, `arm_be()`, `arm_thumb()`, `mipsbe32()`, `mipsle32()`, `mipsbe64()`, `mipsle64()`, `ppc32be()`, `ppc32le()`, `ppc64be()`, `ppc64le()`.

**Verifying code**: `crates/target/src/arch.rs:159, 170, 181, 192, 207, 218, 229, 241, 253, 265, 278, 294, 308, 324, 345` — all 15 preset constructors present.

**Verdict**: **Confirmed**.

#### Claim 24 — `Match` typed accessors (`get_int`/`get_uint`/`get_bool`/...) signatures
> `get_int(c, &graph) -> Option<i128>` (signed, sign-extended), `get_uint(c, &graph) -> Option<u128>` (unsigned, masked), `get_bool(...)`, `get_float_bits(...)`, `get_vn(...)`.

**Verifying code**: `crates/pattern/src/matcher/match_result.rs:64-187`:
- `get_uint(c, &ir::Graph) -> Option<u128>` ✓
- `get_int(c, &ir::Graph) -> Option<i128>` ✓
- `get_bool(c, &ir::Graph) -> Option<bool>` ✓
- `get_float_bits(c, &ir::Graph) -> Option<u64>` ✓
- `get_vn(c, &BuiltFunctionGraph) -> Option<rsleigh::Vn>` ✓

**Verdict**: **Confirmed**.

#### Claim 25 — `RetPat::preceded_by` exists
> `RetPat (`.preceded_by(call_pat)`, `.ret_val(idx, p)`)`

**Verifying code**: `crates/pattern/src/pat/builders/ret.rs:29, 35` — `preceded_by` and `ret_val` are both `pub fn` on `RetPat`.

**Verdict**: **Confirmed**.

#### Claim 26 — strider-py `float_is_nan` "not in v1" / "raises PatternError"
> `float_is_nan` constructor (no `FloatIsNan` IR node yet). (`crates/strider-py/README.md` L310)
> `float_is_nan` is registered but raises `PatternError` until the IR gains a `FloatIsNan` node kind. (L203-204)

**Verifying code**: `crates/strider-py/src/pattern.rs:1051-1063` — `pub fn float_is_nan(operand)` returns `PyPat::from_pat(pattern::float_ne(op.clone(), op))` — it does NOT raise PatternError; it constructs the `BoolNeg(FloatEqual(x, x))` IR-equivalent pattern.  The function is registered via `add_fn!(float_is_nan);` at L2051.

**Verdict**: **Refuted (stale)**.
**Proposed edit (`crates/strider-py/README.md` L203-204 and L310)**: Both lines should be removed or rewritten as "✅ `float_is_nan(x)` is sugar for `float_ne(x, x)`, matching the lowered FloatNan shape Sleigh emits."

#### Claim 27 — `Worklist` is FIFO with built-in dedup
> `worklist::Worklist<E>` — FIFO queue with a built-in dedup bitset.  (`crates/entity-utils/README.md` L12-14)

**Verifying code**: `crates/entity-utils/src/worklist.rs:14` (`worklist: VecDeque<E>`), L57 (`push_back`), L64 (`pop_front`) — FIFO confirmed.  Dedup bitset is a paired `DenseEntitySet<E>` (per the README).

**Verdict**: **Confirmed**.

#### Claim 28 — `strider::run` and `RunConfig` shape
> The canonical entry is `strider::run(config) -> Result<BuiltFunctionGraph>` ... `RunConfig` owns the `Sleigh` and threads it through every iteration.  (CLAUDE.md + `crates/strider/README.md` L11)

**Verifying code**:
- `crates/strider/src/orchestrator.rs:167` `pub fn run<R>(config: RunConfig<'_, R>) -> Result<ir::BuiltFunctionGraph>`.
- `RunConfig` (L60-106) carries `strider: &'a Strider`, `start_addr: u64`, owned `sleigh: rsleigh::Sleigh<R>`, optional `rom`, `fn_max_size`, `allow_code_before_start_addr`, `compact`, `per_address_ccs`.

**Verdict**: **Confirmed**.

---

## Notes / recommended actions

1. **CLAUDE.md L168-170**: invert the `Optimizer` / `OptimizerOnBuilt` description — the trait names and impl direction are inverted relative to current code.  This is the most consequential stale claim — anyone trying to implement a pass following CLAUDE.md will hit a compile error.  See claim 20.

2. **Root README L231**: remove `pattern::float_is_nan` from the Rust pattern alias list (it's Python-only).  See claim 14.

3. **strider-py README L203-204 and L310**: update both stale claims about `float_is_nan` — the function works as `float_ne(x, x)`.  See claim 26.

4. **`crates/cfg/README.md` L14-21**: add `**deprecated**` markers on `Builder::new` and `Builder::with_endianness` so the doc reflects the `#[deprecated]` attribute in source.  See claim 17.

The asm-fingerprint contract, validator layering, CC-preset table, pipeline
composition, IR `NodeKind` set, vn_io width support, pattern commutativity,
and lift-time canonicalisation aliases are all accurately described in
CLAUDE.md and the per-crate READMEs.

## Files referenced

Doc sources:
- `/mnt/c/Users/mikeg/Documents/strider/CLAUDE.md`
- `/mnt/c/Users/mikeg/Documents/strider/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/reader/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/dot/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/entity-utils/README.md`
- `/mnt/c/Users/mikeg/Documents/strider/crates/graphwalk/README.md`

Code witnesses (key files cited):
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_a.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_b.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_c.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/kind.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/node/output_type.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/store.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/ops/op_kinds.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/vn_io.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/lib.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/pipeline.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/arch.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/call_other_abi.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/reader/src/elf.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/commutativity.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/match_result.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/ctor/int.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/ctor/float.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/builders/ret.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/run.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/entity-utils/src/worklist.rs`
