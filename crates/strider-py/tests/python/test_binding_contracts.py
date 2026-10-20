"""Contracts the Rust bindings answer for.

The options views hand out immutable values, `build_cfg` reports only the seeds
it seated, `pcode_at` answers the same whatever order the calls came in,
merging an image does nothing when it is already loaded and reaches every view
from any thread, `build_cfg` and `analyze` answer the same for a bad
`entry`, a rewrite does not burn a one-shot pattern on a bad replacement,
importing strider exports nothing to a child process, a custom convention is
checked against the register table rather than the preset name, and a pattern
shared into an exponential tree is refused rather than compiled.
"""

from __future__ import annotations

import os
import subprocess
import sys
import time

import pytest

import strider
from strider import template as tpl
from strider.pattern import Capture, anything, int_add, ret, var
from strider.sleigh import CallingConvention, SleighArch

from .conftest import fixture_path

_BASE = 0x1000
#: mov rax, [rdi] ; jmp rax
_INDIRECT = bytes([0x48, 0x8B, 0x07, 0xFF, 0xE0])
_DISPATCH = 0x1003
#: xor rax, rax ; ret
_STRAIGHT = bytes([0x48, 0x31, 0xC0, 0xC3])


def _image_state(prog):
    """The handle's state for comparison: `repr` and a symbol count miss a
    merge that replaced region bytes in place, so the bytes at every symbol
    are part of it."""
    syms = sorted((s.name, s.address) for s in prog.iter_symbols())
    return (repr(prog.reader()), syms, [prog.read(a & ~1, 16) for _, a in syms])


def _x86_64(code: bytes, base: int = _BASE):
    return strider.lift.lifter(
        SleighArch.x86_64(), strider.reader.BufferReader(base, code)
    )


# --- the options views -------------------------------------------------------


def test_known_targets_values_cannot_be_edited():
    """The proxy blocks its keys, and a list value is read back immutable, so
    an edit through one cannot reach the table the analysis reads."""
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


# --- build_cfg's seeded sites -----------------------------------------------


def test_a_seed_the_build_seated_nowhere_is_not_reported():
    """`build_cfg` reports only the seeds it seated, so a whole-binary answer
    table leaves `incomplete` empty and names no address the function does not
    dispatch from."""
    opts = strider.cfg.CfgOptions(known_targets={0xDEAD0000: [0xDEAD1000]})
    cfg = _x86_64(_STRAIGHT).build_cfg(_BASE, opts)
    assert cfg.incomplete == []
    # `analyze` over the same options says the same.
    result = _x86_64(_STRAIGHT).analyze(
        _BASE,
        CallingConvention.x86_64_systemv(),
        strider.lift.LifterOptions(cfg=opts),
    )
    assert result.cfg.incomplete == []


def test_a_seed_the_build_did_seat_is_still_reported():
    opts = strider.cfg.CfgOptions(known_targets={_DISPATCH: [0x1100]})
    cfg = _x86_64(_INDIRECT + bytes(0x100) + _STRAIGHT).build_cfg(
        _BASE,
        opts,
    )
    assert [a for a, r in cfg.incomplete if r == "unverified_seed"] == [_DISPATCH]


# --- pcode_at is history-independent ----------------------------------------

#: mov r0,r0 ; b +0 ; .thumb adds;adds ; .arm blx 0x1008 ; bx lr
_ARM_BACKWARD_BLX = bytes.fromhex("0000a0e1000000ea401c401cfdfffffa1eff2fe1")


def _arm():
    return strider.lift.lifter(
        SleighArch.arm(), strider.reader.BufferReader(_BASE, _ARM_BACKWARD_BLX)
    )


@pytest.mark.parametrize("addr", [0x1000, 0x1004, 0x1008, 0x100C])
def test_pcode_at_does_not_depend_on_earlier_calls(addr):
    """`pcode_at` answers the same whatever order the calls came in, a
    backward `blx` pinning Thumb at its target for a longer sweep."""
    expected = _arm().pcode_at(_BASE, addr)
    assert expected, "a decoded address answers p-code"
    walked = _arm()
    walked.pcode_at(_BASE, 0x1010)
    assert walked.pcode_at(_BASE, addr) == expected


class _CountingReader(strider.reader.MemReader):
    """A `BufferReader` at `_BASE` that counts the reads the decoder makes."""

    def __init__(self, code: bytes):
        super().__init__()
        self.inner = strider.reader.BufferReader(_BASE, code)
        self.reads = 0

    def read(self, addr: int, size: int):
        self.reads += 1
        return self.inner.read(addr, size)


def _counted_x86_64(code: bytes):
    reader = _CountingReader(code)
    return strider.lift.lifter(SleighArch.x86_64(), reader), reader


