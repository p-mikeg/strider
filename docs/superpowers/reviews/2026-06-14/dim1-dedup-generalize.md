# Dimension 1 audit — code simplification / generalization / utility helpers

Read-only deep audit of the entire Strider Rust workspace (15 crates).
Single dimension: duplicated logic, derivable function arguments, and
missing utility helpers. Every finding below was verified by reading the
actual code at the cited sites (comments / CLAUDE.md / memory were not
treated as ground truth).

User suspicion of memory handling (Load/Store, memory-SSA, sp_expr,
stack-args) was investigated first: the `sp_expr` subsystem
(`decompose` / `alias` / `cfg` / `mem_ssa` / `ranges`) and the two
stack-arg passes are already strongly factored — the shared
`SpAliasCfg` / `SpAliasOracle` / `MemorySSAWalker` / `reaching_store`
plumbing was clearly built specifically to remove the duplication that
once existed there. The remaining dedup opportunities are elsewhere (the
lift handler families and the IR builder), with one small residual in the
opt cone-walks.

## Findings by severity

- HIGH: 2
- MED: 5
- LOW: 4

Sorted by impact below.

---

## D1-01 — `ValueType::int_for_byte_size(<vn>.size)` everywhere (derivable arg + missing helper)  [HIGH, confidence very high]

The single most-repeated idiom in the value-producing path is converting a
varnode's byte size to a `ValueType`:

```rust
strider_ir::ValueType::int_for_byte_size(out_vn.size)?
```

50 total `int_for_byte_size(` call sites across `strider-ir` + `strider-lift`;
**32** of them pass a `.size` field of a varnode/space the caller already holds.
This is the canonical "derivable argument" anti-pattern the user described: the
function takes `size` when the caller has the whole `Vn`.

Representative concrete sites (all verified):
- `crates/strider-lift/src/lift/memory.rs:17`
- `crates/strider-lift/src/lift/integer.rs:42`
- `crates/strider-lift/src/lift/misc.rs:38`, `:49`, `:60`
- `crates/strider-lift/src/lift/arithmetic.rs:82`, `:103`, `:156`, `:214`, `:250`, `:328`
- `crates/strider-lift/src/lift/cast.rs:127`, `:133`, `:138`, `:146`, `:155`, `:185`, `:225`, `:231`, `:286`
- `crates/strider-lift/src/lift/float.rs:32`, `:217`, `:243`
- `crates/strider-lift/src/lift/vn_io.rs:36`, `:77`, `:85`
- `crates/strider-ir/src/builder/vn_io.rs:147`, `:196`, `:267` (verified earlier read; `reg.size` / `container_reg.size`)
- `crates/strider-ir/src/builder/call.rs:94` (`vn.size`, in loop), `:447` (`sp.size`)

The matching float idiom (`float_for_byte_size(vn.size)`) already has a
helper — `FunctionLifter::float_type_from_vn(vn)` in
`crates/strider-lift/src/lift/float.rs:21` — proving the pattern is worth a
named accessor. The integer side has no equivalent.

`rsleigh::Vn` is an external type, so the helper must be a free fn or
extension trait. Proposed (in `strider-ir`, next to `ValueType`):

```rust
// Extension trait re-exported from strider-ir.
pub trait VnTypeExt {
    fn int_type(&self) -> crate::Result<ValueType>;   // int_for_byte_size(self.size)
    fn float_type(&self) -> crate::Result<ValueType>; // float_for_byte_size(self.size)
}
impl VnTypeExt for rsleigh::Vn { /* … */ }
```

Then `int_for_byte_size(out_vn.size)?` → `out_vn.int_type()?` and
`float_type_from_vn(out_vn)?` → `out_vn.float_type()?` (collapsing that
helper too). Rough LOC saved: ~30 net, plus it removes the longest single
token-sequence repeated in the codebase and gives one place to attach the
"unsupported width" diagnostic.

---

## D1-02 — Value-family lift handler boilerplate: `read → coerce → build → write_vn`  [HIGH, confidence high]

Every value-producing pcode handler repeats the same four-step skeleton:
read input(s), compute `out_ty` from `out_vn`, coerce operands to `out_ty`
via `convert_to_int_if_needed` / `cast_to_float_if_needed`, build the op,
`write_vn(out_vn, result)`. The unary/binary/cmp cases were already pulled
into `process_int_unary_op` / `process_int_binary_op` / `process_int_cmp_op`
/ `process_float_*`, but the surrounding "open" and "close" boilerplate is
still copy-pasted in ~20 handlers.

Concrete duplicate sites (the `let out_vn = require_output_vn(insn)?; let
out_ty = …int_for_byte_size(out_vn.size)?; … self.write_vn(out_vn, …)`
envelope):
- `crates/strider-lift/src/lift/arithmetic.rs:74-86` (unary), `:99-110`
  (neg-as-xor), `:148-186` (binary)
- `crates/strider-lift/src/lift/integer.rs:16-20` (copy), `:29-46` (extend)
- `crates/strider-lift/src/lift/cast.rs:143-150` (popcount), `:152-159`
  (lzcount) — these two are *identical* except `build_popcount` vs
  `build_lzcount`
