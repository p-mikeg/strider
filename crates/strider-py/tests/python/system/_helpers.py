"""The arches are the fixtures/arch/*.mk stems. `load_elf` infers each one's
arch and calling convention, except for the two in `_OVERRIDES`.
"""

from __future__ import annotations

import pathlib
from typing import Callable, Optional

import pytest

import strider
from strider import pattern as pat
from strider.pattern import anything, var, Capture


_FIXTURES = pathlib.Path(__file__).resolve().parents[5] / "fixtures"

ARCH_DIRS: list[str] = sorted(p.stem for p in (_FIXTURES / "arch").glob("*.mk"))

_OVERRIDES: dict[
    str,
    tuple[
        Optional[Callable[[], strider.sleigh.SleighArch]],
        Optional[Callable[[], strider.sleigh.CallingConvention]],
    ],
] = {
    # `-mregparm=3` leaves no trace in the ELF.
    "x86_kernel": (None, strider.sleigh.CallingConvention.x86_linux_kernel),
    # Nothing in the ELF says `-mthumb`; decode as the Rust roster does.
    "arm_thumb": (strider.sleigh.SleighArch.arm_thumb, None),
}


def analyze(
    arch_id: str,
    case: str,
    fn_name: str,
    *,
    fixtures_dir,
):
    """Lift fixtures/out/<arch>/<case>.elf::<fn_name> under the default
    optimiser pipeline (which already includes LoadReadOnly), returning
    the Function. A missing fixture or symbol fails the test.

    Unresolved indirect branches stay as `IndirectBranch` placeholders
    instead of failing the run: pattern-shape tests don't depend on
    resolution, which is covered by the dedicated jump-table tests.
    """
    elf = fixtures_dir / arch_id / f"{case}.elf"
    if not elf.exists():
        pytest.fail(f"fixture missing: {elf} (run `make -C fixtures ARCH={arch_id}`)")
    loaded = strider.lift.load_elf(str(elf))
    symbol = loaded.symbol_opt(fn_name)
    if symbol is None:
        pytest.fail(f"symbol {fn_name!r} not present in {elf}")
    arch_ctor, cc_ctor = _OVERRIDES.get(arch_id, (None, None))
    arch = arch_ctor() if arch_ctor else loaded.arch
    cc = cc_ctor() if cc_ctor else loaded.cc
    mem = loaded.reader()
    lift = strider.lift.lifter(arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        symbol.address,
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


def count_calls(g) -> int:
    return count_pat(g, pat.call())



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
