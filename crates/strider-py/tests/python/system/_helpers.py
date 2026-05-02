"""Shared helpers for the per-arch system test suite.

Mirror of `crates/strider/tests/common/mod.rs` for the Python bindings:
provides an `analyze(arch_id, case, fn_name) → Graph` entry that runs
the full strider pipeline (CFG → IR → indirect-branch fixed-point loop
→ optimiser pipeline) against a fixture ELF, plus the same
`count_*` / `has_*` assertion vocabulary the Rust suite exposes.

The arch registry is a list of `(arch_id, arch_factory, cc_factory)`
triples — pytest parametrises over it via the `arch_id` fixture in
conftest.py.  Adding a new arch is mechanical: add a row here.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable

import pytest

import strider
from strider import pattern as pat
from strider.pattern import any_, var, Capture


# ── Architecture registry ────────────────────────────────────────────────


@dataclass(frozen=True)
class ArchSpec:
    id: str
    arch_factory: Callable[[], "strider.SleighArch"]
    cc_factory: Callable[[], "strider.CallingConvention"]
    thumb_mask: bool = False  # ARM-Thumb symbol address has its LSB set


ARCHES: list[ArchSpec] = [
    ArchSpec("x86", strider.SleighArch.x86, strider.CallingConvention.x86_cdecl),
    # x86_kernel: same Sleigh as x86, but the fixtures live in a
    # separate directory (`fixtures/out/x86_kernel/`) where every case
    # is compiled with `-mregparm=3` and analysed under
    # `x86_linux_kernel`.  Mirrors the Rust `Arch::X86Kernel`
    # variant in crates/strider/tests/common/mod.rs.
    ArchSpec("x86_kernel", strider.SleighArch.x86, strider.CallingConvention.x86_linux_kernel),
    ArchSpec("x64", strider.SleighArch.x86_64, strider.CallingConvention.x86_64_systemv_abi),
    ArchSpec("aarch64", strider.SleighArch.aarch64, strider.CallingConvention.aarch64_aapcs64),
    ArchSpec("aarch64be", strider.SleighArch.aarch64be, strider.CallingConvention.aarch64_aapcs64),
    ArchSpec("arm", strider.SleighArch.arm, strider.CallingConvention.arm_aapcs),
    ArchSpec("arm_be", strider.SleighArch.arm_be, strider.CallingConvention.arm_aapcs),
    ArchSpec("arm_thumb", strider.SleighArch.arm_thumb, strider.CallingConvention.arm_aapcs, thumb_mask=True),
    ArchSpec("mips32le", strider.SleighArch.mipsle32, strider.CallingConvention.mips_o32),
    ArchSpec("mips32be", strider.SleighArch.mipsbe32, strider.CallingConvention.mips_o32),
]

# Lookup helper: id → spec.
_BY_ID: dict[str, ArchSpec] = {a.id: a for a in ARCHES}


def arch_spec(arch_id: str) -> ArchSpec:
    if arch_id not in _BY_ID:
        raise KeyError(f"unknown arch_id {arch_id!r}; supported: {sorted(_BY_ID)}")
    return _BY_ID[arch_id]


# ── Pipeline runner ──────────────────────────────────────────────────────


def analyze(
    arch_id: str,
    case: str,
    fn_name: str,
    *,
    fixtures_dir,
):
    """Lift fixtures/out/<arch>/<case>.elf::<fn_name> to IR and run the
    full optimiser pipeline + LoadReadOnly against it.  Returns the
    Graph.  Skip the test if the fixture is missing.

    Mirrors `crates/strider/tests/common/mod.rs::analyze` — uses the
    custom-pipeline path of `strider.run` so unresolved indirect
    branches show up as `IndirectBranch` placeholders in the IR
    instead of failing the run.  Pattern-shape tests don't depend on
    indirect-branch resolution; the orchestrator path is exercised
    separately by `test_indirect_branch_debug.py` and
    `test_switch_jump_table.py`.
    """
    spec = arch_spec(arch_id)
    elf = fixtures_dir / arch_id / f"{case}.elf"
    if not elf.exists():
        pytest.skip(f"fixture missing: {elf}")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    try:
        addr = mem.symbol(fn_name)
    except Exception:
        pytest.skip(f"symbol {fn_name!r} not present in {elf}")
    if spec.thumb_mask:
        addr &= ~1
    arch = spec.arch_factory()
    cc = spec.cc_factory()

    # Build a Strider so we can construct the convention-aware optimiser
    # pipeline (mirrors `common::analyze`'s `ana.build_optimizer_pipeline`
    # + `opt::LoadReadOnly(rom)`).
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    pipeline = s.build_optimizer_pipeline()
    pipeline.add(strider.opt.LoadReadOnly(mem))

    # `strider.run(pipeline=…)` lifts via `analyze_cfg` and applies the
    # supplied pipeline, leaving any `IndirectBranch` placeholder in
    # the IR — same shape as the Rust suite's `analyze` helper.
    result = strider.run(
        arch=arch,
        cc=cc,
        mem=mem,
        entry=addr,
        pipeline=pipeline,
        allow_code_before_start_addr=True,
    )
    return result.graph


# ── Assertion vocabulary (mirror of common/mod.rs counters) ──────────────
#
# The Rust suite uses `count_int_binop(g, IntBinaryOp::Add)`-style
# counters keyed on the IR's `NodeKind`.  Python's pattern surface
# doesn't expose `NodeKind` directly, so we map each counter onto the
# pattern's structural matcher.  The matcher walks every reachable node;
# `find_all` returns one Match per root binding, so its length is the
# count.  For commutative ops (add/mul/and/or/xor and the cmp ops the
# pattern crate marks commutative), each match shape is counted once.


def _to_pat(p):
    """Coerce builder objects (IntBinaryPat / etc.) to a Pat — most
    typed builders expose `.into_pat()`; plain Pats round-trip
    unchanged."""
    return p.into_pat() if hasattr(p, "into_pat") else p


def count_pat(g, p) -> int:
    return len(g.find_all(_to_pat(p)))


# `and_` / `or_` are exposed verbatim as `and` / `or` by the Rust
# bindings (Python keyword names — accessible only via `getattr`).
# Use `getattr(pat, "and")` rather than `pat.and_` for the bitwise
# variants until strider-py adds the trailing-underscore aliases.
_INT_BINOP_BUILDERS = {
    "Add": pat.add,
    "Sub": pat.sub,
    "Mul": pat.mul,
    "Div": pat.div,
    "Sdiv": pat.sdiv,
    "Rem": pat.rem,
    "Srem": pat.srem,
    "And": getattr(pat, "and"),
    "Or": getattr(pat, "or"),
    "Xor": pat.xor,
    "ShiftLeft": pat.shl,
    "ShiftRight": pat.shr,
    "SShiftRight": pat.sshr,
}


def count_int_binop(g, op: str) -> int:
    builder = _INT_BINOP_BUILDERS.get(op)
    if builder is None:
        raise ValueError(f"unknown IntBinaryOp variant {op!r}")
    return count_pat(g, builder(any_(), any_()))


def count_int_unop(g, op: str) -> int:
    if op == "Neg":
        return count_pat(g, pat.neg(any_()))
    if op == "Not":
        return count_pat(g, pat.not_(any_()))
    raise ValueError(f"unknown int unop {op!r}")


def count_calls(g) -> int:
    return count_pat(g, pat.call())


def count_returns(g) -> int:
    return count_pat(g, pat.ret())


def count_ifs(g) -> int:
    return count_pat(g, pat.if_())


def count_loads(g) -> int:
    return count_pat(g, pat.load())


def count_stores(g) -> int:
    # `stack_store` is a structural variant of `store`; either counts as
    # a write to memory.
    return count_pat(g, pat.store()) + count_pat(g, pat.stack_store())


def count_loops(g) -> int:
    # Counts CFG loop headers (ControlStates with a back-edge predecessor).
    # Robust to RedundantPhis collapsing loop-invariant tracked variables —
    # a real loop with no surviving VarPhi at its header still counts.
    return g.count_loop_headers()


def count_int_consts(g) -> int:
    c = Capture()
    return count_pat(g, pat.any_int_const(c))


def has_constant(g, value: int) -> bool:
    """Returns True iff some `IntConst(value)` node exists.

    Comparison is performed against the typed extractor `match.uint(c)`
    masked to the constant's output width — matches the Rust
    `has_constant` which compares the stored u128 against `u64::from
    (value)`.
    """
    c = Capture()
    hits = g.find_all(pat.any_int_const(c))
    target = value & 0xFFFF_FFFF_FFFF_FFFF
    for m in hits:
        u = m.uint(c)
        if u is None:
            continue
        if u & 0xFFFF_FFFF_FFFF_FFFF == target:
            return True
    return False


def has_kind_match(g, p) -> bool:
    return count_pat(g, p) > 0
