"""Deep pattern nesting raises rather than taking the process down.

Compiling a pattern is native recursion mirroring its depth, and overflowing
the Rust stack is a SIGSEGV no Python `except` sees. A level count alone does
not bound stack: a frame costs many times more in an unoptimised build, and a
`threading.stack_size` thread is a fraction of the main thread's, so a pattern
inside the 512-level internal nesting bound can still exhaust a 2 MiB thread.
The limit the stubs document is 256 NODES; 512 levels is the internal
`MAX_PATTERN_NESTING`.
"""

from __future__ import annotations

import subprocess
import sys
import textwrap
import threading

import pytest

import strider
from strider import pattern as p

#: Comfortably inside the 512-level `MAX_PATTERN_NESTING` bound (one level per
#: nested operand), so only the stack bound can answer here.
_NESTED = 255

#: The smallest stack a caller realistically asks for.
_SMALL_STACK = 1 << 20

#: A quarter of that, well under any fixed budget a guard could assume.
_TINY_STACK = 256 * 1024

#: The outermost guard's refusal, on a thread that starts the compile under
#: the reserve one step and its refusal take.
_TOO_LITTLE = "too little of this thread's stack left"

#: The stack budget's refusal, past that reserve.
_OVER_BUDGET = "pattern nesting too deep for this thread's stack"

#: What a stack bound raises on a thread of 32 or 40 KiB: which one is a few
#: KiB of interpreter frames either way.
_STACK_REFUSALS = (_TOO_LITTLE, _OVER_BUDGET)

#: A constant in a memory slot, refused by the operand walk.
_MEMORY_SLOT = "a memory-input slot (`mem`) requires a memory producer"


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


def _child_matches_or_raises(step: str, n: int, stack: int) -> None:
    """Run a query on a `stack`-byte thread over `n` applications of `step`
    (an expression in `pat`) in a child process, since a SIGSEGV takes the
    whole process down, and require that it matched or raised."""
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
            for _ in range({n}):
                pat = {step}
            try:
                print("matches", len(list(fn.find_all(pat))), flush=True)
            except strider.StriderError as e:
                print("raised", e, flush=True)

        threading.stack_size({stack})
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


@pytest.mark.parametrize("nested", [20, 40])
def test_a_query_on_a_quarter_megabyte_thread_raises_rather_than_crashing(nested):
    """The budget comes from the thread's own stack bounds, so a stack smaller
    than any fixed budget still refuses in time."""
    _child_matches_or_raises("p.int_add(pat, p.anything())", nested, _TINY_STACK)


def test_nested_alternations_on_a_one_megabyte_thread_match_or_raise():
    """Each alternation level recurses through the matcher, which costs most
    in an unoptimised build; 255 of them and their leaf are exactly the node
    budget."""
    _child_matches_or_raises("p.one_of([pat])", _NESTED, _SMALL_STACK)


def test_the_count_bound_still_answers_for_a_pathological_chain():
    fn = _function()
    with pytest.raises(strider.StriderError, match="nesting too deep"):
        fn.find_all(_chain(4000))



def test_a_builder_wired_into_itself_is_named_as_a_cycle():
    b = strider.pattern.load()
    b.addr(b)
    with pytest.raises(strider.StriderError, match="contains itself"):
        _function().find_all(b)

def test_the_count_bound_answers_where_no_other_bound_reaches():
    """Past 512 levels but far under the step cap, on a stack whose budget
    holds even an unoptimised build's frames: only the level count refuses."""
    fn = _function()
    kind, value = _on_thread(lambda: fn.find_all(_chain(600)), 256 << 20)
    assert kind == "raised", value
    assert isinstance(value, strider.StriderError), value
    assert "over 512 levels" in str(value), value


def _captures(n: int):
    a = p.Capture("A")
    pat = p.anything()
    for _ in range(n):
        pat = pat.capture(a)
    return pat


@pytest.mark.parametrize(("n", "fits"), [(511, True), (512, False)])
def test_a_pat_root_is_one_of_the_512_levels(n, fits):
    """`anything()` under `n` captures is `n + 1` levels, the root among them:
    512 compile and 513 are refused, though construction accepts both."""
    fn = _function()
    pat = _captures(n)
    kind, value = _on_thread(lambda: fn.find_all(pat), 256 << 20)
    if fits:
        assert kind == "ok", value
    else:
        assert kind == "raised", value
        assert "over 512 levels" in str(value), value


@pytest.mark.parametrize("into_pat", [False, True])
@pytest.mark.parametrize(("n", "fits"), [(510, True), (511, False)])
def test_a_builder_root_is_one_of_the_512_levels(n, fits, into_pat):
    """A `load` over `n` captures of `anything()` is `n + 2` levels, the
    builder root counting one, bare or as its `into_pat()` `Pat`, whose own
    operand walk counts the same and so refuses first."""
    fn = _function()
    sealed = []

    def query():
        root = p.load(_captures(n))
        if into_pat:
            root = root.into_pat()
            sealed.append(root)
        return fn.find_all(root)

    kind, value = _on_thread(query, 256 << 20)
    if fits:
        assert kind == "ok", value
    else:
        assert kind == "raised", value
        assert "over 512 levels" in str(value), value
        assert not sealed


