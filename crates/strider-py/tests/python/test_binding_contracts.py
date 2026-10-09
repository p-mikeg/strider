"""Contracts the Rust bindings answer for, each one a shape that answered
differently before.

The options views hand out immutable values, `build_cfg` reports only the seeds
it seated, `pcode_at` answers the same whatever order the calls came in,
re-merging a loaded image does nothing, a bad `entry` raises the exception its
conversion raises, a rewrite does not burn a one-shot pattern on a bad
replacement, importing strider exports nothing to a child process, a custom
convention is checked against the register table rather than the preset name,
a `.when()` predicate is read for truth, and an exponentially-shared pattern is
refused rather than compiled.
"""

from __future__ import annotations

import os
import subprocess
import sys
import time

import pytest

import strider
from strider.pattern import Capture, anything, int_add, ret, var
from strider.sleigh import CallingConvention, Sleigh, SleighArch

from .conftest import fixture_path

_BASE = 0x1000
#: mov rax, [rdi] ; jmp rax
_INDIRECT = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])
_DISPATCH = 0x1003
#: xor rax, rax ; ret
_STRAIGHT = bytes([0x48, 0x31, 0xC0, 0xC3])


def _x86_64(code: bytes, base: int = _BASE):
    return strider.lift.lifter(
        SleighArch.x86_64(), strider.reader.BufferReader(base, code)
    )


# --- the options views -------------------------------------------------------


def test_known_targets_values_cannot_be_edited():
    """The proxy blocks its keys; a list value took an edit the analysis never
    read, so two option objects reading the same gave different CFGs."""
    opts = strider.cfg.CfgOptions(known_targets={_DISPATCH: [0x2000]})
    assert opts.known_targets[_DISPATCH] == (0x2000,)
    with pytest.raises(AttributeError):
        # Deliberate: a tuple has no `append`, which is the point.
        opts.known_targets[_DISPATCH].append(0x3000)  # type: ignore[union-attr]
    assert dict(opts.known_targets) == {_DISPATCH: (0x2000,)}


def test_per_address_ccs_is_one_memoised_view():
    cc = CallingConvention.x86_64_systemv()
    opts = strider.lift.LifterOptions(per_address_ccs={0x1000: cc, 0x2000: cc})
    assert opts.per_address_ccs is opts.per_address_ccs
    view = opts.per_address_ccs
    assert view is not None and sorted(view) == [0x1000, 0x2000]
    assert strider.lift.LifterOptions().per_address_ccs is None


# --- build_cfg's seeded channel ---------------------------------------------


def test_a_seed_the_build_seated_nowhere_is_not_reported():
    """A whole-binary answer table made every `build_cfg(...).is_complete()`
    False and named addresses the function does not dispatch from."""
    opts = strider.cfg.CfgOptions(known_targets={0xDEAD0000: [0xDEAD1000]})
    cfg = _x86_64(_STRAIGHT).build_cfg(_BASE, opts)
    assert cfg.unverified_seeded_sites() == []
    assert cfg.is_complete()
    # `analyze` over the same options says the same.
    result = _x86_64(_STRAIGHT).analyze(
        _BASE,
        CallingConvention.x86_64_systemv(),
        strider.lift.LifterOptions(cfg=opts),
    )
    assert result.cfg.is_complete()


def test_a_seed_the_build_did_seat_is_still_reported():
    opts = strider.cfg.CfgOptions(known_targets={_DISPATCH: [0x1100]})
    cfg = _x86_64(_INDIRECT + bytes(0x100) + _STRAIGHT).build_cfg(_BASE, opts)
    assert cfg.unverified_seeded_sites() == [_DISPATCH]


# --- pcode_at is history-independent ----------------------------------------

#: mov r0,r0 ; b +0 ; .thumb adds;adds ; .arm blx 0x1008 ; bx lr
_ARM_BACKWARD_BLX = bytes.fromhex("0000a0e1000000ea401c401cfdfffffa1eff2fe1")


def _arm():
    return strider.lift.lifter(
        SleighArch.arm(), strider.reader.BufferReader(_BASE, _ARM_BACKWARD_BLX)
    )


@pytest.mark.parametrize("addr", [0x1000, 0x1004, 0x1008, 0x100C])
def test_pcode_at_does_not_depend_on_earlier_calls(addr):
    """A backward `blx` decoded by a longer sweep pinned Thumb at its target,
    so the same call answered differently, or raised, by history."""
    fresh = _arm()
    try:
        expected = fresh.pcode_at(_BASE, addr)
    except strider.StriderError as e:
        expected = type(e)
    walked = _arm()
    walked.pcode_at(_BASE, 0x1010)
    if expected is strider.StriderError:
        with pytest.raises(strider.StriderError):
            walked.pcode_at(_BASE, addr)
    else:
        assert walked.pcode_at(_BASE, addr) == expected


