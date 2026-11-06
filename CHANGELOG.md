# Changelog

## 0.3.0 (unreleased)

### Breaking, Python

These change an answer without an error:

- A `one_of` / `first_of` arm nests through its slot's edge:
  `store().mem(one_of([call().res()]))` and the `load()` twin, which matched
  nothing, match the call's memory token.
- A `call()` / `call_other()` in a raw slot (`input`, `any_input`,
  `phi_token`, `call_other().arg`) also binds the call's control and memory
  edges; `.res()` keeps it to the value.
- `into_pat()` holds the builder by reference: a later edit to the builder
  changes what the `Pat` matches, and it answers every query.
- `BoundCapture.__eq__` compares at full width: `m[c] == 2**300` can be
  `True`.
- `find_all` returns one match per bound output:
  `call().any_output().capture(c)` on an n-output call gives n matches, and
  `find_unique` raises there.
- `ElfLifter.analyze` / `build_cfg` follow a function's GCC `<name>.cold` and
  `<name>.cold.N` fragments instead of ending at a tail call.
- `BoundCapture`'s readers are methods: `m[c].uint == 4` is `False`; call
  `m[c].uint()`.
- An object file's defined IFUNC symbol sits at the unmapped extern address a
  call to it targets, not at its resolver: its `Symbol.address` changes, its
  `size` is `None`, it is not `is_function`, `functions()` leaves it out, and
  `analyze` of it raises.
- `asm_fingerprint` of a constant a compare or integer binary op reads
  through a register no longer includes that instruction's address.
- `symbols()`, `iter_symbols()` and `symbol_at` leave out ARM / AArch64
  mapping symbols (`$a`, `$d`, `$t`, `$x`, `$d.N`).
- `CallingConvention.name()` of a `preserves_all()` / `preserves_regs()`
  variant is `"<base>.preserves_all"` / `"<base>.preserves_regs"`.
- `load_elf(from_segments=False)` serves no section of a writable PT_LOAD
  as ROM.
- A carry after `cmn` / `adds` / x86 `add` becomes one unsigned `Less`, so
  HI / LS / `ja` / `jbe` sites match an unsigned-compare pattern.

These raise:

