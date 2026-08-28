"""The lowered comparison helpers, matched against real lifted code.

The lowering `a <= b -> Xor(Less(b, a), 1)` is written in the lifter
(`strider-lift`'s `lower_cmp_negated`), again in `strider-pattern`'s typed
builders, and again in the Python `.ordered()` path. Nothing tied the pattern
side to the lifter side: every other test for these helpers builds the shape by
hand, so all three copies could agree with each other while disagreeing with
what the lifter emits, and every `int_le()` query would silently return zero
matches with the suite green.

Two layers, because callers run both. The raw layer is the direct
lifter-versus-helper tie and is the one that must hold for any pipeline; the
default-pipeline layer additionally pins the shapes that survive
`FlagCmpCanonicalize`.

Each case was found by scanning the whole fixture corpus for a function the
helper actually matches. The arch matters: a flag-register arch (AArch64, ARM)
lifts a signed `<=` branch to a flag tree and never emits `INT_SLESSEQUAL` at
all, so `int_sle`'s case is MIPS, which compares into a register.
"""

import pytest

import strider
from strider import pattern as p

from .conftest import fixture_path


def _function(arch: str, case: str, fn: str, *, optimize: bool):
    prog = strider.lift.load_elf(str(fixture_path(arch, case)))
    opts = (
        None
        if optimize
        else strider.lift.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty())
    )
    return prog.analyze(fn, opts=opts).function


RAW_CASES = [
    ("int_le", "aarch64", "arithmetic", "shl", lambda: p.int_le(p.anything(), p.anything())),
    ("int_sle", "mips32be", "complex", "complex_dispatch", lambda: p.int_sle(p.anything(), p.anything())),
    ("float_le", "arm", "floats", "f32_compare", lambda: p.float_le(p.anything(), p.anything())),
    ("float_ne", "aarch64", "floats", "f32_compare", lambda: p.float_ne(p.anything(), p.anything())),
]


@pytest.mark.parametrize(("helper", "arch", "case", "fn", "build"), RAW_CASES)
def test_the_helper_matches_the_raw_lifted_shape(helper, arch, case, fn, build):
    """No optimization: the helper against exactly what the lifter emits."""
    function = _function(arch, case, fn, optimize=False)
    assert function.find_all(build()), (
        f"{helper}: {arch}/{case}::{fn} no longer matches the raw lift; the "
        f"lifter's lowering and the pattern helper have diverged"
    )


@pytest.mark.parametrize(
    ("helper", "arch", "case", "fn", "build"),
    [c for c in RAW_CASES if c[0] != "int_sle"],
)
def test_the_helper_still_matches_after_the_default_pipeline(helper, arch, case, fn, build):
    """The shapes that survive canonicalization, for a caller using defaults."""
    function = _function(arch, case, fn, optimize=True)
    assert function.find_all(build()), f"{helper}: {arch}/{case}::{fn} lost its match"


def test_a_signed_le_branch_condition_is_canonicalized_away_under_the_default_pipeline():
    """`int_sle` is raw-only, and that is a property of the passes, not the helper.

    `FlagCmpCanonicalize` rewrites a signed `a <= b` branch condition to
    `!(b < a)` and `IfCondInversion` folds the negation into swapped arms, so a
    bare `Sless` is what survives. A caller running the default pipeline should
    reach for `int_slt`; one running a custom pipeline still sees `int_sle`,
    which is what the raw case above pins.
    """
    fn = "complex_dispatch"
    raw = _function("mips32be", "complex", fn, optimize=False)
    optimized = _function("mips32be", "complex", fn, optimize=True)
    assert raw.find_all(p.int_sle(p.anything(), p.anything()))
    assert not optimized.find_all(p.int_sle(p.anything(), p.anything()))
