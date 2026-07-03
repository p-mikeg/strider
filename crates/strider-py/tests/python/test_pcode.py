"""Tests for the p-code provenance surface.

Covers `ElfLifter.pcode(addr, count)`, the `strider.pcode_at` /
`strider.pcode_at_addrs` `#[pyfunction]`s, and the
`Lifter.fingerprint_pcode(node)` audit-trail companion to
`Node.fingerprint()` (also reachable via `Match.asm_fingerprint(key)`).

These close the "audit trail" loop: a matched value node's fingerprint
(machine-instruction *addresses*) can be lifted to p-code text without
leaving strider.  rsleigh is a p-code lifter — the returned text is the
lifted semantics, NOT native assembly mnemonics.

`fingerprint_pcode` lives on `Lifter` (not `Function`/`Node`) because it
needs a `Sleigh` to render an address to p-code text; an `ElfLifter` IS a
`Lifter`, so calling it on `prog` below is available directly.  It
builds its OWN fresh, throwaway Sleigh per call rather than lifting
through the Lifter's persistent one, so it never perturbs a later
`analyze()`/`build_cfg()` call on the same handle (see
`test_fingerprint_pcode_does_not_perturb_a_following_analyze`).

Each test skips cleanly when the required fixture isn't built.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def _load_memory():
    """Load the x86_64 `memory.elf` fixture (carries `array_sum`)."""
    elf = fixture_path("x64", "memory")
    return strider.load_elf(str(elf))


# ── ElfLifter.pcode ─────────────────────────────────────────────────────


def test_pcode_returns_count_tuples_in_order():
    """`prog.pcode(entry, count=3)` returns exactly three
    `(int, str)` tuples in strictly-increasing address order."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    out = prog.pcode(entry, count=3)
    assert isinstance(out, list)
    assert len(out) == 3
    for addr, text in out:
        assert isinstance(addr, int)
        assert isinstance(text, str)
    # First entry is the function entry; addresses increase by each
    # instruction's machine byte length.
    assert out[0][0] == entry
    addrs = [a for a, _ in out]
    assert addrs == sorted(addrs)
    assert len(set(addrs)) == 3, "addresses must be distinct"


def test_pcode_text_is_non_empty_for_pcode_instructions():
    """Instructions that lift to p-code carry non-empty text.

    The very first instruction of `array_sum` is `endbr64`, a CET
    NOP-like op that lifts to *zero* p-code ops (empty text) — that
    entry still exists and advances by its byte length.  Every
    subsequent instruction here lifts to real p-code, so its text is
    non-empty.
    """
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    out = prog.pcode(entry, count=3)
    # At least the 2nd and 3rd instructions (test / je) lift to p-code.
    assert out[1][1].strip(), f"empty text for {out[1][0]:#x}"
    assert out[2][1].strip(), f"empty text for {out[2][0]:#x}"


def test_pcode_default_count_is_one():
    """`pcode(addr)` with no count returns a single instruction."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    out = prog.pcode(entry)
    assert len(out) == 1
    assert out[0][0] == entry


# ── strider.pcode_at / strider.pcode_at_addrs ───────────────────────────


def test_pcode_at_pyfunction_matches_elf_strider_pcode():
    """The low-level `strider.pcode_at(arch, mem, addr, count)`
    pyfunction produces the same result as `ElfLifter.pcode` for a
    non-interworking arch."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    mem = prog._elf.reader()
    low = strider.pcode_at(prog.arch, mem, entry, 3)
    high = prog.pcode(entry, count=3)
    assert low == high


