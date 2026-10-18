import contextlib
import gc
import os
import threading

import pytest
import strider

ELF = "fixtures/out/x86/memory.elf"
FUNC = "array_sum"


def _rss_mb():
    with open("/proc/self/statm") as f:
        return int(f.read().split()[1]) * os.sysconf("SC_PAGESIZE") // (1 << 20)


def _analyze():
    el = strider.lift.load_elf(ELF)
    cfg, fn, _unresolved = el.analyze(FUNC)
    return el, cfg, fn


def _drop_on_new_thread(box):
    t = threading.Thread(target=lambda: (box.clear(), gc.collect()))
    t.start()
    t.join()


@pytest.mark.skipif(not os.path.exists("/proc/self/statm"), reason="needs procfs")
def test_dropping_a_function_off_thread_does_not_leak_its_lifter():
    """A `Function` outliving its `Lifter` on another thread frees the whole
    analysis. PyO3 refuses to run an `unsendable` destructor off-thread and
    leaks the object instead, so none of these may be `unsendable`."""

    def wave(n):
        for _ in range(n):
            el, cfg, fn = _analyze()
            del el, cfg
            box = [fn]
            del fn
            _drop_on_new_thread(box)
        gc.collect()

    wave(4)
    settled = _rss_mb()
    wave(12)
    # One ELF is ~20 MB, so a per-drop leak would add hundreds of MB here.
    assert _rss_mb() - settled < 40


def test_a_render_runs_off_the_thread_that_built_the_lifter():
    """A render reads the arch's name table, not the decoder, so a worker
    thread renders exactly what the building thread does."""
    _el, cfg, fn = _analyze()
    here = {"pretty": fn.to_dot(pretty=True), "cfg_dot": cfg.to_dot()}
    there = {}

    def body():
        there["pretty"] = fn.to_dot(pretty=True)
        there["cfg_dot"] = cfg.to_dot()

    t = threading.Thread(target=body, daemon=True)
    t.start()
    t.join(60)
    assert not t.is_alive(), "render thread hung"
    assert there == here


def test_work_that_needs_no_sleigh_still_runs_off_thread():
    _el, cfg, fn = _analyze()
    out = {}

    def body():
        dot = fn.to_dot()
        out["dot"] = len(dot) if dot is not None else 0
        out["entry"] = cfg.entry

    t = threading.Thread(target=body)
    t.start()
    t.join()
    assert out["dot"] > 0
    assert out["entry"] is not None


def _off_thread(thunk):
    out = {}

    def body():
        try:
            out["v"] = thunk()
        except Exception as e:
            out["v"] = type(e).__name__

    t = threading.Thread(target=body, daemon=True)
    t.start()
    t.join(60)
    assert not t.is_alive(), "worker hung"
    return out["v"]


def test_analyze_off_thread_answers_as_on_thread():
    el = strider.lift.load_elf(ELF)
    here = el.analyze(FUNC).function.to_text()
    assert _off_thread(lambda: el.analyze(FUNC).function.to_text()) == here


def test_call_other_abi_answers_off_thread():
    el = strider.lift.load_elf("fixtures/out/x86/memory.elf")
    assert _off_thread(lambda: el.call_other_abi("in")) == el.call_other_abi("in")


def test_add_elf_off_thread_merges_for_every_thread():
    el = strider.lift.load_elf("fixtures/out/x86/memory.elf")
    el.analyze("array_sum")
    added = _off_thread(lambda: el.add_elf("fixtures/out/x86/elf_relocs.elf") or "ok")
    assert added == "ok"
    assert "helper_a" in el.symbols()
    el.analyze("array_sum")


_ADD_RET = b"\x48\x83\xc0\x01\xc3"  # add rax, 1 ; ret
_CC = strider.sleigh.CallingConvention.x86_64_systemv()
_IGNORE = strider.lift.LifterOptions(on_incomplete=strider.lift.OnIncomplete.IGNORE)


