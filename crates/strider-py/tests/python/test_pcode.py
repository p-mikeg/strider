"""Tests for the p-code provenance surface.

Covers `ElfLifter.pcode(addr, count)`, the `strider.pcode_at` /
`strider.pcode_at_addrs` `#[pyfunction]`s, and the
`Lifter.fingerprint_pcode(node)` audit-trail companion to
`Node.fingerprint()` / `Function.asm_fingerprint(id)`.

These close the "audit trail" loop: a matched value node's fingerprint
(machine-instruction *addresses*) can be lifted to p-code text without
leaving strider.  rsleigh is a p-code lifter — the returned text is the
lifted semantics, NOT native assembly mnemonics.

`fingerprint_pcode` lives on `Lifter` (not `Function`/`Node`) because it
needs a `Sleigh` to render an address to p-code text; an `ElfLifter` IS a
`Lifter`, so calling it on `prog` below reuses the same Sleigh instance
that produced the function.

Each test skips cleanly when the required fixture isn't built.
"""

from __future__ import annotations

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
    function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
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
    function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    )
    assert matches
    root = matches[0].root
    via_a = prog.fingerprint_pcode(function.node(root))
    via_b = prog.fingerprint_pcode(function.node(root))
    assert via_a == via_b


def test_fingerprint_pcode_empty_for_structural_node():
    """A structural node (no fingerprint, e.g. Entry) yields `[]`."""
    prog = _load_memory()
    function, _unresolved = prog.analyze("array_sum")
    struct_id = None
    for nid in function.node_ids():
        if not function.asm_fingerprint(nid):
            struct_id = nid
            break
    assert struct_id is not None, "expected at least one structural node"
    assert prog.fingerprint_pcode(function.node(struct_id)) == []
