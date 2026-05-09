# `pcode-lift` — pure value-producing pcode → IR lifter

The per-opcode value-lifting logic, factored out of [`strider`](../strider).
Translates a single `rsleigh::Insn` (one p-code instruction) into [`ir`](../ir)
nodes for every value-producing opcode. Control-flow / call / store opcodes
return `Ok(false)` so the caller can route them through its own region-aware
machinery.

## Public surface

- `ValueLifter<'a, R: rsleigh::MemReader>` — the lifter context. Holds
  borrows to the `ir::FunctionBuilder` being filled, the `rsleigh::Sleigh<R>`
  context (for address-space / register metadata), and the target
  `Endianness`. Stateless beyond those borrows.
- `ValueLifter::new(builder, sleigh, endianness)` — constructor.
- `ValueLifter::lift(insn) -> Result<bool>` — `Ok(true)` if `insn`'s opcode
  is value-producing and was lifted; `Ok(false)` if it's a control-flow /
  call / store op the caller must route. `Err(_)` for malformed
  instructions.
- `value` module — per-opcode handlers (private bodies, public dispatcher
  invoked by `ValueLifter::lift`).
- `vn_io` module — register aliasing (`read_vn`, `write_vn`,
  `find_largest_fitting_register`, `vn_mask`). All reads / writes of
  overlapping registers (x86 `rax`/`eax`/`ax`/`al`/`ah`, AArch64
  `q0`/`d0`/`s0`, x87 `ST*`, XMM/q-register containers) go through the
  largest containing register with shift / mask ops for sub-register
  slices.
- `vn_sort_key(vn)` — stable sort key for `rsleigh::Vn` so two lifters
  (e.g. cfg's mini-IR and strider's per-region IR) produce the same
  `VarId` numbering.
- `first_input_or_err(insn)` / `decode_space_id(insn)` /
  `require_output_vn(insn)` — small shared helpers used by the per-opcode
  handlers.
- `Result<T>` alias (`anyhow::Result<T>`).

## Architecture

`src/lib.rs` exposes the `ValueLifter` struct and the small input-validation
helpers. The opcode dispatch lives in `src/value/` (one submodule per
opcode family — `arithmetic`, `bool_ops`, `cmp`, `casts`, `float`, `load`,
…), entered via `value::lift(self, insn)`.

`src/vn_io.rs` is the register-aliasing layer. Sleigh emits operations on
sub-registers (e.g. write to `al`) but the IR tracks one `VarId` per
*containing* register (`rax`). `read_vn` and `write_vn` insert
`Truncate` / `Extend` / `Insert { lsb, len }` / `Extract { lsb, len }`
nodes so the IR sees consistent register-sized values regardless of which
sub-slot the source operation named. Width support: 1, 2, 4, 8, 10
(x87 80-bit extended), 16 (XMM/q-register), 32 (YMM), 64 (ZMM) bytes.
Widths 32 and 64 use a degraded `u128::MAX` mask; sub-register aliasing
within > 16-byte containers raises an error.

The lifter is split out from `strider` so two callers can reuse it:
[`strider`](../strider)'s per-region IR translator (the main consumer), and
[`cfg`](../cfg)'s indirect-branch resolver (which builds a stand-alone
single-block mini-IR to classify a `BranchIndirect` producer shape).

## Key invariants

- **Value vs. control split**: `ValueLifter::lift` handles only
  value-producing opcodes. The caller must dispatch `Branch`, `CondBranch`,
  `BranchIndirect`, `Return`, `Call`, `CallIndirect`, `CallOther`, and
  `Store` itself based on the `Ok(false)` return value.
- **Largest-containing-register rule**: every `Vn` read/write goes through
  `find_largest_fitting_register`. A 1-byte write to `al` becomes an
  `Insert { lsb: 0, len: 8 }` into the current `rax` value; a 4-byte read
  of `eax` becomes a `Truncate(rax, U32)` (or, on big-endian, an `Extract`
  at the appropriate offset).
- **Endianness drives bit-shift formulas**: little-endian places `al` at
  `lsb = 0`, `ah` at `lsb = 8`; big-endian places them at the high end of
  the container. The endianness comes from `target::Endianness`.
- **Stable VarId numbering**: callers that key off `vn_sort_key` produce
  the same `VarId` ordering across runs. Without this, `HashSet` iteration
  order would let the random hasher seed leak into IR shapes.
- **CONST-space inputs**: `decode_space_id` requires `inputs[0]` to live in
  `rsleigh::VnSpace::CONST` and uses `unsafe VnSpace::by_id` to decode the
  pointer — sound because rsleigh only emits LOAD/STORE with a valid
  space-pointer encoding.

## Tests

Integration tests in `crates/pcode-lift/tests/value_lifter.rs`.

```
cargo test --package pcode-lift
```

## Gotchas

- Calling `lift` on a control-flow / call / store opcode is **not** an
  error — it returns `Ok(false)`. Callers that don't check the return
  value will silently miss control-flow handling.
- The lifter does not validate the resulting graph; the caller must run
  `ir::validate::validate` (typically via the `FunctionBuilder::build` end-of
  -build hook).
- `vn_io` assumes the `rsleigh::Sleigh` context has the architecture's full
  register table loaded (`SleighArch` + `pspec`). A custom Sleigh built
  without the standard register file will break aliasing.
- Depends on [`ir`](../ir), [`target`](../target), and `rsleigh`. No
  dependency on [`opt`](../opt) or [`pattern`](../pattern).