def test_pcode_at_addrs_decodes_a_set_once():
    """`strider.pcode_at_addrs` lifts a set of (non-sequential)
    addresses, one instruction each, in argument order."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    seq = prog.pcode(entry, count=3)
    # Feed the addresses back in reverse — the result must preserve the
    # *argument* order, and each entry must match the sequential decode.
    addrs = [a for a, _ in seq][::-1]
    mem = prog._elf.reader()
    out = strider.pcode_at_addrs(prog.arch, mem, addrs)
    assert [a for a, _ in out] == addrs
    by_addr = dict(seq)
    for a, t in out:
        assert t == by_addr[a]


# ── Lifter.fingerprint_pcode ─────────────────────────────────────────────


def test_fingerprint_pcode_renders_a_matched_node():
    """For a matched value node, `fingerprint_pcode(node)` returns
    `(addr, text)` pairs whose addresses equal `node.fingerprint()`
    and whose texts are non-empty, sorted by address."""
    prog = _load_memory()
    _cfg, function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "expected at least one Add node in array_sum"
    node = function.node(matches[0].root)

    fp = node.fingerprint()
    assert fp, "matched Add node must carry a non-empty fingerprint"

    fpc = prog.fingerprint_pcode(node)
    assert isinstance(fpc, list)
    # Addresses (sorted) equal the fingerprint addresses.
    assert [addr for addr, _ in fpc] == sorted(fp)
    # Sorted by address.
    assert [addr for addr, _ in fpc] == sorted(addr for addr, _ in fpc)
    # An Add node lifts from a real arithmetic instruction, so its
    # p-code text is non-empty.
    for addr, text in fpc:
        assert isinstance(addr, int)
        assert isinstance(text, str)
        assert text.strip(), f"empty p-code text for {addr:#x}"


def test_fingerprint_pcode_stable_across_separately_constructed_nodes():
    """Two separately-constructed `Node` handles for the same id produce
    identical `fingerprint_pcode` output — `Lifter.fingerprint_pcode`
    only cares about the (function, node id) the `Node` carries, not
    `Node` object identity."""
    prog = _load_memory()
    _cfg, function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches
    root = matches[0].root
    via_a = prog.fingerprint_pcode(function.node(root))
    via_b = prog.fingerprint_pcode(function.node(root))
    assert via_a == via_b


def test_fingerprint_pcode_empty_for_structural_node():
    """A structural node (no fingerprint, e.g. Entry) yields `[]`."""
    prog = _load_memory()
    _cfg, function, _unresolved = prog.analyze("array_sum")
    struct_id = None
    for nid in function.node_ids():
        if not function.node(nid).fingerprint():
            struct_id = nid
            break
    assert struct_id is not None, "expected at least one structural node"
    assert prog.fingerprint_pcode(function.node(struct_id)) == []


def test_fingerprint_pcode_accepts_node_match_or_raw_id():
    """`fingerprint_pcode` accepts a `Node`, a `Match`, or a raw `int`
    node id (paired with `function=`) — the same three-way acceptance
    the old `Analysis.fingerprint_pcode` gave via `_coerce_node_id`."""
    prog = _load_memory()
    _cfg, function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches
    match = matches[0]
    node = function.node(match.root)

    via_node = prog.fingerprint_pcode(node)
    via_match = prog.fingerprint_pcode(match)
    via_id = prog.fingerprint_pcode(match.root, function=function)
    assert via_node == via_match == via_id


def test_fingerprint_pcode_raw_id_without_function_raises():
    """A raw int node id with no `function=` is ambiguous — a `Lifter`
    can `analyze` many different functions over its lifetime, so there
    is no implicit "current function" to resolve the id against."""
    prog = _load_memory()
    _cfg, function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches
    with pytest.raises(ValueError):
        prog.fingerprint_pcode(matches[0].root)


def test_fingerprint_pcode_does_not_perturb_a_following_analyze():
    """Regression: `fingerprint_pcode` must lift through its own fresh,
    throwaway Sleigh rather than the `Lifter`'s persistent one — the
    persistent Sleigh's `lift_one` carries context-register state across
    calls (see CLAUDE.md's "External Dependency: rsleigh"), so lifting a
    node's fingerprint addresses (visited in sorted, not decode, order)
    through it would both mis-render the p-code AND leave the persistent
    Sleigh's context wherever the last address left it — corrupting a
    later `analyze()`/`build_cfg()` call on the same handle.  This pins
    that a `fingerprint_pcode` call sandwiched between two `analyze()`
    calls on the same entry doesn't change the second result."""
    prog = _load_memory()

    _cfg, function_before, unresolved_before = prog.analyze("array_sum")
    baseline_dot = function_before.raw_dot_str()
    matches = function_before.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches

    # Fingerprinted node ids, each with its own fingerprint address(es);
    # walked in REVERSE node-id order (not asm decode order) through
    # `fingerprint_pcode`, exercising all three accepted argument forms,
    # between the two `analyze()` calls.
    fingerprinted_ids = [
        nid
        for nid in function_before.node_ids()
        if function_before.node(nid).fingerprint()
    ]
    assert len(fingerprinted_ids) >= 2
    for nid in reversed(fingerprinted_ids):
        prog.fingerprint_pcode(nid, function=function_before)
    prog.fingerprint_pcode(matches[0])
    prog.fingerprint_pcode(function_before.node(matches[0].root))

    _cfg, function_after, unresolved_after = prog.analyze("array_sum")
    assert function_after.raw_dot_str() == baseline_dot
    assert unresolved_after == unresolved_before
