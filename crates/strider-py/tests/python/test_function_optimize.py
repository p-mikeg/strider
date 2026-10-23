"""`Function.optimize` runs a pipeline in place against the rom of the
`Lifter` that produced the function."""

from __future__ import annotations

import pytest

import strider
from strider import pattern as p

_CODE = bytes([0x48, 0x8B, 0x04, 0x25, 0x00, 0x20, 0x40, 0x00, 0xC3])  # mov rax,[0x402000]; ret
_BASE = 0x401000
_RO = 0x402000
_CC = strider.sleigh.CallingConvention.x86_64_systemv()


def _lifter(fill: int):
    mem = strider.reader.BufferReader(_BASE, _CODE)
    rom = strider.reader.BufferReader(_RO, bytes([fill]) * 8)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, rom=rom)


def _unoptimized(lift):
    return lift.analyze(
        _BASE,
        _CC,
        opts=strider.lift.LifterOptions(
            pipeline=strider.opt.OptimizerPipeline.empty()
        ),
    ).function


def _folded(fn, fill: int) -> bool:
    word = fill * 0x0101_0101_0101_0101
    return not fn.find_all(p.load()) and len(fn.find_all(p.int_const(word))) == 1


def test_optimize_runs_the_default_pipeline_against_its_lifters_rom():
    fn = _unoptimized(_lifter(0x11))
    assert len(fn.find_all(p.load())) == 1
    fn.optimize()
    assert _folded(fn, 0x11)


def test_each_function_folds_against_the_rom_of_its_own_lifter():
    """Two handles over different roms: each function's load folds to the
    bytes its own handle serves."""
    a, b = _unoptimized(_lifter(0x11)), _unoptimized(_lifter(0x22))
    b.optimize()
    a.optimize()
    assert _folded(a, 0x11)
    assert _folded(b, 0x22)


def test_optimize_takes_a_pipeline():
    fn = _unoptimized(_lifter(0x11))
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    fn.optimize(pipeline=pipe)
    assert len(fn.find_all(p.load())) == 1, "no LoadReadOnly in this pipeline"
    pipe.add(strider.opt.LoadReadOnly())
    fn.optimize(pipeline=pipe)
    assert _folded(fn, 0x11)


def test_optimize_adds_the_claims_it_is_given():
    fn = _unoptimized(_lifter(0x11))
    wider = fn.assumptions | strider.lift.AssumptionOptions.none(escape_analysis=True)
    assert fn.assumptions != wider
    fn.optimize(assumptions=wider)
    assert fn.assumptions == wider


def test_optimize_lives_on_the_function_only():
    assert not hasattr(_lifter(0x11), "optimize")
    assert not hasattr(strider.opt, "optimize")


def test_a_positional_pipeline_is_refused():
    """`assumptions` is first, so a pipeline passed positionally does not
    silently bind to it."""
    fn = _unoptimized(_lifter(0x11))
    with pytest.raises(TypeError):
        fn.optimize(strider.opt.OptimizerPipeline.default())  # type: ignore[arg-type]
    assert len(fn.find_all(p.load())) == 1, "nothing ran"