def test_walking_a_stream_decodes_it_once():
    """Every call used to re-decode from the entry, so a per-address walk was
    quadratic."""
    count = 4000
    lifter = _x86_64(bytes([0x48, 0x83, 0xC0, 0x01]) * count)  # add rax,1
    addrs = [_BASE + 4 * i for i in range(count)]

    t = time.perf_counter()
    for addr in addrs:
        lifter.pcode_at(_BASE, addr)
    walk = time.perf_counter() - t

    t = time.perf_counter()
    lifter.pcode_at(_BASE, addrs[-1])
    last = time.perf_counter() - t
    # Linear: the whole walk costs about one sweep, not one per address. The
    # cached call reads a dict, so the comparison is against a fresh sweep.
    one_sweep = _x86_64(bytes([0x48, 0x83, 0xC0, 0x01]) * count)
    t = time.perf_counter()
    one_sweep.pcode_at(_BASE, addrs[-1])
    sweep = time.perf_counter() - t
    assert last < sweep
    assert walk < 20 * sweep, f"walk {walk:.3f}s against one sweep {sweep:.3f}s"


# --- add_elf ----------------------------------------------------------------


def test_re_merging_a_loaded_image_does_nothing():
    """The doc promised a byte-identical re-merge was a no-op; it appended
    every region and every symbol again."""
    path = str(fixture_path("x86", "memory"))
    prog = strider.lift.load_elf(path)
    before = (repr(prog.reader()), len(list(prog.iter_symbols())))
    prog.add_elf(path)
    assert (repr(prog.reader()), len(list(prog.iter_symbols()))) == before


# --- the exception contract -------------------------------------------------


@pytest.mark.parametrize("entry", [-1, 2**64, "add"])
def test_analyze_and_build_cfg_answer_the_same_for_a_bad_entry(entry):
    """`build_cfg` raised `OverflowError` where `analyze` raised the
    `StriderError` naming the real cause."""
    lifter = _x86_64(_STRAIGHT)
    with pytest.raises(strider.StriderError):
        lifter.analyze(entry, CallingConvention.x86_64_systemv())
    with pytest.raises(strider.StriderError):
        lifter.build_cfg(entry)


# --- rewrite does not burn a one-shot pattern -------------------------------


def _analyzed_function():
    lifter = _x86_64(_STRAIGHT)
    return lifter.analyze(_BASE, CallingConvention.x86_64_systemv()).function


def test_a_bad_replacement_leaves_the_find_pattern_usable():
    fn = _analyzed_function()
    lhs = ret().into_pat()
    with pytest.raises(strider.StriderError):
        fn.rewrite(lhs, anything())
    assert len(fn.find_all(lhs)) == 1


def test_a_bad_replacement_in_rewrite_all_leaves_them_usable():
    fn = _analyzed_function()
    lhs = ret().into_pat()
    with pytest.raises(strider.StriderError):
        fn.rewrite_all([(lhs, anything())])
    assert len(fn.find_all(lhs)) == 1


# --- importing strider exports nothing --------------------------------------


def test_import_does_not_export_rust_lib_backtrace():
    """`set_var` writes the C `environ`, which children inherit and
    `os.environ` does not show."""
    env = {k: v for k, v in os.environ.items()}
    env.pop("RUST_LIB_BACKTRACE", None)
    env.pop("RUST_BACKTRACE", None)
    out = subprocess.run(
        [
            sys.executable,
            "-c",
            "import strider, subprocess;"
            "print(subprocess.run(['sh','-c','echo ${RUST_LIB_BACKTRACE-unset}'],"
            "capture_output=True, text=True).stdout.strip())",
        ],
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    assert out.stdout.strip() == "unset"


def test_errors_still_carry_a_backtrace():
    """The variable is removed only after std has cached the decision."""
    try:
        _x86_64(_STRAIGHT).analyze(0x9999, CallingConvention.x86_64_systemv())
    except strider.StriderError as e:
        assert "stack backtrace" in e.backtrace.lower()
    else:
        pytest.fail("expected a StriderError")


# --- the custom-convention arch check ---------------------------------------


def _custom_cc(arch) -> CallingConvention:
    sleigh = Sleigh(arch, strider.reader.BufferReader(_BASE, b"\x00" * 4))
    return CallingConvention.custom(
        sleigh=sleigh,
        arg_passing_regs=["r0", "r1"],
        callee_saved_regs=["r4", "lr"],
        ret_val_regs=["r0"],
        ret_val_regs_float=[],
        stack_pointer="sp",
        stack_arg_base=None,
        stack_arg_increment=4,
        ret_stack_pop=0,
        link_register="lr",
        preserves_memory=False,
    )


def test_one_register_table_is_one_arch_for_a_custom_convention():
    """`arm` and `arm_thumb` are one sla and one register table, so a
    convention built on either resolves the same varnodes."""
    cc = _custom_cc(SleighArch.arm())
    lifter = strider.lift.lifter(
        SleighArch.arm_thumb(), strider.reader.BufferReader(_BASE, b"\x1e\xff\x2f\xe1")
    )
    lifter.analyze(_BASE, cc)


def test_a_different_register_table_is_still_refused():
    cc = _custom_cc(SleighArch.arm())
    lifter = _x86_64(_STRAIGHT)
    with pytest.raises(strider.StriderError, match="register table"):
        lifter.analyze(_BASE, cc)


# --- predicates and pattern size -------------------------------------------


def test_a_pattern_shared_into_an_exponential_tree_is_refused():
    """Every operand lowers eagerly, so the matcher's node cap was reached only
    after 2**n closures existed."""
    q = var(Capture("x"))
    for _ in range(24):
        q = int_add(q, q)
    fn = _analyzed_function()
    t = time.perf_counter()
    with pytest.raises(strider.StriderError, match="too large to compile"):
        fn.find_all(q)
    assert time.perf_counter() - t < 2.0
