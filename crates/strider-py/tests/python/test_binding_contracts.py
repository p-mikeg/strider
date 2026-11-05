"""Contracts the Rust bindings answer for.

The options views hand out immutable values, `build_cfg` reports only the seeds
it seated, `pcode_at` answers the same whatever order the calls came in,
merging an image does nothing when it is already loaded and reaches every view
from any thread, `build_cfg` and `analyze` answer the same for a bad
`entry`, a refused rewrite leaves its `into_pat()` pattern usable,
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
    # The arm is `_STRAIGHT`, at 0x1100.
    cfg = _x86_64(_INDIRECT + bytes(0xFB) + _STRAIGHT).build_cfg(
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


@pytest.mark.parametrize(
    ("entry", "says"),
    [(-1, "out of range"), (2**64, "out of range")],
)
def test_pcode_at_names_a_bad_entry_as_analyze_does(entry, says):
    """`pcode_at` takes its `entry` as `analyze` and `build_cfg` do."""
    with pytest.raises(strider.StriderError, match=says):
        _arm().pcode_at(entry, _BASE)


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


@pytest.mark.parametrize("entry", [-1, 2**64])
def test_analyze_and_build_cfg_answer_the_same_for_a_bad_entry(entry):
    """Both raise the `StriderError` naming the real cause, not a bare
    `OverflowError`."""
    lifter = _x86_64(_STRAIGHT)
    with pytest.raises(strider.StriderError, match="`entry`") as analyzed:
        lifter.analyze(entry, CallingConvention.x86_64_systemv())
    with pytest.raises(strider.StriderError, match="`entry`") as built:
        lifter.build_cfg(entry)
    assert str(built.value) == str(analyzed.value)


@pytest.mark.parametrize("entry", ["add", 1.0, None], ids=["str", "float", "none"])
def test_a_wrong_typed_entry_raises_type_error_everywhere(entry):
    """Every entry point takes `entry` alike; only `ElfLifter.analyze` takes a
    symbol name, and no message sends an `ElfLifter` to `load_elf`."""
    lifter = _x86_64(_STRAIGHT)
    prog = strider.lift.load_elf(fixture_path("x64", "memory"))
    calls = [
        lambda: lifter.analyze(entry, CallingConvention.x86_64_systemv()),
        lambda: lifter.build_cfg(entry),
        lambda: lifter.pcode_at(entry, _BASE),
        lambda: prog.build_cfg(entry),
        lambda: prog.pcode_at(entry, _BASE),
    ]
    if not isinstance(entry, str):
        calls.append(lambda: prog.analyze(entry))
    for call in calls:
        with pytest.raises(TypeError, match="`entry`") as raised:
            call()
        assert not isinstance(raised.value, strider.StriderError)
        assert "load_elf" not in str(raised.value)
        if isinstance(entry, str):
            assert "ElfLifter.analyze" in str(raised.value)


class _IndexFails(Exception):
    pass


class _BadIndex:
    def __index__(self) -> int:
        raise _IndexFails("from __index__")


def test_an_entry_index_error_propagates_as_raised():
    """Only a wrong type or an out-of-range int is `entry`'s own error."""
    lifter = _x86_64(_STRAIGHT)
    prog = strider.lift.load_elf(fixture_path("x64", "memory"))
    calls = [
        lambda: lifter.analyze(
            _BadIndex(),  # type: ignore[arg-type]
            CallingConvention.x86_64_systemv(),
        ),
        lambda: lifter.build_cfg(_BadIndex()),  # type: ignore[arg-type]
        lambda: prog.build_cfg(_BadIndex()),  # type: ignore[arg-type]
        lambda: prog.analyze(_BadIndex()),  # type: ignore[arg-type]
    ]
    for call in calls:
        with pytest.raises(_IndexFails):
            call()


class _Index:
    def __init__(self, value: int) -> None:
        self.value = value

    def __index__(self) -> int:
        return self.value


def test_elf_lifter_analyze_takes_an_index_entry_as_an_address():
    prog = strider.lift.load_elf(fixture_path("x64", "memory"))
    main = prog.symbol("main").address
    assert (
        prog.analyze(_Index(main)).function.to_text()  # type: ignore[arg-type]
        == prog.analyze(main).function.to_text()
    )
    with pytest.raises(TypeError, match="`entry`"):
        prog.analyze(True)


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


def test_a_refused_rule_leaves_the_find_pattern_usable():
    """The rule itself is refused (the RHS names a capture the LHS never
    binds), after both sides were built."""
    fn = _analyzed_function()
    lhs = ret().into_pat()
    with pytest.raises(strider.StriderError):
        fn.rewrite(lhs, tpl.var(Capture()))
    assert len(fn.find_all(lhs)) == 1


def test_a_refused_second_rule_leaves_the_first_find_pattern_usable():
    fn = _analyzed_function()
    c = Capture()
    first = ret().capture(c).into_pat()
    with pytest.raises(strider.StriderError):
        fn.rewrite_all([(first, tpl.var(c)), (ret().into_pat(), tpl.var(Capture()))])
    assert len(fn.find_all(first)) == 1


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


def test_an_error_pickles_with_its_backtrace():
    """`.backtrace` renders when read, so a pickled error carries it as text."""
    import pickle

    try:
        _x86_64(_STRAIGHT).analyze(0x9999, CallingConvention.x86_64_systemv())
    except strider.StriderError as e:
        copy = pickle.loads(pickle.dumps(e))
        assert type(copy) is strider.StriderError
        assert str(copy) == str(e)
        assert copy.backtrace == e.backtrace
        assert "stack backtrace" in copy.backtrace.lower()
    else:
        pytest.fail("expected a StriderError")


def test_an_error_raised_from_python_has_an_empty_backtrace():
    assert strider.StriderError("mine").backtrace == ""


def test_backtrace_stays_assignable():
    try:
        _x86_64(_STRAIGHT).analyze(0x9999, CallingConvention.x86_64_systemv())
    except strider.StriderError as e:
        e.backtrace = "mine"
        assert e.backtrace == "mine"
        setattr(e, "backtrace", None)  # the stub types it `str`
        assert e.backtrace is None
        del e.backtrace
        assert e.backtrace == ""
    else:
        pytest.fail("expected a StriderError")


def test_backtrace_has_a_docstring():
    doc = strider.StriderError.backtrace.__doc__
    assert doc and "Rust backtrace" in doc, doc


def test_a_per_address_cc_refusal_keeps_the_backtrace_out_of_its_message():
    """The refusal names the address and the cause; the frames stay on
    `.backtrace`, as for every other error."""
    opts = strider.lift.LifterOptions(
        per_address_ccs={0x2000: CallingConvention.aarch64_aapcs64()}
    )
    try:
        _x86_64(_STRAIGHT).analyze(_BASE, CallingConvention.x86_64_systemv(), opts=opts)
    except strider.StriderError as e:
        assert str(e) == (
            'per-address CC at 0x2000 unresolved: unknown sleigh register name "x0"'
        ), str(e)
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
