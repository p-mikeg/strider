"""Deep pattern nesting raises rather than taking the process down.

Compiling a pattern is native recursion mirroring its depth, and overflowing
the Rust stack is a SIGSEGV no Python `except` sees. A level count alone does
not bound stack: a frame costs many times more in an unoptimised build, and a
`threading.stack_size` thread is a fraction of the main thread's, so a pattern
well inside the documented 512 levels used to kill a 2 MiB thread outright.
"""

from __future__ import annotations

import subprocess
import sys
import textwrap
import threading

import pytest

import strider
from strider import pattern as p

#: Comfortably inside the 512-level count bound (a nested call costs two), so
#: only the stack bound can answer here.
_NESTED = 255

#: The smallest stack a caller realistically asks for.
_SMALL_STACK = 1 << 20

#: A quarter of that, well under any fixed budget a guard could assume.
_TINY_STACK = 256 * 1024


def _chain(n: int):
    pat = p.anything()
    for _ in range(n):
        pat = p.int_add(pat, p.anything())
    return pat


def _function():
    code = bytes([0x48, 0x01, 0xF8, 0xC3])  # add rax, rdi ; ret
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    return lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv()).function


def _on_thread(fn, stack_size: int):
    """Run `fn` on a thread with `stack_size` bytes and give back what it
    returned or raised."""
    box: list = []

    def run():
        try:
            box.append(("ok", fn()))
        except BaseException as e:  # noqa: BLE001 (reported to the caller)
            box.append(("raised", e))

    old = threading.stack_size(stack_size)
    try:
        t = threading.Thread(target=run)
        t.start()
        t.join()
    finally:
        threading.stack_size(old)
    return box[0]


def test_a_deep_query_on_a_small_thread_stack_survives():
    """Used to be SIGSEGV, which takes the whole process down: reaching the
    assertions at all is most of what this proves. An optimised build fits the
    chain inside the budget and answers; an unoptimised one does not and says
    so, and either is a live interpreter."""
    fn = _function()
    kind, value = _on_thread(lambda: fn.find_all(_chain(_NESTED)), _SMALL_STACK)
    if kind == "raised":
        # Which bound answers depends on the build: the stack budget in an
        # unoptimised one, the matcher's node cap in an optimised one.
        assert isinstance(value, strider.StriderError), value
        assert "too deep" in str(value) or "nodes, over the" in str(value)


@pytest.mark.parametrize("nested", [20, 40])
def test_a_query_on_a_quarter_megabyte_thread_raises_rather_than_crashing(nested):
    """The budget comes from the thread's own stack bounds, so a stack smaller
    than any fixed budget still refuses in time. A SIGSEGV takes the whole
    process down, so the run is a child and the parent reads its exit code."""
    body = textwrap.dedent(
        f"""\
        import threading

        import strider
        from strider import pattern as p

        mem = strider.reader.BufferReader(0x1000, bytes([0x48, 0x01, 0xF8, 0xC3]))
        lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
        fn = lift.analyze(
            0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
        ).function

        def run():
            pat = p.anything()
            for _ in range({nested}):
                pat = p.int_add(pat, p.anything())
            try:
                print("matches", len(list(fn.find_all(pat))), flush=True)
            except strider.StriderError as e:
                print("raised", e, flush=True)

        threading.stack_size({_TINY_STACK})
        t = threading.Thread(target=run)
        t.start()
        t.join()
        """
    )
    out = subprocess.run(
        [sys.executable, "-c", body], capture_output=True, text=True, timeout=120
    )
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    assert out.stdout.startswith(("raised", "matches")), out.stdout


def test_the_count_bound_still_answers_for_a_pathological_chain():
    fn = _function()
    with pytest.raises(strider.StriderError, match="nesting too deep"):
        fn.find_all(_chain(4000))


def test_a_reentrant_index_raises_instead_of_recursing_forever():
    """A `__index__` that re-enters the builder must raise, not recurse
    forever. The depth guard's error used to be swallowed by the
    int-extraction attempt and retried by the next one, which is exponential.

    Which bound fires first depends on the build: a debug frame exhausts the
    stack budget, a release one is small enough that CPython's recursion limit
    trips first, at about two Python frames per level."""
    builder = p.load()

    class Reentrant:
        def __index__(self):
            builder.into_pat()
            return 3

    builder.addr(Reentrant())  # type: ignore[arg-type]
    with pytest.raises((strider.StriderError, RecursionError)):
        builder.into_pat()


def test_a_raising_index_surfaces_its_own_error():
    """The operand walk reads an int through `__index__`; an exception from
    there is the caller's, not a "this is not an int" signal."""
    sentinel = ValueError("from __index__")

    class Boom:
        def __index__(self):
            raise sentinel

    with pytest.raises(ValueError, match="from __index__"):
        p.load().addr(Boom()).into_pat()  # type: ignore[arg-type]
