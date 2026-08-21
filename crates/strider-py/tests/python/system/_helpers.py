"""Adding a new arch means adding a row to ARCHES; conftest.py parametrises
the `arch_id` fixture over it.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable

import pytest

import strider
from strider import pattern as pat
from strider.pattern import anything, var, Capture


@dataclass(frozen=True)
class ArchSpec:
    id: str
    arch_factory: Callable[[], "strider.sleigh.SleighArch"]
    cc_factory: Callable[[], "strider.sleigh.CallingConvention"]



ARCHES: list[ArchSpec] = [
    ArchSpec("x86", strider.sleigh.SleighArch.x86, strider.sleigh.CallingConvention.x86_cdecl),
    # Same Sleigh as x86, but its fixtures live in fixtures/out/x86_kernel/
    # and are compiled with -mregparm=3 to match x86_linux_kernel.
    ArchSpec("x86_kernel", strider.sleigh.SleighArch.x86, strider.sleigh.CallingConvention.x86_linux_kernel),
    ArchSpec("x64", strider.sleigh.SleighArch.x86_64, strider.sleigh.CallingConvention.x86_64_systemv),
    ArchSpec("aarch64", strider.sleigh.SleighArch.aarch64, strider.sleigh.CallingConvention.aarch64_aapcs64),
    ArchSpec("aarch64be", strider.sleigh.SleighArch.aarch64be, strider.sleigh.CallingConvention.aarch64_aapcs64),
    ArchSpec("arm", strider.sleigh.SleighArch.arm, strider.sleigh.CallingConvention.arm_aapcs),
    ArchSpec("arm_be", strider.sleigh.SleighArch.arm_be, strider.sleigh.CallingConvention.arm_aapcs),
    ArchSpec("arm_thumb", strider.sleigh.SleighArch.arm_thumb, strider.sleigh.CallingConvention.arm_aapcs),
    ArchSpec("mips32le", strider.sleigh.SleighArch.mipsle32, strider.sleigh.CallingConvention.mips_o32),
    ArchSpec("mips32be", strider.sleigh.SleighArch.mipsbe32, strider.sleigh.CallingConvention.mips_o32),
]

_BY_ID: dict[str, ArchSpec] = {a.id: a for a in ARCHES}


def arch_spec(arch_id: str) -> ArchSpec:
    if arch_id not in _BY_ID:
        raise KeyError(f"unknown arch_id {arch_id!r}; supported: {sorted(_BY_ID)}")
    return _BY_ID[arch_id]


def analyze(
    arch_id: str,
    case: str,
    fn_name: str,
    *,
    fixtures_dir,
):
    """Lift fixtures/out/<arch>/<case>.elf::<fn_name> under the default
    optimiser pipeline (which already includes LoadReadOnly), returning
    the Function.  Skips if the fixture or symbol is missing.

    Unresolved indirect branches stay as `IndirectBranch` placeholders
    instead of failing the run: pattern-shape tests don't depend on
    resolution, which is covered by the dedicated jump-table tests.
    """
    spec = arch_spec(arch_id)
    elf = fixtures_dir / arch_id / f"{case}.elf"
    if not elf.exists():
        pytest.skip(f"fixture missing: {elf}")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    try:
        addr = loaded.symbol(fn_name).address
    except Exception:
        pytest.skip(f"symbol {fn_name!r} not present in {elf}")
    arch = spec.arch_factory()
    cc = spec.cc_factory()

    lift = strider.lift.lifter(arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        addr,
        cc,
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
        ),
    )
    return function


# The counters below stand in for the Rust suite's NodeKind-keyed ones:
# Python doesn't expose NodeKind, so each maps onto a structural pattern.
# `find_all` returns one Match per root binding, so its length is the
# count, and commutative ops still count each shape once.


def _to_pat(p):
    """Coerce builder objects (IntBinaryPat etc.) to a Pat via
    `.into_pat()`; plain Pats round-trip unchanged."""
    return p.into_pat() if hasattr(p, "into_pat") else p


def count_pat(g, p) -> int:
    return len(g.find_all(_to_pat(p)))


_INT_BINOP_BUILDERS = {
    "Add": pat.int_add,
    "Sub": pat.int_sub,
    "Mul": pat.int_mul,
    "Div": pat.int_div,
    "Sdiv": pat.int_sdiv,
    "Rem": pat.int_rem,
    "Srem": pat.int_srem,
    "And": pat.int_and,
    "Or": pat.int_or,
    "Xor": pat.int_xor,
    "ShiftLeft": pat.int_shl,
    "ShiftRight": pat.int_shr,
    "SShiftRight": pat.int_sshr,
}


def count_int_binop(g, op: str) -> int:
    builder = _INT_BINOP_BUILDERS.get(op)
    if builder is None:
        raise ValueError(f"unknown IntBinaryOp variant {op!r}")
    return count_pat(g, builder(anything(), anything()))


def count_int_unop(g, op: str) -> int:
    # `"BitNot"` is a back-compat spelling: `~x` lifts to
    # `Xor(_, IntConst(all_ones))`, matched via `pat.int_not`.
    if op == "Neg":
        return count_pat(g, pat.int_neg(anything()))
    if op == "BitNot":
        return count_pat(g, pat.int_not(anything()))
    raise ValueError(f"unknown int unop {op!r}")


def count_calls(g) -> int:
    return count_pat(g, pat.call())


def count_returns(g) -> int:
    return count_pat(g, pat.ret())


def count_ifs(g) -> int:
    return count_pat(g, pat.if_else())


def count_loads(g) -> int:
    return count_pat(g, pat.load())


def count_stores(g) -> int:
    # Stack-relative writes are the same node kind as any other store,
    # so one `pat.store()` count covers every memory write.
    return count_pat(g, pat.store())


def count_regions(g) -> int:
    # Control-flow joins. The loop tests count these rather than phis: a
    # real loop whose header phi is optimised away still leaves a Region.
    return g.count_regions()


def count_int_consts(g) -> int:
    c = Capture()
    return count_pat(g, pat.int_const(c))


def has_constant(g, value: int) -> bool:
    """True iff some `IntConst(value)` node exists, compared at 64-bit
    width so a value's storage width doesn't affect the answer."""
    c = Capture()
    hits = g.find_all(pat.int_const(c))
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