- `crates/strider-lift/src/lift/misc.rs:20-41`, `:44-52`, `:55-63`
- `crates/strider-lift/src/lift/float.rs:41-54`, `:57-68` (binary/unary
  share the read+`float_type_from_vn`+coerce+`write_float_to_vn` envelope)

The `read → cast_to_float → build_*_op → write_float_to_vn` envelope in
`process_float_binary_op` / `process_float_unary_op` is character-for-character
the same except the op-builder call.

Two concrete sub-helpers (both verified to fit the existing call shapes):

```rust
// Single-input unary value op at output width: read in0, coerce to out_ty,
// run `build`, write out. Covers popcount/lzcount/int-unary.
fn lift_int_unary(
    &mut self, insn: &rsleigh::Insn,
    build: impl FnOnce(&mut FunctionBuilder, Value, ValueType) -> Result<Value>,
) -> Result<()>;
```

`handle_popcount` / `handle_lzcount` then become one line each
(`self.lift_int_unary(insn, |b,v,t| b.build_popcount(v,t))`), removing the
two fully-identical bodies at `cast.rs:143` and `:152`. Rough LOC saved
across the family: ~40-60. (Lower-bound the claim to the two identical
popcount/lzcount bodies if the broader refactor is judged risky.)

---

## D1-03 — `call_ret_vals_for` / `call_clobbered_for` share the CC-register→container chain  [MED, confidence high]

Both methods build the same iterator chain over the CC's return/clobber
register lists mapped through `container_of`:

```rust
cc.ret_val_regs.iter().chain(cc.ret_val_regs_float.iter())
    .map(|v| self.container_of(v))…
```

Sites (per the builder/function audit, to be re-confirmed at edit time):
- `crates/strider-ir/src/function/data.rs:402-405` (`call_ret_vals_for`)
- `crates/strider-ir/src/function/data.rs:436-440` (`call_clobbered_for`)
- `crates/strider-ir/src/function/data.rs:418` (`ret_val_regs`)
- the FunctionBuilder ctor chains the same CC lists.

Proposed: one iterator helper on `BuiltCallingConvention`
(`combined_ret_regs() -> impl Iterator<Item = &Vn>`) plus a
`container_of`-mapping collector on `Function`. LOC saved ~25.

---

## D1-04 — Hand-written Python builder init + field-setter boilerplate (strider-py)  [MED, confidence high]

The convenience pyfunctions that construct a builder and set one field
repeat `let b = Builder::new(); b.field.replace(Some(v)); b` verbatim, and
the hand-written builders (`PyCallPat`, `PyIfPat`) re-implement the
`borrow_mut().field = Some(p)` setters that `node_builder!` already
generates for the macro-built ones.

Init sites: `crates/strider-py/src/pattern.rs:2467-2472` (load),
`:2622-2627` (call), `:2776-2781` (if_), `:2810-2813` (phi_for),
`:2922-2925`/`:2936-2942`/`:2947-2954` (function_arg variants).
Setter sites: `crates/strider-py/src/pattern.rs:2592-2615` (PyCallPat, 6
setters), `:2748-2761` (PyIfPat, 3 setters).

Proposed: a small `builder_with_init!` macro for the init shape and a
`field_setter!` macro (or extending `node_builder!` coverage to Call/If's
fixed fields). LOC saved ~50. NOTE: MEMORY claims strider-py is "~2%
reducible / macros already spent"; this is a NEW concrete pocket in the
hand-written (non-macro) builders, so it is worth re-checking against that
prior conclusion before acting.

---

## D1-05 — `compile_repr_match` / `compile_repr_template` arm boilerplate (strider-py)  [MED, confidence high]

`crates/strider-py/src/pattern.rs:548-765` (match, ~90 arms) and
`:805-938` (template, ~40 arms) each repeat the
`PatRepr::X(v) => { let v=*v; DynMatch(Box::new(move |b| mc(strider_pattern::x(v), b))) }`
closure-wrapping shape per node kind. The differences are only the target
`strider_pattern::*` fn and whether operands recurse. A
`wrap_const!` / `wrap_unary!` / `wrap_binary!` arm macro would collapse the
near-identical arms. LOC saved ~100-150 (largest single block in py), but
this is mechanical churn over a stable surface — MED because the per-arm
divergence (operand recursion) needs care.

---

## D1-06 — `mask + build_int_const + And/Or` construction chain in register aliasing  [MED, confidence medium-high]

`crates/strider-ir/src/builder/vn_io.rs` builds the sub-register
read/write masks with a repeated `build_int_const(mask, ty)` →
`build_int_binary_operation(_, _, And|Or, ty)` pair (the
`build_masked_insert` body, ~lines 322-349, uses it 2-3 times; the same
const-then-binop shape recurs in `cast.rs` lowerings, e.g.
`handle_extract:267-269`, `handle_insert` via `build_bit_field_insert`,
`handle_subpiece:125-134`). A builder convenience:

```rust
fn build_const_binop(&mut self, k: u128, x: Value, op: IntBinaryOp, ty: ValueType) -> Result<Value>;
```

