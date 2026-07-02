"""Tests for the p-code provenance surface.

Covers `ElfLifter.pcode(addr, count)`, the `strider.pcode_at` /
`strider.pcode_at_addrs` `#[pyfunction]`s, and the
`Analysis.fingerprint_pcode(node)` audit-trail companion to
`Analysis.fingerprint`.

These close the "audit trail" loop: a matched value node's fingerprint
(machine-instruction *addresses*) can be lifted to p-code text without
leaving strider.  rsleigh is a p-code lifter — the returned text is the
lifted semantics, NOT native assembly mnemonics.

Each test skips cleanly when the required fixture isn't built.
"""

from __future__ import annotations

import strider

from .conftest import fixture_path


def _load_memory():
    """Load the x86_64 `memory.elf` fixture (carries `array_sum`)."""
    elf = fixture_path("x64", "memory")
    return strider.load_elf(str(elf))


def _analyze_wrapped(prog, name: str) -> strider.Analysis:
    """Wrap `ElfLifter.analyze(name)`'s `(Function, unresolved)` tuple in
    an `Analysis` — `analyze()` itself no longer constructs one
    automatically, so the `fingerprint_pcode` tests below (which need
    the mem + effective-arch binding `Analysis` carries) build it here."""
    addr = prog.symbol(name)
    function, unresolved = prog.analyze(name)
    return strider.Analysis(
        function,
        entry=addr,
        name=name,
        effective_arch=prog.arch,
        mem=prog.reader(),
        unresolved_indirect_branches=unresolved,
    )


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


# ── Analysis.fingerprint_pcode ───────────────────────────────────────────


def test_fingerprint_pcode_renders_a_matched_node():
    """For a matched value node, `fingerprint_pcode(match)` returns
    `(addr, text)` pairs whose addresses equal `fingerprint(match)`
    and whose texts are non-empty, sorted by address."""
    prog = _load_memory()
    a = _analyze_wrapped(prog, "array_sum")
    matches = a.find(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    )
    assert matches, "expected at least one Add node in array_sum"
    m = matches[0]

    fp = a.fingerprint(m)
    assert fp, "matched Add node must carry a non-empty fingerprint"

    fpc = a.fingerprint_pcode(m)
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


def test_fingerprint_pcode_accepts_root_id_and_node():
    """`fingerprint_pcode` accepts a raw id, a Match, and a Node handle
    interchangeably (mirrors `fingerprint`'s coercion)."""
    prog = _load_memory()
    a = _analyze_wrapped(prog, "array_sum")
    matches = a.find(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    )
    assert matches
    m = matches[0]
    via_match = a.fingerprint_pcode(m)
    via_id = a.fingerprint_pcode(m.root)
    via_node = a.fingerprint_pcode(a.function.node(m.root))
    assert via_match == via_id == via_node


def test_fingerprint_pcode_empty_for_structural_node():
    """A structural node (no fingerprint, e.g. Entry) yields `[]`."""
    prog = _load_memory()
    a = _analyze_wrapped(prog, "array_sum")
    fn = a.function
    struct_id = None
    for nid in fn.node_ids():
        if not fn.asm_fingerprint(nid):
            struct_id = nid
            break
    assert struct_id is not None, "expected at least one structural node"
    assert a.fingerprint_pcode(struct_id) == []
