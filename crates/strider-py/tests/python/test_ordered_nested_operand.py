"""`.ordered()` must be nestable below the pattern root.

Regression: `.ordered()` used to eagerly seal its builder into a finished
`Pattern`, which the operand-nesting check then rejected ("a finished control /
variadic pattern cannot be nested as a value operand").  It now stays a lazy
builder and composes exactly like the bare commutative builder.
"""

import strider
from strider import pattern as p


def _shl_add_fn():
    # An Add with distinguishable operands (one const, one not) so operand
    # ORDER is observable.
    code = bytes([
        0x83, 0xc7, 0x07,  # add edi, 7        -> Add(edi, 7)
        0x89, 0xf8,        # mov eax, edi
        0xc3,              # ret
    ])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    return fn


def _store_and_fn():
    #   and edi, 0xf
    #   mov [rsi], edi     -> Store(data=And(edi, 0xf))
    #   ret
    code = bytes([
        0x83, 0xe7, 0x0f,  # and edi, 0xf
        0x89, 0x3e,        # mov [rsi], edi
        0xc3,              # ret
    ])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    return fn


def test_ordered_builds_and_matches_as_a_value_operand():
    """User-reported repro: an `.ordered()` binary op nested in a
    `store(data=...)` slot used to raise StriderError at seal time."""
    fn = _store_and_fn()
    pat = p.store(data=p.int_binary("And", p.anything(), p.anything()).ordered())
    assert len(fn.find_all(pat)) >= 1


def test_nested_ordered_actually_enforces_operand_order():
    """`.ordered()` below the root must still disable commutativity, not just
    survive nesting: the swapped spelling matches only via the bare builder."""
    fn = _store_and_fn()

    # Canonical IR order for `and edi, 0xf` is And(<value>, IntConst(0xf)).
    canonical = p.store(
        data=p.int_binary("And", p.anything(), p.any_int_const()).ordered()
    )
    swapped = p.store(
        data=p.int_binary("And", p.any_int_const(), p.anything()).ordered()
    )
    commutative = p.store(
        data=p.int_binary("And", p.any_int_const(), p.anything())
    )

    n_canonical = len(fn.find_all(canonical))
    n_swapped = len(fn.find_all(swapped))
    n_commutative = len(fn.find_all(commutative))

    # Exactly one ordered spelling fires (whichever matches the stored operand
    # order); the non-ordered form fires either way round.
    assert (n_canonical > 0) != (n_swapped > 0)
    assert n_commutative > 0


def test_ordered_at_root_still_works():
    """Root-level `.ordered()` must keep working after the lazy-builder fix."""
    fn = _shl_add_fn()
    assert len(fn.find_all(p.int_binary("Add", p.anything(), p.anything()).ordered())) >= 1


def test_ordered_is_chainable_with_capture():
    """Staying lazy means `.ordered()` composes with the other chainable verbs
    instead of terminating the chain."""
    fn = _store_and_fn()
    c = p.Capture()
    hits = fn.find_all(
        p.store(data=p.int_binary("And", p.anything(), p.anything()).ordered().capture(c))
    )
    assert len(hits) >= 1
    assert hits[0].node(c) is not None