- An out-of-range int for a fixed-width argument (`int_const_any_width`,
  `float_const`, `stack_offset`, `source_stack`, `function_arg_stack`,
  `custom()`'s stack fields) raises `OverflowError`, where it raised
  `TypeError` or `StriderError`.
- `entry()` / `region()` as a value operand raise at query time; they
  returned `[]`.
- `find_unique_value` raises `StriderError` for a capture no pattern binds;
  it returned `None`.
- `IfPat.true_branch` / `false_branch` are gone: join on the edge,
  `[if_else().capture_true(t), region().any_input(t)]`.
- `Cfg.is_complete()` and the per-reason `Cfg` getters are gone:
  `cfg.incomplete` lists every `(address, reason)`, the reasons in
  `strider.cfg.INCOMPLETE_REASONS`. `analyze` and `build_cfg` report
  incompleteness rather than raise it; only an unmapped entry, or a
  fall-through no call explains into bytes Sleigh rejects, raises
  `StriderError`.
- `CallingConvention.no_return()` is gone: list the callee in
  `CfgOptions(no_return_targets=...)`, which `build_cfg` honours too.
- `CfgOptions.with_function_max_size`, `with_data_ranges` and
  `LifterOptions.with_cfg` are gone: pass the fields to the constructors.
  `function_max_size` with `allow_code_before_start_addr=True` raises
  `ValueError`.
- `strider.opt.CfgDetach` and `StackOffsetDetect` are gone: the default
  pipeline does their work.
- `Lifter.optimize(function, pipeline, opts)` is
  `Function.optimize(assumptions=None, pipeline=None)`. Claims accumulate on
  the function (`Function.assumptions`); a set leaving out a claim already in
  force, or adding one the lift reads, raises.
- `Lifter.visualize` is `strider.explore.visualize(target, *, ...)`.
- `Lifter.reg` / `reg_name` and `strider.sleigh.Sleigh` are gone: use
  `SleighArch.reg` / `reg_name` / `name()`. `lifter=` is gone from
  `to_dot` / `neighborhood_dot`.
- `Node.wide_const_bytes()` is gone: `uint()` reads every width.
- Op-name aliases (`"eq"`, `"shl"`, `"add"`, `"zero"`) raise: pass the op's
  own name (`"Equal"`, `"ShiftLeft"`, `"Add"`, `"ZeroExtend"`).
- A `strider.pattern` pattern is no longer a rewrite right-hand side: use
  `strider.template`.
- `BoundCapture` has no truth value (`bool(m[c])` raises).
- `AssumptionOptions` is unhashable and compares by value; `SleighArch`,
  `CallingConvention` and `Symbol` compare and hash by value.
- `bool(CastMask)` is whether it selects any cast.
- `ElfLifter.analyze` / `build_cfg` given the wrong options type raise
  `TypeError`.
- A `MemReader.read` that raises, returns a non-bytes value or returns more
  bytes than asked fails the lift with `StriderError` (its exception as
  `__cause__`) at any address, not `unmapped_target` past the entry.
- A buffer with items wider than a byte (`array('H')`,
  `memoryview.cast('I')`) is refused where it was read one item per byte:
  `BufferReader` raises `TypeError`, a `MemReader.read` returning one fails
  the lift, a `ReadOnlyMemory.read` returning one reads as unmapped.
- `Lifter.call_other_abi` raises for an entry built against another arch.
- `ElfLifter.arch` and `reader()` raise while the handle is in use;
  `reader()` returns the handle's own `BufferReader`.
- An address, node id or operand index outside the unsigned range raises
  `StriderError`, not `OverflowError`; a `bool` there, `entry` included,
  raises `TypeError` where it read as 0 or 1.
- A non-int `entry` to `Lifter.analyze` / `build_cfg` (and
  `ElfLifter.build_cfg`) raises `TypeError`, not `StriderError`; `pcode_at`
  takes `entry` the same way.
- `of_width`, `value_of_width`, `inputs_of_width` and the `load()` /
  `store()` `bit_width` raise `StriderError` for a width no value type has;
  they matched nothing.
- A constant `int_sub` / `float_sub` costs 5 pattern nodes against the
  256-node cap, not 3.

Output that reads differently:

- `Node.kind()` / `repr(Node)` render `IntConst`, `InitialVar` and
  `CallOther` without the interner handle.
- `to_dot(pretty=True)` names a `Load` / `Store` space by its sla name.
- `Node.sint()` and the signed `Match` readers answer past 128 bits where they
  answered `None`; `int_const` takes an `int` of up to 512 bits.
- `StriderError.backtrace` read off the class is a property.

### Breaking, Rust

The crates are unpublished and their API changed throughout; the larger moves
are `Strider::analyze(entry, cc, &AnalyzeOptions, pipeline)`,
`DecodeEngine`, `ValueType` as `Int(u16)` / `Float(u16)`, integer constants
stored as `dashu-int` `UBig`, and a sealed `TemplatePat`. One silent change:
a repeated `CallOtherPat::name` or `PhiPat::for_vn` intersects where the last
replaced the rest.

- `Lifter::build_ir_with` takes `entry_values` (registers bound to a known
  value on entry, passed by `AnalyzeOptions::entry_values`);
  `OpdTable::toc_at` reads an ELFv1 descriptor's TOC pointer.
- `BuiltCallingConvention::validate` no longer requires a callee-saved
  link register; `validate_default` does, for a function's own convention.

### Added, Python

- Integer arguments take any `__index__` object (`BoundCapture`, numpy
  ints); `to_dot` / `to_html` take an `os.PathLike` path.
- `Capture("any_")` is an ordinary name; only `"_"` stays reserved.
- `CfgOptions(fragment_ranges=[(start, end)])`: ranges that also belong to
  the function (`ElfLifter` fills it from `.cold` symbols).
- `"past_function_end"` in `INCOMPLETE_REASONS`: code fell through past
  `function_max_size` or a `fragment_ranges` end, now a tail call where it
  was an error.
- `AssumptionOptions.fp_rounding_mode_is_nearest` and
  `msr_register_keeps_mode` (both default on, read by the lift);
  `AssumptionOptions.none(...)` takes keywords; `a | b` / `union`;
  `Function.assumptions`.
- `strider.sleigh.CC_PRESETS`, `CallingConvention.preset(name)`,
  `SleighArch.preset(name)`, `SleighArch.calling_conventions()`,
  `CallingConvention.to_dict()`, `custom(entry_address=,
  reg_args_adjoin_stack_args=)`.
- `CallingConvention.custom` / `CallOtherAbi.custom` take a `SleighArch`;
  `SleighArch.reg` / `reg_name` work without a lifter.
- `strider.template.int_ne`, `int_le`, `int_sle`, `float_ne` and
  `capture_typed`.
- `Symbol.local_entry` (ppc64 ELFv2).
- `strider.cfg.DOT_STYLES`, `strider.sleigh.ARCH_PRESETS`, and
  `strider.pattern.VALUE_TYPES`, `INT_CMP_OPS`, `INT_BINARY_OPS`,
  `BOOL_BINARY_OPS`, `FLOAT_BINARY_OPS` and `EXTEND_OPS`.
- A `Lifter` takes weak references, so a reader can point back at its
  handle without a cycle.
- `.ordered()` on `inputs_of_width` and `bool_inputs`.
- `Function.to_text()` prints `assumptions` and
  `frame_address_in_call_register` lines, and a REGISTER-space byte order
  that differs from the data order.

### Changed, Python

- A `Lifter` works on any thread. `analyze`, `build_cfg` and `add_elf` hold
  it alone; a `Lifter` call on it meanwhile raises `StriderError` ("already
  in use"), while `ElfLifter` symbol and memory reads still work.
- Every typed builder nests in every slot but a memory one; a memory slot
  takes `anything()`, `var(c)` and a bare `Capture`.
- `load()` / `store()` region filters apply in call order, the last winning.
- `function_arg_reg` / `source_register` match a sub-register of the carrier.
- `Function.rewrite` / `rewrite_all` skip a site whose replacement has
  another width or is not available where it is read, count each site once,
  and raise when a replacement would build nodes no instruction produced.
- `ElfLifter.build_cfg` derives `data_ranges` and ppc64 local entries from
  the ELF as `analyze` does.
- `ElfLifter.analyze` takes an `__index__` entry as an address.
- `ElfLifter.analyze` enters a ppc64 ELFv1 function with `r2` holding its
  `.opd` descriptor's TOC pointer, so TOC-relative loads and jump tables
  resolve as on ELFv2.
- `CallingConvention.custom` accepts a `link_register` missing from
  `callee_saved_regs`: a call to such a `per_address_ccs` callee clobbers
  it (the ppc32 `-mbss-plt` GOT thunk). `analyze` raises for one as the
  function's own convention.
- `analyze` refuses a calling convention whose stack geometry belongs to
  another arch; `CallingConvention.custom` checks it at construction.
- `AssumptionOptions(...)` takes its defaults from Rust (`None` means
  default) and keeps `noalias_allocators` sorted.
- `LoadForward`, `FunctionArgDetect` and return-target verification narrow
  under `stack_global_disjoint` and `noalias_allocators`.
- `ConstantFold` narrows a truncate of a wide op, folds integer constants
  past 128 bits, and collapses cast chains and constant bitcasts;
  `FlagCmpCanonicalize` folds a signed compare against an immediate to one
  `Sless` on x86, ARM, Thumb and AArch64; `PhiCollapse` folds `phi & C`.
  Patterns written against the source shape may no longer match.
- `DeadBranchElimination` folds a branch only if its surviving arm still
  reaches a terminator, and strips every dead `Region` predecessor.
- Error messages name the value-op constructor called, list accepted op
  names, spell value types as `i64`, and name the first unbound capture.
- A pattern compile reads its stack budget from the real thread stack on
  Windows and the BSDs too.

### Fixed

Matching and rewriting:

- `first_of` and `ignore_casts` settle in a fixed order: at each value the
  first listed arm (or outermost cast level) that matches wins.
- `int_const_any_width` with a negative value matches `I256` / `I512`
  constants.
- `.when()` on a nested `function_arg(..)` runs; `m[a] == m[b]` compares the
  captured constants in the abi3 wheel.
- A template constant at a shift count is built at the shift's width;
  `capture_typed` over a capture raises rather than panicking; a template
  `float_const` with bits above its width is refused.
- Rewrites skip a match through a `Phi` / `MemPhi` / `Region` whose
  replacement would form a data cycle or read an unavailable value.
- `repr()` of `CastMask`, `VnSpace`, a stale `BoundCapture` and a
  `preserves_all()` / `preserves_regs()` convention evaluates back or names
  the variant.
- A `StriderError` raised on a small thread, and a query compiled on one, no
  longer crash the process.
- `f.optimize()` keeps the `noalias_allocators` `analyze` ran under.
- `int_sub(x, K)` and `float_sub(x, K)` with an uncaptured constant `K` (a
  value, a list, `int_const_any_width`, bare or under one `of_width`) also
  match the folded `x + (-K)`.

Analysis:

- A ppc64 `rldicl` switch (`switch (x >> 28)`) bounds its table by the
  guard on the truncated rotate; it read past the table.
- The ppc64 Linux-kernel conventions clobber r13 (the per-CPU `paca`)
  across a call.
- x86 `wrmsr` writes `FS_OFFSET` / `GS_OFFSET`.
- A folded `popcount` / `lzcount` wraps to its output width.
- The ppc64 `*16_HIGHER34` relocations hole only their halfword.
- Call stack-argument collection and table classification are no longer
  quadratic in calls behind merges and in table sites over one index.
- Jump tables: index known bits, equality guards, phis of constants, bit
  fields, `rlwinm` guards and byte indices bound the seated arms; a guard the
  range analysis cannot read, or an unproven Thumb byte-table index, defers
  the site rather than seating cells past the table; a `cmov; jmp *reg` phi
  of labels resolves.
- Seeds: a one-target `known_targets` seed outside the function is a tail
  call; a `"return"` seed over a p-code `RETURN` is `"unverified_seed"`; a
  seeded arm that is unmapped, undecodable or past the function's end is
  dropped and reported; duplicate seed entries seat one arm.
- ARM: ThumbEE mode carries across direct and resolved indirect edges; a
  `blx` / `jalx` to a lower callee no longer switches the caller's mode;
  `msr` to the CPSR control field re-banks, and `mrs` reads the masks.
- Hard-float AAPCS arguments, and the MIPS `t9` / ppc64 `r12` entry address,
  read the entry value at an entry that is also a loop header.
- `per_address_ccs` overrides with another stack pointer are refused; they
  produced wrong stack offsets.
- `FunctionArgDetect` keeps a reassigned parameter's carriers;
  `CallStackArgCollect` handles big-endian right-justified slots, partial
  overwrites and listed allocators; incoming-argument detection no longer
  takes a reload after `h(&a)` as the argument.
- `LoadForward` under `escape_analysis` no longer skips an opaque store.
- `hlt` falls through and clobbers memory (`halt` `call_other`); volatile
  system registers and timers read a fresh value each time
  (`read_volatile`).
- x86 `swapgs` and `wr{fs,gs}base` update the segment base a later `fs:` /
  `gs:` access reads; `aapcs64` keeps `sp_el0` across a call.
- Address offsets compare on the address ring.
- A jump-table target found in its other ISA mode as well no longer counts
  as a narrowing, which could withdraw the site.
- N constant `if (flag)` guards over one local (gcc -O0) settle in a bounded
  number of optimizer iterations, not N.

Lifting and decoding (most in the vendored Sleigh):

- Float-to-integer and narrowing conversions round by the dynamic rounding
  mode and saturate as the ISA does on x86 (SSE, VEX, x87), ARM, AArch64,
  PowerPC and MIPS; `fcvt*`, `vcvt`, `fctid*`, `fcfidu`, `friz`, `frsp`,
  `fist*` and `frndint` were wrong.
- x86: `fprem`, `fxam`, `fcom`, `rcl` / `rcr` OF, `lock hlt`, 66/F2/F3
  conversion prefixes, `0x67` memory and string operands, `rep`, `lea`, and
  the upstream Ghidra b16f1556d2 fixes; VEX.256 FMA writes all 256 bits;
  `rdtscp` writes EDX:EAX and ECX; `0x90` under a REX prefix without B is a
  `nop`; `pushf` and `syscall`'s R11 set reserved RFLAGS bit 1.
- PowerPC: `fmadd` and kin round once and give the ISA's NaN; the scalar,
  VSX and record forms set the FPSCR status bits (FPRF, FR, FI, FX, OX, UX,
  ZX, XX, VX*); `vcmp*.` and `bcd*.` set CR6; `mtfsfi`, `lxsd`, `stxvw4x`,
  `fmrgew`, `stfs`, `lfs`, `fmsub`, `xscvdpspn` (truncates), `slbmfee`,
  `xscmp*`, `paste.`, `isel` (ppc32), VSX double FMAs and `divwe[u]` fixed;
  `divweo` / `divweuo` set OV by the 32-bit quotient; `mfpmr` / `mtpmr`
  lift; `xsdivqp` has its own `call_other`.
- MIPS: `mtc0` clobbers CP0; `syscall` clobbers `at`, `hi` and `lo`; MIPS64
  `ins` / `ext` / `wsbh` sign-extend; fixed-mode float-to-integer
  conversions lift to fewer p-code ops.
- AArch64: `fmov` to an h register, `bfcvt*`, `fcvtxn`, `ldpsw` (second
  word), `stlrb` / `stlrh` and `ldar` W (access width), LSE atomics and
  `ldlar` into W (upper half), byte and half `ld{s,u}{max,min}` (compare
  width); by-element `sdot`, `udot`, `usdot`, `sudot`, `bfdot`, `fcmla` and
  `sm3tt*` lift; 64-bit `bfdot`, `sudot` and `usdot` zero bytes 8-15.
- ARM: `vrintr`, `vld3/4`, `vst3/4`, BE8/BE32 `vld1` / `vst1`, `msr` flags,
  NEON `vcvt` f16 vectors, modified immediates with imm8 0, `sdiv` / `udiv`
  by 0, `rfeia` / `rfeib` (PC from the lower word); BE8 decodes with its own
  `ARM8_be8` sla.
- The fixes Ghidra master made after the fork's base, listed in
  `externals/rsleigh/sleigh/FORK.md`.
- ppc64 ELFv1 and ppc32 SysV conventions return floats in `f1`-`f4`.
- CFG decoding of a run of `nop`s is deterministic. A call followed by a
  `nop` and decodable non-code (Thumb `bl __stack_chk_fail; nop` before a
  literal pool) ends `NoReturn` where it failed the build.
- `Lifter.pcode_at` honours the entry's ISA bit (odd is Thumb), and keeps the
  Thumb mode after many builds on one handle.
- A branch into the middle of an instruction with internal p-code branches
  (`lock cmpxchg`, `rep movs`) edges to its first op, not its last p-code
  slice, and is still `"interior_target"`; glibc's `je 0f; lock; 0:`
  re-entry is not reported whichever edge decodes first.

Loading:

- Relocations: a linked image is relocated from its dynamic tags as `ld.so`
  reads them; no image fails with "patch budget"; overlapping or ambiguous
  fields are holes; mips64el `SHT_REL`, `R_X86_64_32`, `R_PPC64_REL32`,
  `R_386_GOT32`, `R_AARCH64_GOT_LD_PREL19` with an addend, and `STN_UNDEF`
  relocations are handled.
- `relocated_got_is_rom=True` leaves unmapped what only the loader computes
  (IFUNC slots, MIPS global GOT entries, lazy-binding header words, TLS slots,
  `R_*_COPY` objects) and serves PPC32 `.got2`.
- `add_elf` refuses an image of another architecture, class, byte order or
  `EF_ARM_BE8` (`StriderError`), and a microMIPS image (`ValueError`).
- `load_elf` refuses a microMIPS image unless given both `arch=` and `cc=`,
  and a COFF or Mach-O file; a malformed ELF raises `StriderError`.
- An uncomputed PowerPC half16 relocation holes only its halfword, and an
  uncomputed 8-byte field (MIPS64 `R_MIPS_64` composite, x86-64 `GOTPLT64`
  / `SIZE64`, ppc64 doubleword64 and prefixed fields) all eight bytes.
- A symbol in an empty allocated `ET_REL` section sits at the section's
  aligned base, where it read 0.
- The PowerPC TLS markers (`R_PPC64_TLSGD` / `TLSLD`, ppc32 `R_PPC_TLS` /
  `TLSGD` / `TLSLD`) leave the instruction they mark served, where they
  holed it.
- `add_symbol_file` of a ppc64 ELFv1 debug file names the code its `.opd`
  descriptors point at, read from the loaded image.
- A ppc64 ELFv1 function's `Symbol.size` is its `st_size`; ppc64 ELFv2
  `no_return_targets` and `per_address_ccs` keyed by symbol also cover the
  local entry.
- `load_elf` / `add_elf` parse an image once.


## 0.2.0

Both the Python and the Rust surfaces changed; the two are listed separately.
[docs/python-api.md](docs/python-api.md) has the shape each API settled into.

### Breaking, Python

Loading and symbols:

- The symbol accessors return a `Symbol` record (`name`, `address`, `size`,
  `end`, `is_function`, `region`, `is_thumb`); `symbol_size` is gone.
  `functions()` yields `Symbol`s, one per address and function symbols only,
  keeping the alias whose size the ELF records. A Thumb function's `address`
  keeps its ISA bit; `end`, `region` and `symbol_at` measure from
  `address & ~1`. A name resolves to a definition, in load order, before an
  undefined symbol that carries an address.
- `add_elf` applies relocations by default (`apply_relocations=True`), as
  `load_elf` does. The flag also selects what is mapped: `True` every
  allocatable section, `False` code and read-only data only.
- An `ET_REL` object's sections are laid out from a synthetic image base, not
  address 0, and sections that shared an address are rebased apart, so every
  symbol address in a `.o` moves and address 0 is unmapped.
- Images are memory-mapped. A file changed on disk while loaded raises;
  `STRIDER_NO_MMAP=1` reads the image instead.
- `wide_const_bytes()` returns `bytes`, not `list[int]`.
- `load_elf` refuses an ELFCLASS32 MIPS image whose ABI is n32, EABI32, EABI64
  or O64, none of which has a preset; it assumed O32.
- `CfgOptions.known_targets` reads back target lists as tuples; editing one
  of those lists was a silent no-op.
- `LifterOptions.per_address_ccs` reads back as the same read-only
  `mappingproxy` on every access, as the `CfgOptions` tables do.

Analysis:

- The three unchecked memory claims move off `LifterOptions` into
  `LifterOptions(assumptions=AssumptionOptions(...))`, and
  `strider.lift.AliasMode` is gone; `LifterOptions(alias_mode=)`,
  `calls_clobber=` and `assume_distinct_sp_bases_disjoint=` raise `TypeError`.
  `alias_mode` is `stack_global_disjoint` (default `True`), `calls_clobber` is
  `assume_incoming_args_survive_calls` (inverted, default `True`) and
  `assume_distinct_sp_bases_disjoint` is `distinct_sp_bases_disjoint`, beside
  the new `callee_preserves_stack_args`, `noalias_allocators`,
  `escape_analysis` and `return_slots_survive_untracked_writes` (default
  `True`, the claim return verification rests on).
  `AssumptionOptions.none()` clears all seven.
- `build_cfg` raises a `StriderError` naming the cause for an `entry` it
  cannot use, as `analyze` does, not a bare `OverflowError`.
- A `Lifter` decodes only on the thread that built it. `analyze`, `build_cfg`,
  `optimize`, `pcode_at`, `reg`, `reg_name`, `call_other_abi`,
  `user_op_names` or a renderer given `lifter=` raise `StriderError` on any
  other thread. The handle moves and drops anywhere; to work off-thread, build
  a second one over the same `arch` / `reader()` / `rom()` and call its
  `analyze(addr, cc)` (a plain handle: no symbols, no default convention).
- `CallingConvention.custom(sleigh, ...)` resolves register names against the
  `Sleigh` it is given, so using the result with a `Lifter` of another
  architecture raises.
- `cc.no_return()` passed as `analyze`'s main `cc` raises; it is only a
  `per_address_ccs` override.
- Building a `Cfg` raises when the architecture reports no user-op names.
- `Lifter.optimize(function)` raises for a function another handle lifted.
- `Function.rewrite` / `rewrite_all` raise where a replacement reads the value
  it replaces (`x = f(x)`).
- `Node.op()` returns the `ExtendOp` for `Extend`, not `None`.
- `Node` equality and hash include the graph generation, so a handle held
  across an `optimize` no longer equals a fresh one.
- `Node.const_uint` / `const_int` / `const_bool` are `uint` / `sint` /
  `boolean`.

Patterns:

- Pattern and template builders take the `int_` prefix: `add`, `sub`, `mul`,
  `div`, `sdiv`, `rem`, `srem`, `neg`, `shl`, `shr`, `sshr`, `popcount`,
  `lzcount`, `extend`, `truncate`, `sign_extend` and `zero_extend` become
  `int_add` ... `int_zero_extend`.
- The any-operator builders take the `any_` prefix: `int_bin_any` ->
  `any_int_binary`, `int_un_any` -> `any_int_unary`, `float_bin_any` ->
  `any_float_binary`, `float_un_any` -> `any_float_unary`, `bool_bin_any` ->
  `any_bool_binary`, `int_cmp_any` -> `any_int_cmp`, `float_cmp_any` ->
  `any_float_cmp`, `function_arg_any` -> `any_function_arg`.
- `any_int` / `any_float` / `any_bool` match any node with an output of that
  type, constant or not. `any_int_const` / `any_float_const` /
  `any_bool_const` and `bool_value` are gone: `int_const()` / `float_const()`
  / `bool_const()` take a `Capture` or no argument, and `any_bool` replaces
  `bool_value`. `I1` is an integer type, so `any_int` covers booleans.
- `int_const_any_of(values)` is gone: `int_const` takes a list.
- `signed_int_const` -> `int_const_any_width`, which also takes a list. It
  matches a constant when the value is a zero or sign extension of the
  constant's bits at its width, so a stored `0x34` no longer answers a search
  for `0x1234`. `strider.template.signed_int_const` is gone;
  `template.int_const` builds the same constant.
- A bare string is no longer a capture operand; use `Capture(name)`.
- `.cap(name)` is gone: `.capture()` takes a `Capture` or a name.
- `call(at=)`, `call().at()` and `.at_any()` -> `call().target()`, which also
  takes a list.
- `switch().address(p)` -> `.selector(p)`: `inputs[1]` is the dispatched value.
- `preceded_by` -> `ctrl` on `SwitchPat`, `RetPat`, `IndirectBranchPat` and
  `UnreachablePat`.
- `LoadPat.mem_in` / `StorePat.mem_in` -> `.mem`.
- `PhiPat.input` / `MemPhiPat.input` -> `.phi_input`, which indexes
  predecessors. `.input` is now the raw-slot method every builder has, so
  `.input(0, p)` reaches the phi token.
- `one_of` reports every arm that matches; `first_of` is the first-match form.
- `float_is_nan(p)` and `float_le(a, b)` require the operand each repeats to be
  the same value, so they match fewer shapes.
- `find_all(..., ignore_casts_mask=)` is gone: `ignore_casts` takes a bool or a
  `CastMask`.
- A pattern over 256 nodes, counting `if_else` branch patterns, or nested
  deeper than the thread's stack allows, raises `StriderError` instead of
  crashing the interpreter.
- The operand-index setters (`CallPat.arg`, `CallOtherPat.arg`,
  `RetPat.ret_val`, `PhiPat.phi_input`, `MemPhiPat.phi_input`, `.input()`)
  reject an index past 1,048,576.
- A `.when()` predicate on a root with no value output gets the real matched
  node as `Match.root`, not a fabricated `I1`.

Match readers:

- `Match[capture]` returns a `BoundCapture` carrying every reader (`.uint`,
  `.node`, `.op`, ... and their `_opt` forms), where v0.1.0 returned the value.
  `m[off] == 4` and `int(m[off])` still work, but `m[c] is None` is always
  false; ask `c in m`.
- `Match.op`, `.value_type`, `.vn`, `.node` and `.float_bits` raise when the
  capture is absent; the `None`-returning forms are `op_opt`, `node_opt`, ....
  `const_uint` / `const_int` / `const_bool` are `uint` / `sint` / `boolean`.
- `capture in match` raises once the function has been compacted, like every
  other capture accessor.

Rendering:

- `Function.to_dot(style=)` / `to_html(style=)` are gone: `pretty` takes a bool
  or a `DotStyle`. `Cfg` keeps `style=`.
- `Lifter.neighborhood_dot(function, center, ...)` is gone; use
  `Function.neighborhood_dot(center, ..., pretty=True)`.
- `Function.to_dot` / `to_html` quote every attribute value (`shape="box"`).
- `Function.validate()` appends each node's kind and lowest instruction
  address to its error (`[node12 Store(..) @ 0x401000]`).

### Breaking, Rust

- The `rsleigh` path dependency is the `externals/rsleigh` git submodule: clone
  with `--recursive`, or run `git submodule update --init --recursive`.
- The MSRV is 1.91.
- Building the wheel or the sdist needs maturin 1.10 or newer (1.8.2 through
  1.9.x silently keep only the first of several `license-files`).
- The fixture binaries are Git LFS objects; a fresh clone needs
  `git lfs install && git lfs pull`.
- Every pattern builder takes the Python name: the `int_` and `any_` renames
  above (`bit_not` -> `int_not`, `truncate` -> `int_truncate`, ...), `any` ->
  `anything`, `if_node` -> `if_else`, `CallPat::at_any(addrs)` ->
  `target(int_const(addrs))`, `SwitchPat::address` -> `selector`,
  `signed_int_const` -> `int_const_any_width`, `preceded_by` -> `ctrl`,
  `mem_in` -> `mem`, `PhiPat` / `MemPhiPat::input` -> `phi_input`. Builder
  types follow (`IntBinaryAny` -> `AnyIntBinary`, ...). `bool_value` and
  `int_const_any_of` are gone.
- `MatchPat`, `NodePredicate` and `PostMatchFn` carry `+ Send`, so a closure
  capturing an `Rc<Cell<_>>` no longer compiles. `PostMatchFn` takes
  `Option<ValueType>`.
- `TemplatePat` is implemented for `Captured<Var>` alone, so `.capture()` on a
  composite template is a compile error rather than a dropped operand.
- `MemPat` no longer requires `compile_mem`. `WithOutput`'s slot is an
  `Option<usize>`, `None` being `any_output()`.
- The `int`, `bool`, `int_binary_op`, `int_cmp_op` and `bool_binary_op`
  capture kinds of `int_const_with!` / `bool_const_with!` /
  `float_const_with!` are gone.
- `MatcherBuilder::constrain_input_widths` is gone; use
  `MatcherBuilder::set_value_width` on the one vertex that must carry it.
- `NodeKind::Switch(SwitchTableId)` and `NodeKind::Call { cc: Option<CcId> }`
  carry their case table and override convention. `SideTables::{switch_targets,
  set_call_cc, call_other_name, set_call_other_name}` moved to `Function`;
  `set_call_other_name` takes the `user_op_id`. `set_switch_targets` is gone:
  mint a table with `Function::add_switch_table(targets) -> SwitchTableId` and
  build `NodeKind::Switch(id)`.
- `NodeKind::SegmentOp`, `IRBuilderExt::build_segment_op` and the `Seg` / `Off`
  slot roles are gone; a `SegmentOp` or `Indirect` opcode fails the lift by
  name.
- `IndirectBranch` and `Switch` take an optional trailing input, the ISA mode
  their instruction commits (`build_switch_with_mode`,
  `IRViewer::switch_isa_mode`). `Unreachable` takes an optional memory input.
  `build_switch` returns the `NodeId` it created.
- `ValueType` gains `I24`, `I40`, `I56`, `I72`, `I96`, `I112`, `F16` and `F128`.
- `ValidationError` (not `non_exhaustive`) gains thirteen variants:
  `ArithmeticWidthMismatch`, `DanglingCcId`, `DanglingInitialVnId`,
  `DanglingSwitchTableId`, `DataCycle`, `FloatConstUnrepresentableType`,
  `FloatConstWidthMismatch`, `InitialVarTypeMismatch`, `InputNotAvailable`,
  `LostStore`, `MemoryRewind`, `NoTerminatorReachable` and
  `UnmergedMemoryEffect`. `FunctionBuilder::build` fails with a
  `ValidationReport` whose `errors` field holds them.
- `strider_ir::dominates`, `control_edge_dominators` and `control_dominators`
  are gone, and with them strider-ir's production dependency on petgraph:
  `control_dominator_tree` / `control_edge_dominator_tree` return a
  `DominatorTree` with an O(1) `dominates`.
- `strider_ir` renames the stack side tables: `SpDecomp` -> `MemDecomp`,
  `StackId` -> `MemoryId`, `stack_slot` -> `memory_class`,
  `stack_slot_resolved` -> `memory_slot_resolved`, `set_stack_slot_not` ->
  `set_not_memory`, `clear_stack_slots` -> `clear_memory_slots`.
- `Graph::retain_reachable` is `retain_reachable_stale_cache`, and
  `Graph::update_input` panics on a `UseId` its node no longer holds.
  `Graph::remove_node_input` needs the `test-injectors` feature and
  `FunctionBuilder::build_branch` the `test-util` feature.
- `FunctionBuilder::build_call_other_abi` is gone with no public replacement.
- Removed: `Cfg::raw_neighborhood_dot`, `Cfg::region_id_at_start`, the
  `ConstValue` re-export from `strider-ir`, `strider_ir::graph::InputCursor`
  (`strider_graph::InputCursor` remains), `dot::Result`,
  `PostOrder::into_visited`, `MemRegion::fully_covers`, `StackArgs::index_of`,
  `graph_algorithms::walk::entity_preorder` (call `PreOrder::new`),
  `graph_algorithms::walk::VisitTracker` and
  `graph_algorithms::dominance::DefSites`.
- `PreOrder` / `PostOrder` take one type parameter, the graph.
  `PreOrderContext` / `PostOrderContext` and `DenseEntitySet::with_capacity`
  are private. `phi_placement` takes the `HashMap` directly.
  `dominance_frontiers` takes the root, and `DomTree::nodes` must yield each
  node once. `Worklist<E>` requires `E: EntityRef`.
- `dot::DotEmitter::node` / `edge` escape and quote every `extra` value: pass
  `#ffcc00`, not `"\"#ffcc00\""`. `DotStyle`'s fields and
  `DEFAULT_SFDP_NODE_THRESHOLD` are private; build a style with
  `DotStyle::dark()`, `dark_cfg()` or `empty()`.
- `dot::DotEmitter::node` doubles every backslash in a label, so a `\l` or
  `\n` renders literally; `node_raw_label` keeps line breaks.
  `dot::GraphDot::as_dot_with_state` is gone (`as_dot` remains).
- `CfgOptions` gains public `call_other_overrides` and `data_ranges` fields,
  so a struct literal needs `..Default::default()`.
- `strider_cfg::CfgOptions<'a>` and `strider_lift::LiftOptions<'a>` carry a
  lifetime: `CfgOptions::known_targets` is a `&'a FxHashMap<PcodeInsnAddr,
  ResolvedTargets>` and `LiftOptions::per_address_ccs` a `&'a FxHashMap<u64,
  BuiltCallingConvention>`, both taken by value before; `Default` borrows a
  shared empty table. `strider_cfg::Builder::for_arch` takes
  `&'a CfgOptions<'a>` and `with_per_address_ccs` a `&'a FxHashMap<..>`;
  forward `LiftOptions::per_address_ccs` to `Lifter::build_cfg` as it stands.
- `strider_cfg::ResolvedTargets` carries `ResolvedTarget { addr, isa_bit }`, so
  a `known_targets` map needs `ResolvedTarget::from(addr)`.
- `AnalyzeResult` gains `unverified_seeded_sites`, `interior_branch_targets`,
  `isa_mode_conflicts`, `unmapped_branch_targets` and
  `undecodable_branch_targets`.
- `AliasMode`, `MemAliasOptions` and `OptOptions::arg_alias` are gone.
  `OptOptions` is `{ resolve_indirect_branches, assumptions }`, the claims as
  under Breaking, Python; `AssumptionOptions::default()` keeps three on and
  `::none()` clears all seven.
- `strider_opt::apply_rules_in_order` is gone. `LoadForward` is no longer a
  unit struct: use `LoadForward::default()`.
- `PostOptimizer` has an `Any` supertrait, so an implementor must be
  `'static`.
- `strider_opt::value_range::compute_value_ranges` takes a
  `&DominatorTree<NodeId>` (from `control_dominator_tree`), not petgraph's
  `Dominators`.
- `BuiltCallingConvention::try_new` -> `validate(&self)`;
  `BuiltCallingConventionParts` is gone. `validate` compares argument and
  callee-saved registers by byte range, so a callee-saved `d8` against an
  argument `q8` is rejected.
- `CallingConvention::x86_64_all_preserving` is gone on both surfaces; use
  `CallingConvention::x86_64_systemv().preserves_all()`.
- The userland PowerPC presets name `v2`-`v13` for vector arguments, so a
  `call().arg(n)` query reads a different operand on both surfaces: a
  collected stack argument's first position is `8+25` on PPC64 and `8+20` on
  PPC32, where it was `8+13` and `8+8`. The three `*_linux_kernel` flavours
  name no vector register, so nothing moves there.
- `SleighArch::probe_regs` needs the `test-util` feature.
- The ARM processor-mode `CallOther` rows apply to the ARM32 presets only, and
  a processor-mode switch (`setUserMode`, `setFIQMode`, ...) clobbers
  `r8`-`r12` as well as `sp` and `lr`. `setStackMode` is its own row and writes
  `sp` alone; `setThreadMode` stays unclassified.
- `OwnedElf::file` is gone; use `checked_file`.
  `elf_get_loadable_regions_including_writable`, `elf_load_with_relocations`,
  `elf_load_readonly_with_relocations`, the two sections-only region loaders,
  `apply_elf_relocations` and `apply_elf_relocations_autoload` are gone; use
  `OwnedElf::regions(source, filter, relocate)`. `MemRegion::data` /
  `data_mut` are gone; a region serves bytes through `read`.
- `ElfFileMemReader::from_bytes` and `::from_path` are gone, and
  `elf::relocations` is private. `::from_object(&obj)` is the one
  constructor and copies each mapping; a zero-copy view of a mapped ELF is an
  `OwnedElf::regions` load behind a `MemRegionsLookupTable`.
- `OwnedElf::function_entry` is gone; build an `OpdTable` over a
  `checked_file`.
- `elf::LoadFilter` gains `ImmutableAndRelocatedGot`; `OwnedElf::regions`
  errors for it with `relocate=false`.
- `BuiltCallingConvention` gains `entry_address_vn`, the register holding the
  function's own entry address on entry (PPC64 ELFv2 `r12`, MIPS abicalls
  `t9`), which the lift binds.
- `BuiltCallingConvention::ret_and_clobber_vns` reports REGISTER clobbers only,
  so a `Call` no longer carries an output per tracked Sleigh temporary.
- `strider_cfg`'s region types carry resolved targets and extents, all fields
  public and none `non_exhaustive`: `RegionTerminator::TailCall`'s and
  `Switch`'s `target`/`targets` are `ResolvedTarget`s, `Switch` gains `addr`,
  `Region` gains `empty_span_len` and `RegionInstruction` gains `len`.
- `OptimizerPipeline::add` and `add_post_pass` require `Send`, and `passes()` /
  `post_passes()` hand back `Box<dyn ... + Send>`.
- `LiftOutcome` gains `switch_anchors` and `return_sites`.
- `entity_utils::UnionDag`'s value parameter is `V: Copy + Eq + Hash`.
- `strider_pattern::IndirectBranchPat::isa_mode` is gone; the slot is
  `.input(3, p)`.
- `ArchPreset::arch` needs the `test-util` feature; call the `SleighArch`
  constructor instead.
- `Lifter::arch` is gone; `Strider::arch` remains.

### Added, Python

- `load_elf(relocated_got_is_rom=True)` / `add_elf(relocated_got_is_rom=True)`
  serve `.got` and `.data.rel.ro` as read-only memory, so a load through a GOT
  slot folds to the address it holds. Off by default (a runtime claim the
  image cannot prove); `.got.plt` is never served; refused with
  `apply_relocations=False`.
- `LifterOptions(assumptions=AssumptionOptions(...))` tunes memory precision;
  [docs/python-api.md](docs/python-api.md#2-analyzing-a-function) says what
  each claim buys. `AssumptionOptions.none()` makes none of them.
- `cfg.is_complete()` and six incompleteness channels: `unresolved`,
  `cfg.unverified_seeded_sites()` (sites settled by `known_targets`, consumed
  as a tail call, a seeded return, or an arm on an unevaluated mode),
  `cfg.isa_mode_conflicts()` (reached in two ISA modes),
  `cfg.interior_branch_targets()` (off every instruction boundary),
  `cfg.unmapped_branch_targets()` (no byte backs it) and
  `cfg.undecodable_branch_targets()` (no instruction, reached by a branch or a
  fall-through past a call).
- A `Return` whose target does not fold to the entry return address
  (`push x; ret`, `mov lr, r1; bx lr`, an overwritten saved link) is reported
  in `unresolved`.
- `CfgOptions(data_ranges=[(start, end), ...])` marks bytes the CFG builder
  never decodes; `ElfLifter.analyze` fills it from ARM and AArch64 `$d`
  mapping symbols unless the caller sets it.
- Indirect branches resolve with a per-target ISA mode, so ARM/Thumb and MIPS16
  targets decode in the right mode. `CfgOptions(known_targets=...)` seats your
  own answers; `LifterOptions(resolve_indirect_branches=False)` turns the
  classifier off.
- Object files (`ET_REL`) analyse: code relocations are applied for x86,
  x86-64, AArch64, ARM and Thumb, PowerPC and MIPS; an undefined or common
  symbol gets its own address in an unmapped range past the image; a GOT
  reference loads from a synthetic slot holding the symbol's address. An
  uncomputed relocation is a hole outside writable mappings, and the decode
  error names its type.
- ppc64 ELFv1 function symbols and the entry point resolve through their `.opd`
  descriptors.
- `ElfLifter.add_symbol_file(path)` takes the symbols of a debug companion;
  `ElfLifter.add_symbols({name: addr | (addr, size)}, is_function=)` takes
  symbols from no ELF, such as a `System.map`. `symbol_at(address)`,
  `symbol_opt`, `iter_symbols()`, `endianness` and `is_arm_be8`.
- `Lifter.arch`, `Lifter.reader()` and `Lifter.rom()`, and `Cfg.lifter`.
- `BufferReader` accepts `bytes`, `bytearray` or a sequence of ints.
- `Lifter.optimize` takes `opts=` and uses the handle's `rom`.
- `cc.preserves_all()` / `cc.preserves_regs()` and an `arm_aapcs_soft`
  preset. A callee-cleanup convention (`ret $imm16`) or an i386 PC thunk needs
  its own `per_address_ccs` entry, built with
  `CallingConvention.custom(..., ret_stack_pop=4 + imm16)` or with the thunk's
  register left out of `callee_saved_regs`.
- `CallingConvention.powerpc_sysv32_linux_kernel()`,
  `powerpc64_elf_v1_linux_kernel()` and `powerpc64_elf_v2_linux_kernel()`,
  which name no vector register. `load_elf` picks one when `e_entry` is at or
  above `KERNELBASE` (`0xc000_0000` on ppc32, `0xc000_0000_0000_0000` on
  ppc64). x86's kernel ABI has no header bit and needs an explicit `cc=`.
- `CfgOptions(call_other_abis={name: CallOtherAbi})` reclassifies a Sleigh
  user-op: `CallOtherAbi.noop()` / `pure()` / `mem_clobber()` / `no_return()`,
  or `CallOtherAbi.custom(sleigh, implicit_reads=, implicit_writes=,
  clobbers_memory=, no_return=)`. `Lifter.user_op_names()` lists an
  architecture's user-ops; `Lifter.call_other_abi(name, opts=None)` reads back
  the classification in force.
- `function_arg_float(n)` and `call().arg(n)` reach float arguments, which
  follow the integer ones at a call site.
- A control cycle that never exits (`while (1)`, a spin loop) is anchored at
  lift time, so its stores and their operands stay in the graph.
- Patterns: `first_of`; `one_of` / `first_of` take any pattern as an arm and
  nest in a value, memory or control slot, and an empty list matches nothing.
  `field(base, offset=None)` and `FieldPat`; `code_ptr(x)` (`x` or `x & -2`);
  `PhiPat.input_from(edge, value)`; `IfPat.ctrl(p)`; `load().non_stack()`,
  `store().heap_only()`; `.ordered()` on every binary pattern;
  `find_unique_value`; `JoinPredicate` for relational constraints. A raw int
  stands in for `int_const`, and `int_const` accepts values up to 2^128 - 1.
- `.input(i, p)` / `.any_input(p)` and `.output(slot)` / `.any_output()` on
  every node builder with those slots, declared in the stubs as the
  `runtime_checkable` protocols `NodePat`, `InputPat`, `CtrlPat`, `MemPat`,
  `MemAccessPat`, `OrderedPat` and `OutputPat`. `pattern.CaptureKey` and the
  `IntBinaryOpName`, `BoolBinaryOpName` and `FloatBinaryOpName` aliases.
- `indirect_branch().target(p)` and `switch().selector(p)` take a list.
- `Function.to_text()`: the IR as canonical text, stable across runs: a header
  (endianness, tracked varnodes, default convention, argument carriers), then
  one line per node.
- `Function.validate()` also reports a memory read that skips a memory effect
  on its path, an `InitialVar` naming a varnode the function never minted or
  typed against it wrongly, and a `Store` only a `Load` takes (`LostStore`).
- `visualize` opens on the whole graph (`whole=False` for a neighborhood) and
  returns the bound port; `visualize(background=True)` serves on its own
  thread until `strider.explore.shutdown(port)`. The explorer pans by drag and
  arrow keys, zooms with ctrl+wheel or `+` / `-`, fits with `f` and resets with
  `0`; `0` means no limit on its depth, hub-cap and node-count knobs.
- `Cfg.to_dot(style=)`, matching `Cfg.to_html(style=)`, and `lifter=` on
  `Function.to_dot` / `neighborhood_dot` and `Cfg.to_dot` / `neighborhood_dot`.
- `StriderError.backtrace` carries the Rust backtrace; `STRIDER_BACKTRACE=1`
  also puts it in the message.
- `ElfLifter`, `load_elf`, `PatLike`, `ValueTy`, `DotStyle`, `OptimizerPass`
  and other names join their modules' `__all__`.

### Added, Rust

- `Function::to_text(fingerprints)`.
- `Cfg::isa_mode_conflicts` / `interior_branch_targets` /
  `unmapped_branch_targets` / `undecodable_branch_targets` /
  `undecodable_seeded_targets` / `link_register_seated` / `tail_call_seated` /
  `function_isa_bit` / `flowing_isa_bit_at_site` / `space_ids` /
  `switch_arm_regions`, and `AnalyzeResult::is_complete`.
- `CfgOptions::data_ranges` (`DataRanges`) and
  `strider_reader::elf::mapping_symbol_data_ranges`.
- `strider_cfg::Builder::with_user_op_names` and `with_transient_defaults`;
  `SleighArch::has_delay_slots` and `internal_registers`;
  `FunctionBuilder::set_internal_registers`.
- `LiftOutcome::return_sites` and `strider_opt::ReturnTargets`.
- `OptimizerPipeline::has_post_pass`.
- `ElfSectionLayout::extern_address`, `OwnedElf::writable_ranges`,
  `elf::without_ranges`, `OwnedElf::checked_file`.
- `elf::LoadFilter::ImmutableAndRelocatedGot`, the immutable image plus `.got`
  and `.data.rel.ro`.
- `graph_algorithms::dominance::dominators` (Lengauer-Tarjan), and
  `strider_ir::DominatorTree` with its `span`.
- `NodeKind::input_head_len`, `expected_output_kind` and `is_terminator`.
- `ArchPreset::ALL`.
- `BuiltCallingConvention::float_arg_slots`.
- `strider_ir::node::low_bits_mask_u128`.
- `strider_cfg::{FlowContext, FlowVars, UndecodableTarget}` and
  `Builder::with_flow_context`.
- `strider_reader::elf::{RegionSource, LoadFilter, ElfSectionLayout, OpdTable}`.
- `ValueType::ALL`, `Strider::{user_op_names, rom, arch}`,
  `dot::DotEmitter::node_raw_label` and `strider_opt::apply_rules_count_with`.
- `SleighArch::callother_output_is_exact_in`: whether a `CallOther` output
  narrower than its tracked container leaves the rest of it alone (true on
  x86 and x86-64 outside the x87 register file).
- `[profile.release]` sets `overflow-checks = true`: the shipped wheel traps on
  arithmetic overflow.

### Changed

- Asm fingerprints no longer include instructions whose only contribution was
  dead code, such as a flag computation a rewrite discarded.
- `ConstantFold` factors a sum of scaled copies of one value into one multiply
  for every mul / shift pairing: `x*3 + (x << 2)`, `(x << 2) + (x << 3)` and
  `x*3 - x` all reach a query as `x * C`.
- `PhiCollapse` folds a cycle of phis whose inputs from outside the cycle are
  one value.
- The optimizer skips a pass whose last run changed nothing until the graph
  changes.
- `validate` rejects IR it used to accept: a non-phi node that is its own
  transitive producer; the memory, `InitialVar` and `LostStore` checks above; an
  integer operation mixing operand widths; a `CcId` or `SwitchTableId` no
  interner holds; an input not available at its use; a reachable graph with no
  terminator; and a `Region` that drops an arm's memory effect
  (`UnmergedMemoryEffect`, not `MemoryRewind`).

### Performance

- A `Call` carries an output per clobbered REGISTER rather than per tracked
  Sleigh varnode, so lifted graphs are much smaller and `build_ir` and
  `analyze` faster.
- A Sleigh temporary gets a phi only where it is live across a region
  boundary, not at almost every join; the optimised text is unchanged.
- A `per_address_ccs` table costs what the function calls, not its size: the
  lift seeds only the overrides this CFG's call targets select. From Python
  each `analyze` still resolves every entry's ABI.
- A pattern query no longer doubles per nested failing branch pattern, and a
  commutative node whose two inputs are one value tries one operand order.
- `find_all` attempts a root once per distinct anchor, stops at the first
  operand ordering of a pattern with no visible capture, and no longer runs a
  chain join over the whole cartesian product.
- `LoadForward`, `FunctionArgDetect` and `CallStackArgCollect` answer
  nearest-clobber queries by climbing a memory dominator tree instead of
  walking the memory graph per query; on an irreducible memory graph the walk
  memoises start-independent answers.
- Dominators come from Lengauer-Tarjan rather than petgraph's quadratic
  iterative scheme; join `dominates` constraints, `phi_input_from_edge` and
  value-range guard lookups answer from dominator-tree intervals.
- `Function::validate` is linear, and so are draining a `Worklist` and
  dead-branch elimination over chained constant branches.
- Region lookup is O(log n) for full and partial reads.
- A run of `$d` data mapping symbols is one range, not k nested ranges built
  in O(k^2).
- One def-use walk order serves every peephole pass of a pipeline iteration.
- Compaction sizes its arenas off the survivors, and a finished CFG region
  drops its instruction vector's push slack.
- Resurrecting a value costs its own cone, not the node arena, and a phi's
  incoming edge is an indexed read of its `Region`'s inputs.
- Loading maps the image and patches relocations on read, and a symbol resolves
  through an index, so opening a kernel no longer walks every relocation and
  symbol; `symbol_at` is one index query.
- `add_elf` checks overlap in O(n log n).
- `BufferReader` copies `bytes` and `bytearray` in one piece.
- `pcode_at` decodes through one cached engine instead of cloning `Sleigh` per
  call.
- Reading `CfgOptions.known_targets` or `.call_other_abis` returns a cached
  read-only `mappingproxy` instead of copying the table, and
  `LifterOptions.per_address_ccs` is served the same way.
- A seeded `known_targets` table costs an `analyze` nothing: `CfgOptions`
  converts it once and lends it per call.
- `pcode_at` keeps what each sweep decoded, so a walk over a function's
  instructions decodes it once.
- A CFG build reuses the `Lifter`'s Sleigh user-op table instead of fetching it
  over FFI, and `Function.to_dot(pretty=True)` resolves the register table once
  per render.
- `UnionDag::for_each` costs the size of the answer, not of the arena, so
  reading `asm_fingerprint` over a large function is linear.

### Fixed

Lifting and memory:

- PowerPC `mtfsf` / `mtfsfi` (the Power ISA 2.06 `L=1` / `W=1` forms, on the
  ppc64 presets) and `wrtee` / `wrteei` declare the FPSCR and MSR registers
  they write. The x86 `XSAVE` / `XRSTOR` footprint gained the opmask, high
  ZMM, MPX and CET-supervisor state it moves.
- A p-code `LOAD` / `STORE` in the REGISTER space reads or writes the register
  its folded address names, with a phi placed for it (ARM `vld1.N {dX[i]}`).
  One that does not fold (`VLD2/3/4`, `VST2/3/4`) is an opaque access that
  mirrors every tracked register into its slot before and re-reads each
  after.
- A `LOAD` from the constant space lifts as the constant its address encodes,
  so PowerPC `rlwimi` / `rldimi` / `rldic` / `rldcr` masks fold.
- `CBRANCH` lowers as `cond != 0`, not the condition's low bit.
- A shift count at or past the output width gives 0 (the sign fill for an
  arithmetic shift right), in the lift (x86 `psrad xmm, xmm`) and in
  `ConstantFold`.
- `popcount` / `lzcount` count over the operand width (AArch64 and PPC64
  `clz` on a 32-bit operand).
- Sign-extending a constant into `I256` / `I512` fills the upper half.
- A write into a tracked container wider than 16 bytes lifts; a p-code
  temporary of a width the IR did not type (x86 `adcx` / `adox` / `lsl`,
  AArch64 fixed-point `ucvtf`) and a CONST branch-target varnode of width other
  than 1, 2, 4 or 8 no longer fail the function.
- A `Copy` whose input and output widths disagree fails the lift instead of
  truncating or zero-extending silently.
- Sub-register reads and writes on `arm_be_kernel` use the register file's
  little-endian layout, not the image's data endianness, and `load_elf` picks
  `arm_be_kernel` for an ARM BE8 image.
- A call through an x86 trap vector (`int imm8`, `into`) pops no return
  address; a call through any other `CallOther` result
  (`rdrand rax; call rax`) keeps its pop.
- A `CallOther` output narrower than its tracked container writes that slice
  alone on x86 and x86-64, as the SDM specifies
  (`mov eax,0x12345678; in ax,dx; shr eax,16; ret` returns `0x1234`). Other
  architectures, and an x86 MMX write to its 10-byte x87 `ST`, keep the
  whole-container clobber.
- A calling convention names the register holding the function's own entry
  address (PPC64 ELFv2 `r12`, MIPS abicalls `t9`) and the lift binds it, so
  TOC-relative addressing folds. ELFv1 names a descriptor word rather than a
  register and keeps none.
- A `CallOther` whose output is a memory operand (x86 `sgdt [mem]`) lifts.
- Unclassified user-ops that failed the lift are classified: AArch64 `svc`,
  ARM32 NEON float user-ops, `cpsid a` and its siblings, MIPS `syscall`. ARM32
  `smc` / `hvc` carry the SMCCC register footprint; `software_udf` does not
  return.
- A user-op is opaque to the memory walk whatever the function's convention
  says, so no store forwards across a `syscall`.
- A PowerPC `tw` / `twi` / `td` / `tdi` whose mask covers a whole signed or
  unsigned trichotomy ends its region as no-return; a MIPS `BUG()` trap ends
  its function's decode.
- A MIPS o32 `double` return is the 8-byte `f0_1` pair, not its low half.
- PowerPC `cr2`-`cr4`, `r2`, `r13` and `v20`-`v31` are callee-saved, and the
  PPC64 stack-argument base is 112 bytes on ELFv1 and 96 on ELFv2.
- A callee-saved `d8`-`d15` preserves 64 bits, not the whole `q` register.
- The userland PowerPC conventions pass vector arguments in `v2`-`v13` and
  return a vector in `v2` (ELFv2 a homogeneous vector aggregate in `v2`-`v9`).
  An ELFv2 `__float128` or IEEE `long double` return comes back in `v2` too.
  The three `*_linux_kernel` flavours name no vector register (the kernel is
  built `-mno-altivec -mno-vsx`).
- `BuiltCallingConvention::validate` rejects a stack-pointer sub-register in a
  register list; `ESP` beside `RSP` made the stack pointer `arg 0`.
- A `per_address_ccs` override naming an integer argument register outside the
  function's own convention lifts.
- An ARM32 function with a NEON register and a call analyses.
- PowerPC `frin` folds half away from zero, as p-code's `FLOAT_ROUND` is
  defined.
- A float value range is top, not the singleton `{0}`.
- A `FloatConst` is masked to its width on construction and instantiation, and
  `validate` rejects stray upper bits.
- An SSA read of a never-written variable errors instead of returning `Entry`'s
  control edge.

Optimizer and analysis:

- IR validation runs its dataflow checks even when a node is missing an asm
  fingerprint, so an error bundle can carry both.
- A value tested for non-zero is no longer read as bit 0 of itself.
- Memory SSA no longer memoises a path-relative cycle verdict.
- `LoadForward` and `FunctionArgDetect` rewire load memory edges only on
  structural facts, so re-optimising under `AssumptionOptions.none()` inherits
  no assumption. `LoadForward` reports a change when it narrows without
  forwarding.
- An alignment mask anchors a stack base only when its 1-run reaches the top of
  the address width; `sp & 0x10` or `sp & 0x8000_0000` no longer roots one.
- A call's collected stack arguments stop at a slot a nearer store partly
  overwrote.
- `CallStackArgCollect` and `FunctionArgDetect` replace what an earlier run
  wrote, so re-optimising no longer grows each call's argument list.
- `PhiCollapse` reports the node it killed.
- The optimizer's iteration cap drains and validates before reporting.
- Constant-folding a branch no longer strands a loop with no path to a
  terminator.
- `Function.rewrite` / `rewrite_all` refill the memory-decomposition side
  table, so `load().stack_only()` answers correctly after a rewrite.
- `Lifter::build_ir` refuses a `Cfg` another `Lifter` built.
- `rewrite_all` and `apply_rules_count` stop at the first rule that fires at a
  node.
- Under `escape_analysis`, a frame address held in any register a call
  clobbers escapes the frame (i386 `regparm`, clang fastcc, the GNU C static
  chain, AArch64 `x8`), so a spill is no longer forwarded across a callee that
  writes it. Sleigh's internal registers (`SleighArch::internal_registers`)
  are exempt.
- `PhiCollapse` and `RegionCollapse` skip a region unreachable from the entry
  whose only predecessor is its own `If`.
- A known-bits fold attributes its folded cone to the node it builds.
- `range_of` resolves a chain of phis under a depth budget rather than giving
  up at the first one.
- Return verification locates a RAM store's address as it does a load's, so
  a write through a frame pointer reloaded off the stack is checked against
  the saved slot. A store at no locatable stack address, and any callee or
  user-op, is covered by
  `AssumptionOptions::return_slots_survive_untracked_writes`, so a prologue's
  `push lr` through a merged SP no longer reports a genuine return.

Indirect branches and the CFG:

- A conditional's arms are told apart by where its successors start, so an
  `fn_max_size` bound cutting an instruction cannot swap them.
- A guard on a sub-register bounds the index scaled out of it, and a guard
  bounds a table index through a mask of bits known to be zero (PowerPC
  `slwi` / `sldi`), so tables stop seating slots past their end.
- A table index under a constant offset that wraps below zero defers the site
  instead of seating dead slots.
- A masked switch index bounded on a loop back edge resolves on x86-64.
- A union of two single-element ranges keeps their gap as its stride.
- A jump-table entry is no longer enumerated as the table index.
- A site that loses a successor, including a `LinkRegister` answer merged with
  concrete targets, is reported in `unresolved`, and a resolved target that is
  not code is abandoned and reported rather than failing `analyze`.
- A direct branch to an address with no bytes, or to bytes that hold no
  instruction, costs the edge, not the function; so does a fall-through past a
  call into such bytes, and bytes that do not decode behind a jump-table arm
  drop that arm.
- A narrowing mask on a table cell is not read as the index (`(tbl[i] & m) +
  base` seated one arm per masked value): the index is the bounded value below
  the cell's load; a cell the evaluator cannot fold keeps the masked value.
- A switch over a negative case base (a guard on `index + k` that
  `ConstantFold` merges into the table base) takes its bound from that guard.
- A single-register pop into the PC (`ldr pc,[sp],#4`) with a call between
  push and pop is classified as a return rather than left unresolved, told
  apart from a `push X; pop pc` tail call and re-checked against each round's
  IR.
- A return the caller seeded is reported on `unverified_seeded_sites` alone,
  not also as an unverified return.
- A `Return` the classifier seated is not reported once
  `return_slots_survive_untracked_writes` carries its proof, so an ARM
  `push {lr}; bl; pop {pc}` comes out complete. A caller-seeded `Return` whose
  re-check fails is reported on `unverified_seeded_sites`, not as an
  unresolved dispatch.
- A region starting mid-machine-instruction keeps the decode context those
  bytes were lifted in (`gcc -march=armv4t`'s `movne lr,pc; bxne r3`). A
  lock-prefix re-entry is exact only where the region starts at that
  instruction.
- A seeded arm that fails to decode reports the undecodable bytes on
  `undecodable_branch_targets`, not the arm itself. An arm reaching such bytes
  only through another table keeps its edge.
- A run of zero-p-code-op instructions between a call and bytes that hold no
  instruction ends that call as no-return, and a branch into such a run keeps
  its successor.
- `unmapped_branch_targets` and `undecodable_branch_targets` are sorted and
  deduplicated.
- A decode that runs off the end of the image mid-instruction ends the region.
- A branch into a region's last instruction, and a region split, keep every
  edge.
- `Cfg.region_at` finds an address inside a region's last instruction.
- A reused engine decodes a function the same as a fresh one: ARM's
  `mov lr, pc` state and MIPS's `PAIR_INSTRUCTION_FLAG` and MIPS16e
  `ext_delay` no longer carry into the next decode.
- A reused engine also re-derives the ARM context an instruction commits at a
  later address (IT-block `condit`, ThumbEE `TEEMode`) from this build's own
  decode. An instruction with no p-code ops (ARM `enterx`) records its own
  commit too.
- Two edges reaching one address with different IT state stay
  first-decode-wins and unreported (UNPREDICTABLE in ARM).
- `Cfg.isa_mode_conflicts()` is sorted and deduplicated.
- An `analyze` given a post-pass named `IndirectBranchClassify` still runs the
  real classifier.
- Three inputs that crashed `build_cfg` raise `StriderError`: a malformed
  instruction in a MIPS delay slot, an AArch64 operand with a null address
  space, and a region at the top of the address space.

Patterns:

- A commutative node with one pinned operand tries both orders, and a capture
  on an operand's sibling output keeps its binding.
- A match's identity is its visible bindings, so a commutative swap that moved
  two fanned-out sub-patterns between operands no longer reports one match
  twice (`bool_and(float_is_nan(_), float_is_nan(_))`).
- A `first_of` cut is scoped to the root attempt and keyed on the IR node the
  alternation was offered, so mirror spellings of a commutative query agree
  and an `any_input` slot offering another node no longer cuts the matching
  arm.
- `inputs_of_width` checks the matched node's own inputs, not an
  alternation's arms or the far side of an ignored cast.
- A refusal an unmatchable `if_else` branch pattern left behind is dropped with
  that branch, so rebuilding the query no longer errors.
- The node cap counts a pattern tree-unfolded, charging a shared vertex once
  per consumer, so a small pattern sharing a sub-pattern down a chain is
  refused instead of overflowing the stack. An alternation is charged its
  deepest arm, not the sum.
- An `if_else` branch sub-pattern backtracks into its later bindings, and its
  captures are visible to join constraints; replacing a branch drops the old
  branch's captures.
- An alternation used as a whole pattern binds its capture, a root-level
  `one_of` or bare `call()` matches a `Call` with no value output, and
  `one_of![p]` no longer binds control and memory edges as values.
- `Not` over `phi_input_from_edge` drops the rows the constraint cannot answer.
- A second `.capture()` on one vertex binds both names.
- A `Match` used after `Function.compact` reports stale instead of reading
  another node.
- `int_not` refuses an output wider than 128 bits.
- A guard in a `ctrl()` slot, a guard on a control-rooted pattern,
  `.ordered()` on an alternation, a multi-sink branch pattern and a `Template`
  with a gapped output slot are refused at build time.
- A template refuses an ill-typed operand and output kinds that contradict the
  node signature, and a comparison operand built fresh takes its operand's
  width, not `I1`.
- Deep patterns, `JoinConstraint` trees and `if_else` towers drop without
  recursing.
- An exception inside a `.when()` predicate surfaces from its own query, not a
  later one; a `.when()` predicate nested into another pattern no longer
  leaks a reference cycle or segfaults; an `__index__` that re-enters the
  builder raises.
- A query that raises before doing any work, or fails partway through a
  multi-pattern call, leaves its one-shot patterns usable.
- `Function.rewrite` with a float-typed root and an integer literal declines
  rather than raising `PanicException`.
- `Cfg.fingerprint_pcode` rejects a `Node` from another binary's function.

Loading:

- A PT_LOAD segment declaring more file than it maps (`p_filesz > p_memsz`)
  serves only the bytes it maps.
- The read-only view leaves out every address a writable mapping covers, in one
  image and across `add_elf`, so `LoadReadOnly` no longer folds a stale file
  byte.
- A relocation field straddling a region end is patched in every region
  covering it, and equal-start `PT_LOAD`s dedup widest-first.
- Object files: relocations resolve against `.symtab`; sections sharing an
  address, `SHF_TLS` sections and sections whose extent overflows the address
  space no longer serve each other's bytes; `functions()` / `symbols()` list an
  object's symbols, and a library's `.dynsym` exports.
- `R_MIPS_16` patches its four-byte storage unit, mips64el relocation kinds are
  read correctly, and a relocation against an out-of-range `st_shndx` is
  skipped.
- An image with many overlapping `PT_LOAD`s is refused past 4x relocation
  amplification instead of exhausting memory.
- A mapping ending at 2^64 loses its last byte instead of failing the load.
- `load_elf` no longer aborts on a guarded parse or hangs on a FIFO.
- An instruction relocation field is read and written in instruction byte
  order, little-endian on `aarch64_be`; every A64 field was byte-reversed.
- An `SHT_REL` `R_*_RELATIVE` / `R_MIPS_REL32` with no symbol is served as
  assembled (image base 0), not as a hole, so a read-only pointer table reads
  on an image with text relocations.
- An undefined `STB_WEAK` symbol's synthetic address reaches only a call or
  jump displacement; its GOT slot is unmapped and an absolute field
  uncomputed, so `if (&w)` keeps its null path.
- A ppc64 ELFv1 `ET_REL` object's function symbols resolve through the
  relocations on their `.opd` entry words, so `analyze` by name no longer
  decodes descriptor bytes.
- x86-64 `R_X86_64_GOT64` and `R_X86_64_PLTOFF64` resolve, so
  `-mcmodel=large -fPIC` code addresses its globals rather than reading a hole.
- Walking the dynamic table depends on `.dynsym` being present rather than on
  the image being dynamically linked, and a Python `read` returns `None` inside
  an uncomputed relocation's field as well as off the end of a mapping.

Python and the explorer:

- A mapping argument (`known_targets`, `call_other_abis`, `per_address_ccs`,
  `add_symbols`) is snapshotted before conversion, so mutating it meanwhile
  (from an `__index__`) is safe. `per_address_ccs` accepts any
  mapping, so its read-only getter value round-trips.
- `analyze` releases the GIL on a custom pipeline too, and `OptimizerPipeline`
  is usable from any thread and survives being applied.
- A `Function` or `Cfg` moved to another thread no longer leaks its `Lifter`,
  and an off-thread render raises `StriderError` rather than
  `PanicException`.
- A `MemReader` callback that panics raises `StriderError`, and one that
  re-enters `analyze` no longer aborts the process.
- `Cfg.neighborhood_dot` raises on an out-of-range region id.
- `add_elf` and `add_symbol_file` take a `pathlib.Path`, as `load_elf` does.
- `field` and `code_ptr` cut to their specific arm, so a field no longer comes
  back twice under a permissive base, and a negative field offset reads back
  signed rather than as 2**64-8.
- An empty `CallOtherAbi.custom(...)` reprs as a `custom`, not `pure()`.
- `explore.shutdown` leaves a server it did not stop able to serve.
- The explorer enforces its `Host` check on every loopback bind (127.0.0.2, an
  IPv4-mapped loopback), not just the three canonical spellings, closing DNS
  rebinding there.
- The explorer's autocomplete and query evaluator offer the pattern builders
  plus `Capture`, not everything in `dir(strider.pattern)`. The search
  placeholder names `int_add`, not `add`.
- `build_cfg` reports only the `known_targets` seeds it seated, so a
  whole-binary answer table no longer makes every `is_complete()` False.
- `pcode_at` answers the same whatever the call order (an ARM backward `blx`
  pinned a decode mode).
- `add_elf` does nothing when the image is already loaded byte for byte, and
  refuses off the handle's thread before merging anything.
- A `custom(...)` `CallingConvention` or `CallOtherAbi` is checked against its
  register table, not the preset name, so one built from `arm` works with an
  `arm_thumb` handle.
- `Function.rewrite` / `rewrite_all` compile the replacement first, so an
  invalid one no longer consumes a one-shot `into_pat()` `find` pattern.
- Importing strider no longer leaves `RUST_LIB_BACKTRACE=1` in the C
  environment every child process inherits.
- A pattern that shares one `Pat` into both operands (`q = int_add(q, q)`) is
  refused while it is lowered, before building its exponentially many matcher
  closures.
- DOT, HTML and p-code text are deterministic, and a backslash in a symbol name
  is escaped.
- `strider.explore.visualize(depth=-1)` raises `ValueError`.
- The explorer no longer `eval`s its query string, refuses a request whose
  `Host` is not loopback, and a client that stalls, trickles or stops reading
  neither holds it nor blocks interpreter exit; `explore.shutdown` waits under a
  deadline and returns the ports it stopped.
- The vendored `svg-pan-zoom` bundle carries its BSD-2-Clause notice.

### Fixed, decoder and sla

`externals/rsleigh/sleigh/FORK.md`, "What the patches change", lists every
fix in force in the vendored Sleigh specs and parser, including those 0.3.0
adds. Outside the patches:

- A memory reader overstating its read is an error, a reader that panics is
  caught, and `setOutOfBandState` checks its operand index rather than reading
  uninitialised state. A region at the top of the address space no longer
  overflows the read callback.
- `Sleigh::clone` replays the pinned context the original held, so a clone
  decodes as its source does, up to `MAX_CONTEXT_COMMITS` (4096) commits; past
  that the log is dropped and `Sleigh::clone_would_replay_context` says which
  a clone gets.

## 0.1.0

First release.