def test_a_reentrant_index_raises_instead_of_recursing_forever():
    """A `__index__` that re-enters the builder must raise, not recurse
    forever. The depth guard's error has to escape the int-extraction attempt
    rather than being swallowed and retried by the next one, which is
    exponential.

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


def _on_small_thread_child(kib: int, setup: str, loop: str):
    """Run `loop` on fresh `kib` KiB threads in a child, since a SIGSEGV takes
    the whole process down."""
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
        threading.stack_size({kib} * 1024)
        """
    )
    body += textwrap.dedent(setup) + textwrap.dedent(loop)
    return subprocess.run(
        [sys.executable, "-c", body], capture_output=True, text=True, timeout=120
    )


@pytest.mark.parametrize(
    ("kib", "nest", "refusals"),
    [
        (32, 0, _STACK_REFUSALS),
        (40, 0, _STACK_REFUSALS),
        (32, 2, _STACK_REFUSALS),
        (40, 2, _STACK_REFUSALS),
        (64, 0, (_MEMORY_SLOT, _OVER_BUDGET)),
        (256, 0, (_MEMORY_SLOT,)),
        (512, 2, (_MEMORY_SLOT,)),
    ],
)
def test_any_refusal_fits_a_minimal_thread(kib, nest, refusals):
    """A refusal reaches the caller of a small thread, and `.backtrace`, read
    later on the main thread, still has frames. At 32 and 40 KiB a stack bound
    refuses before the walk reaches the memory slot; at 64 KiB either the
    budget or the slot answers, by build; at 256 KiB, 512 under two loads, the
    walk reaches the slot with room to spare in an unoptimised build too, and
    the refusal is the slot's own (a constant there never matches)."""
    out = _on_small_thread_child(
        kib,
        f"""\
        pat = p.load().mem(p.int_const(3))
        for _ in range({nest}):
            pat = p.load(pat)
        errs = []

        def run():
            try:
                fn.find_all(pat)
            except strider.StriderError as e:
                errs.append(e)
        """,
        """\
        t = threading.Thread(target=run)
        t.start()
        t.join()
        print("stack backtrace" in errs[0].backtrace.lower(), flush=True)
        print(errs[0], flush=True)
        """,
    )
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    has_frames, message = out.stdout.split("\n", 1)
    assert has_frames == "True", out.stdout
    assert message.startswith(refusals), out.stdout


@pytest.mark.parametrize(
    ("kib", "refusals"),
    [
        (32, _STACK_REFUSALS),
        (40, _STACK_REFUSALS),
        (64, (_OVER_BUDGET,)),
        (128, (_OVER_BUDGET,)),
    ],
)
@pytest.mark.parametrize("shape", ["value", "builder", "join"])
def test_the_stack_refusal_itself_fits_a_minimal_thread(kib, refusals, shape):
    """The refusal is raised on the stack the guard just declared spent, so
    building it must not resolve the Rust backtrace there, a SIGSEGV on a
    32 KiB thread. Each depth runs on a fresh thread until one refuses. Under
    48 KiB the outermost guard may refuse at once; from 64 KiB the walk starts
    in an unoptimised build too, and the budget is what refuses."""
    setup = f"""\
        a = p.Capture("A")

        def build(n):
            if {shape!r} == "value":
                pat = p.anything()
                for _ in range(n):
                    pat = p.int_add(pat, p.anything())
                return pat
            pat = p.anything().capture(a)
            for _ in range(n):
                pat = p.load(pat)
            return [pat, p.load(p.var(a))] if {shape!r} == "join" else pat

        def run(n, out):
            try:
                fn.find_all(build(n))
            except strider.StriderError as e:
                out.append(e)
        """
    loop = """\
        for n in range(1, 200):
            out = []
            t = threading.Thread(target=run, args=(n, out))
            t.start()
            t.join()
            if out:
                print("stack backtrace" in out[0].backtrace.lower(), flush=True)
                print(out[0], flush=True)
                break
        """
    out = _on_small_thread_child(kib, setup, loop)
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    has_frames, message = out.stdout.split("\n", 1)
    assert has_frames == "True", out.stdout
    assert message.startswith(refusals), out.stdout


@pytest.mark.parametrize("shape", ["value", "builder", "join", "store"])
def test_the_stack_refusal_fits_every_small_thread(shape):
    """What runs past the last check that passed, one lowering step and then
    the refusal, is a fixed cost rather than a share of the stack left, so a
    budget alone misses a window of thread sizes, different on each Python.
    Each size runs fresh threads, deeper each time, until one refuses."""
    setup = f"""\
        a = p.Capture("A")

        def build(n):
            if {shape!r} == "value":
                pat = p.anything()
                for _ in range(n):
                    pat = p.int_add(pat, p.anything())
                return pat
            if {shape!r} == "store":
                pat = p.store()
                for _ in range(n):
                    pat = p.store().mem(pat)
                return pat
            pat = p.anything().capture(a)
            for _ in range(n):
                pat = p.load(pat)
            return [pat, p.load(p.var(a))] if {shape!r} == "join" else pat

        def run(n, out):
            try:
                fn.find_all(build(n))
            except strider.StriderError as e:
                out.append(e)
        """
    loop = """\
        refused = []
        for kib in range(32, 129, 4):
            threading.stack_size(kib * 1024)
            for n in range(1, 200):
                out = []
                t = threading.Thread(target=run, args=(n, out))
                t.start()
                t.join()
                if out:
                    refused.append(kib)
                    break
        print(len(refused), flush=True)
        """
    out = _on_small_thread_child(32, setup, loop)
    assert out.returncode == 0, f"child exited {out.returncode}: stderr={out.stderr!r}"
    assert out.stdout.split() == [str(len(range(32, 129, 4)))], out.stdout