would remove the intermediate `let kc = build_int_const(...)` line at each
mask/shift site. LOC saved ~20.

---

## D1-07 — `lower_cmp_negated`-style negation also exists float-side; the `Xor(_, IntConst(1)):I1` shape is built in 4 places  [MED, confidence high]

The "logical NOT at I1 = `Xor(cmp, IntConst(u128::MAX as I1))`" construction
appears as a hand-rolled trio (build cmp, build I1 one, build xor) in:
- `crates/strider-lift/src/lift/arithmetic.rs:258-270` (`lower_cmp_negated`)
- `crates/strider-lift/src/lift/float.rs:109-129` (`build_float_eq_negated`)

Both already factor their own family, but the *terminal* shape
(`one = build_int_const(u128::MAX, I1); build_int_binary_operation(x, one,
Xor, I1)`) is identical. A single `IRBuilderExt::build_logical_not(x:
Value) -> Result<Value>` would be reused by both lifter helpers and is a
natural pattern-canonical primitive (the codebase canonicalises NOT to
exactly this shape). LOC saved ~12, plus it documents the canonical NOT in
one place. Confidence high; the two sites are verified.

---

## D1-08 — opt cone-walk: two iterative input-producer DFS collectors  [MED→LOW, confidence high]

Two opt passes hand-roll an iterative DFS over input-producers that
collects interior nodes, gated by a descend-predicate:
- `crates/strider-opt/src/opt/known_bits/mod.rs:626-676`
  (`build_cone_fingerprint_memo`, postorder + memo, descend gated by
  `propagates_known_bits`)
- `crates/strider-opt/src/opt/flag_cmp_canonicalize/mod.rs:486-511`
  (`absorb_cr_pack_fingerprints`, preorder collect, descend stops at
  `IntCmpOp`)

Both push `node_inputs(n).map(producer)`, dedup via a seen set, and stop
descent on a predicate. They differ (postorder-with-memo vs
preorder-collect), so a full merge is awkward, but a shared
`cone_inputs_dfs(ctx, roots, descend: impl Fn(NodeKind)->bool) ->
Vec<NodeId>` would back the preorder case directly and seed the postorder
one. Severity MED at best (the postorder/memo variant resists collapse);
LOC saved ~20. Several other passes also repeat the
`node_inputs(n).iter().map(|i| producer(i))` micro-idiom — a
`fn input_producers(&self, NodeId) -> impl Iterator<Item=NodeId>` on
`IRViewer` would clean those up workspace-wide (verified instances at
`flag_cmp_canonicalize/mod.rs:499-503`, `known_bits/mod.rs:636-640`,
`:666-672`).

---

## D1-09 — `node_inputs_exact::<2>` "two operands of a binary op" idiom  [LOW, confidence high]

47 `node_inputs_exact::<2|3>` sites across `strider-opt` + `strider-ir`.
The `::<2>` ones are almost all "give me a binary op's two operands" with
the same `.expect("…2 inputs (validated)")` tail (e.g.
`sp_expr/decompose.rs:149-151`, `:176-178`). A typed accessor
`binary_operands(&self, NodeId) -> (ValueId, ValueId)` (or
`store_addr_data(&self, NodeId) -> (ValueId, ValueId)` for the `::<3>`
Store case, used at `sp_expr/alias.rs:178-181`, `cfg.rs:201-204`) would
remove the repeated array-destructure + expect. LOC saved ~15-20; LOW only
because each site is already short.

---

## D1-10 — Py optimizer class registration list duplicates the pass enum (strider-py)  [LOW, confidence high]

`crates/strider-py/src/opt.rs:534-554` hand-lists `m.add_class::<…>()?` for
~14 pass classes that already enumerate in the `PyOptPass` enum
(`:462-476`) and the `pure_pass_class!` / `cc_aware_pass_class!` macro
invocations. A `register_classes!` macro driven by one list removes the
third hand-maintained copy. LOC saved ~15.

---

## D1-11 — Dot edge/virtual-node attribute literals (strider-ir)  [LOW, confidence medium]

`crates/strider-ir/src/function/dot/render.rs:144-154` and `:214-220`
build dot edges with repeated `&[("color", …), ("label", …), …]` tuple
arrays. A tiny `DotEdge` attribute builder would dedup the literal arrays.
LOC saved ~10. Lowest priority.

---

## Notes / non-findings (verified clean)

- `sp_expr` (`decompose` / `alias` / `cfg` / `mem_ssa` / `ranges`) and the
  `function_args` / `call_stack_args` passes: the shared `SpAliasCfg`,
  `SpAliasOracle`, `MemorySSAWalker`, and `reaching_store` already collapse
  what the user suspected. `reaching_store`'s `probe_size` design is the
  intended generalization (discovery vs width-sensitive consumers share one
  walk). No actionable duplication here.
- `pcode_util` (`require_output_vn` / `nth_input_or_err` /
  `ensure_const_space` / `decode_space_id`) is already the right shared
  decode layer; the lift handlers use it consistently.
</content>
</invoke>