def test_walking_a_stream_decodes_it_once():
    """One sweep serves a whole function's instructions: walking every address
    reads what one sweep to the last address reads, and asking again reads
    nothing."""
    count = 4000
    code = bytes([0x48, 0x83, 0xC0, 0x01]) * count  # add rax,1
    addrs = [_BASE + 4 * i for i in range(count)]

    swept, swept_reader = _counted_x86_64(code)
    swept.pcode_at(_BASE, addrs[-1])
    sweep = swept_reader.reads
    swept.pcode_at(_BASE, addrs[-1])
    assert swept_reader.reads == sweep, "an address already answered reads nothing"

    walked, walked_reader = _counted_x86_64(code)
    for addr in addrs:
        walked.pcode_at(_BASE, addr)
    assert walked_reader.reads == sweep > 0


# --- add_elf ----------------------------------------------------------------


def test_an_add_elf_off_thread_merges_into_every_view():
    """`add_elf` from a thread that did not build the handle merges, and
    `symbol`, `read` and the decoder all see the new ELF, as an on-thread
    merge does."""
    import threading

    path, other = str(fixture_path("x86", "memory")), str(fixture_path("x86", "elf_relocs"))
    prog, expected = strider.lift.load_elf(path), strider.lift.load_elf(path)
    expected.add_elf(other)
    failure: list[str] = []

    def off_thread():
        try:
            prog.add_elf(other)
        except Exception as e:
            failure.append(repr(e))

    t = threading.Thread(target=off_thread, daemon=True)
    t.start()
    t.join(60)
    assert not t.is_alive(), "add_elf hung"
    assert failure == []
    assert _image_state(prog) == _image_state(expected)
    assert (
        prog.analyze("helper_a").function.to_text()
        == expected.analyze("helper_a").function.to_text()
    )


def test_re_merging_a_loaded_image_does_nothing():
    """A byte-identical re-merge is a no-op: no region and no symbol is
    appended twice."""
    path = str(fixture_path("x86", "memory"))
    prog = strider.lift.load_elf(path)
    before = _image_state(prog)
    prog.add_elf(path)
    assert _image_state(prog) == before


# --- the exception contract -------------------------------------------------


@pytest.mark.parametrize("entry", [-1, 2**64, "add"])
def test_analyze_and_build_cfg_answer_the_same_for_a_bad_entry(entry):
    """Both raise the `StriderError` naming the real cause, not a bare
    `OverflowError`."""
    lifter = _x86_64(_STRAIGHT)
    with pytest.raises(strider.StriderError, match="`entry`") as analyzed:
        lifter.analyze(entry, CallingConvention.x86_64_systemv())
    with pytest.raises(strider.StriderError, match="`entry`") as built:
        lifter.build_cfg(entry)
    assert str(built.value) == str(analyzed.value)


# --- rewrite does not burn a one-shot pattern -------------------------------


def _analyzed_function():
    lifter = _x86_64(_STRAIGHT)
    return lifter.analyze(_BASE, CallingConvention.x86_64_systemv()).function


def test_a_bad_replacement_leaves_the_find_pattern_usable():
    fn = _analyzed_function()
    lhs = ret().into_pat()
    with pytest.raises(strider.StriderError):
        fn.rewrite(lhs, tpl.int_add(anything(), 1))  # type: ignore[arg-type]
    assert len(fn.find_all(lhs)) == 1


def test_a_bad_replacement_in_rewrite_all_leaves_them_usable():
    fn = _analyzed_function()
    lhs = ret().into_pat()
    with pytest.raises(strider.StriderError):
        fn.rewrite_all([(lhs, tpl.int_add(anything(), 1))])  # type: ignore[arg-type]
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
    return CallingConvention.custom(
        sleigh=arch,
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
    lifter = strider.lift.lifter(
        SleighArch.arm_thumb(), strider.reader.BufferReader(_BASE, b"\x1e\xff\x2f\xe1")
    )
    texts = [
        lifter.analyze(_BASE, _custom_cc(arch)).function.to_text(False)
        for arch in (SleighArch.arm(), SleighArch.arm_thumb())
    ]
    # The `default_cc` line carries every resolved varnode, so an empty or
    # wrongly resolved table would not read as agreement.
    assert "link_register_vn=%" in texts[0], texts[0]
    assert texts[0] == texts[1]


def test_a_different_register_table_is_still_refused():
    cc = _custom_cc(SleighArch.arm())
    lifter = _x86_64(_STRAIGHT)
    with pytest.raises(strider.StriderError, match="register table"):
        lifter.analyze(_BASE, cc)


# --- predicates and pattern size -------------------------------------------


def test_a_pattern_shared_into_an_exponential_tree_is_refused():
    """A `Pat` shared into an exponential tree is refused before it is
    lowered: every operand lowers eagerly, so the node cap alone would be
    reached only after 2**n closures existed."""
    q = var(Capture("x"))
    for _ in range(24):
        q = int_add(q, q)
    fn = _analyzed_function()
    t = time.perf_counter()
    with pytest.raises(strider.StriderError, match="too large to compile"):
        fn.find_all(q)
    assert time.perf_counter() - t < 2.0
