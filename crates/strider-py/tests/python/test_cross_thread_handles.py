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

    t = threading.Thread(target=body)
    t.start()
    t.join()
    return out["v"]


def test_call_other_abi_raises_off_thread_instead_of_answering_none():
    """`None` is the answer for a name strider does not classify, which fails
    the lift. Degrading an off-thread failure to it reported a different,
    wrong classification."""
    el = strider.lift.load_elf("fixtures/out/x86/memory.elf")
    assert el.call_other_abi("in") is not None
    assert _off_thread(lambda: el.call_other_abi("in")) == "StriderError"


def test_add_elf_off_thread_does_not_steal_the_handle():
    """`add_elf` rebuilds the underlying handle, and the rebuild keeps the
    pin on the building thread. Re-pinning to the caller would cost that
    thread the handle permanently, under an error naming it as the foreign
    one."""
    el = strider.lift.load_elf("fixtures/out/x86/memory.elf")
    el.analyze("array_sum")
    added = _off_thread(
        lambda: el.add_elf("fixtures/out/x86/elf_relocs.elf") or "ok"
    )
    assert added == "StriderError"
    # Still usable on its own thread, and the merge still works there.
    el.analyze("array_sum")
    el.add_elf("fixtures/out/x86/elf_relocs.elf")
    assert "helper_a" in el.symbols()
