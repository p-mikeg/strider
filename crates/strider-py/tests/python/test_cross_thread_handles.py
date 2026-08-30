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
    """A `Function` outliving its `Lifter` on another thread used to leak the
    whole analysis: PyO3 refuses to run an `unsendable` destructor off-thread,
    so it leaked the object instead."""

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


def test_sleigh_backed_work_off_thread_raises_instead_of_killing_the_thread():
    """`PanicException` is a `BaseException`, so it escaped `except Exception`
    and the worker died silently. These now raise a catchable error."""
    _el, cfg, fn = _analyze()
    caught = {}

    def run(label, thunk):
        def body():
            try:
                thunk()
                caught[label] = "ok"
            except Exception as e:  # must be reachable
                caught[label] = type(e).__name__

        t = threading.Thread(target=body)
        t.start()
        t.join()

    run("pretty", lambda: fn.to_dot(pretty=True))
    run("cfg_dot", lambda: cfg.to_dot())
    assert caught == {"pretty": "StriderError", "cfg_dot": "StriderError"}


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
    """`add_elf` rebuilds the underlying handle. Rebuilding from a foreign
    thread used to re-pin it there, so the thread that BUILT it lost the
    handle permanently, with an error naming the wrong thread as foreign."""
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