@contextlib.contextmanager
def _analyze_in_flight(build, inner, entry, cc):
    """Yield `build(reader)` and an earlier `Function` of it while a worker
    thread's `analyze` on it sits inside `reader.read`."""
    armed, entered, release = threading.Event(), threading.Event(), threading.Event()

    class Blocking(strider.reader.MemReader):
        def read(self, addr, size):
            if armed.is_set() and not entered.is_set():
                entered.set()
                release.wait(30)
            return inner.read(addr, size)

    lift = build(Blocking())
    earlier = lift.analyze(entry, cc, _IGNORE).function
    first = {}

    def run():
        try:
            first["fn"] = lift.analyze(entry, cc, _IGNORE).function
        except Exception as e:
            first["err"] = repr(e)

    armed.set()
    t = threading.Thread(target=run, daemon=True)
    t.start()
    try:
        assert entered.wait(10), first
        yield lift, earlier
    finally:
        release.set()
        t.join(60)
    assert not t.is_alive(), "analyze hung"
    assert "fn" in first, first


def _x86_64(mem):
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def _plain_in_flight():
    return _analyze_in_flight(_x86_64, strider.reader.BufferReader(0x1000, _ADD_RET), 0x1000, _CC)


def test_a_second_call_while_one_is_in_flight_raises_the_reentrant_error():
    """Off-thread use is legal; concurrent use of one handle is not. The
    first `analyze` holds the handle while its reader blocks, and a call from
    another thread meanwhile raises rather than interleaving."""
    with _plain_in_flight() as (lift, _earlier):
        with pytest.raises(strider.StriderError, match="already in use"):
            lift.analyze(0x1000, _CC, _IGNORE)


_METHODS = {
    "reg": lambda lift, fn, vn: lift.reg("RAX"),
    "reg_name": lambda lift, fn, vn: lift.reg_name(vn),
    "user_op_names": lambda lift, fn, vn: lift.user_op_names(),
    "call_other_abi": lambda lift, fn, vn: lift.call_other_abi("in"),
    "pcode_at": lambda lift, fn, vn: lift.pcode_at(0x1000, 0x1000),
    "reader": lambda lift, fn, vn: lift.reader(),
    "rom": lambda lift, fn, vn: lift.rom(),
    "arch": lambda lift, fn, vn: lift.arch,
    "build_cfg": lambda lift, fn, vn: lift.build_cfg(0x1000),
    "optimize": lambda lift, fn, vn: lift.optimize(fn),
    "_rebuild": lambda lift, fn, vn: lift._rebuild(
        strider.sleigh.SleighArch.x86_64(), strider.reader.BufferReader(0x1000, _ADD_RET)
    ),
}


@pytest.mark.parametrize("method", sorted(_METHODS))
def test_a_method_called_while_analyze_is_in_flight_raises_strider_error(method):
    """Every method that reads the handle raises the same catchable error a
    second `analyze` does, from whichever thread it is called."""
    vn = _x86_64(strider.reader.BufferReader(0x1000, _ADD_RET)).reg("RAX")
    with _plain_in_flight() as (lift, fn):
        with pytest.raises(strider.StriderError, match="already in use"):
            _METHODS[method](lift, fn, vn)


def test_an_add_elf_while_analyze_is_in_flight_merges_nothing():
    """The merge and the rebuild take the handle together, so an `add_elf`
    that meets an analysis in flight refuses before the reader or the
    symbol table sees the new ELF."""
    el = strider.lift.load_elf(ELF)
    other = "fixtures/out/x86/elf_relocs.elf"

    def build(mem):
        # Deliberate: the constructor `load_elf` calls, which the stub leaves
        # out, over a reader that can block.
        args = (el._elf, el.arch, el.cc, mem, el._elf.ro_reader())
        return strider.lift.ElfLifter(*args)  # type: ignore[call-arg]

    entry = el.symbol(FUNC).address
    with _analyze_in_flight(build, el.reader(), entry, el.cc) as (lift, _earlier):
        with pytest.raises(strider.StriderError, match="already in use"):
            lift.add_elf(other)
        assert "helper_a" not in el.symbols()
    lift.add_elf(other)
    assert "helper_a" in el.symbols()
