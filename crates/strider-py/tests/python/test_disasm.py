"""Tests for the disassembly provenance surface.

Covers `Program.disasm(addr, count)`, the `strider.disassemble` /
`strider.disassemble_addrs` `#[pyfunction]`s, and the
`Analysis.fingerprint_text(node)` audit-trail companion to
`Analysis.fingerprint`.

These close the "audit trail" loop: a matched value node's fingerprint
(machine-instruction *addresses*) can be rendered to instruction text
without leaving strider for objdump.

Each test skips cleanly when the required fixture isn't built.
"""

from __future__ import annotations

import strider

from .conftest import fixture_path


def _load_memory():
    """Load the x86_64 `memory.elf` fixture (carries `array_sum`)."""
    elf = fixture_path("x64", "memory")
    return strider.load(str(elf))


# ── Program.disasm ──────────────────────────────────────────────────────


def test_disasm_returns_count_tuples_in_order():
    """`prog.disasm(entry, count=3)` returns exactly three
    `(int, str)` tuples in strictly-increasing address order."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    out = prog.disasm(entry, count=3)
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


def test_disasm_text_is_non_empty_for_pcode_instructions():
    """Instructions that lift to pcode carry non-empty text.

    The very first instruction of `array_sum` is `endbr64`, a CET
    NOP-like op that lifts to *zero* pcode ops (empty text) — that
    entry still exists and advances by its byte length.  Every
    subsequent instruction here lifts to real pcode, so its text is
    non-empty.
    """
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    out = prog.disasm(entry, count=3)
    # At least the 2nd and 3rd instructions (test / je) lift to pcode.
    assert out[1][1].strip(), f"empty text for {out[1][0]:#x}"
    assert out[2][1].strip(), f"empty text for {out[2][0]:#x}"


def test_disasm_default_count_is_one():
    """`disasm(addr)` with no count returns a single instruction."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    out = prog.disasm(entry)
    assert len(out) == 1
    assert out[0][0] == entry


# ── strider.disassemble / strider.disassemble_addrs ─────────────────────


def test_disassemble_pyfunction_matches_program_disasm():
    """The low-level `strider.disassemble(arch, mem, addr, count)`
    pyfunction produces the same result as `Program.disasm` for a
    non-interworking arch."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    mem = prog._elf.memory_map()
    low = strider.disassemble(prog.arch, mem, entry, 3)
    high = prog.disasm(entry, count=3)
    assert low == high


def test_disassemble_addrs_decodes_a_set_once():
    """`strider.disassemble_addrs` decodes a set of (non-sequential)
    addresses, one instruction each, in argument order."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    seq = prog.disasm(entry, count=3)
    # Feed the addresses back in reverse — the result must preserve the
    # *argument* order, and each entry must match the sequential decode.
    addrs = [a for a, _ in seq][::-1]
    mem = prog._elf.memory_map()
    out = strider.disassemble_addrs(prog.arch, mem, addrs)
    assert [a for a, _ in out] == addrs
    by_addr = dict(seq)
    for a, t in out:
        assert t == by_addr[a]


# ── Analysis.fingerprint_text ────────────────────────────────────────────


def test_fingerprint_text_renders_a_matched_node():
    """For a matched value node, `fingerprint_text(match)` returns
    `(addr, text)` pairs whose addresses equal `fingerprint(match)`
    and whose texts are non-empty, sorted by address."""
    prog = _load_memory()
    a = prog.analyze("array_sum")
    matches = a.find(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    )
    assert matches, "expected at least one Add node in array_sum"
    m = matches[0]

    fp = a.fingerprint(m)
    assert fp, "matched Add node must carry a non-empty fingerprint"

    ft = a.fingerprint_text(m)
    assert isinstance(ft, list)
    # Addresses (sorted) equal the fingerprint addresses.
    assert [addr for addr, _ in ft] == sorted(fp)
    # Sorted by address.
    assert [addr for addr, _ in ft] == sorted(addr for addr, _ in ft)
    # An Add node lifts from a real arithmetic instruction, so its
    # disassembly text is non-empty.
    for addr, text in ft:
        assert isinstance(addr, int)
        assert isinstance(text, str)
        assert text.strip(), f"empty disassembly text for {addr:#x}"


def test_fingerprint_text_accepts_root_id_and_node():
    """`fingerprint_text` accepts a raw id, a Match, and a Node handle
    interchangeably (mirrors `fingerprint`'s coercion)."""
    prog = _load_memory()
    a = prog.analyze("array_sum")
    matches = a.find(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    )
    assert matches
    m = matches[0]
    via_match = a.fingerprint_text(m)
    via_id = a.fingerprint_text(m.root)
    via_node = a.fingerprint_text(a.function.node(m.root))
    assert via_match == via_id == via_node


def test_fingerprint_text_empty_for_structural_node():
    """A structural node (no fingerprint, e.g. Entry) yields `[]`."""
    prog = _load_memory()
    a = prog.analyze("array_sum")
    fn = a.function
    struct_id = None
    for nid in fn.node_ids():
        if not fn.asm_fingerprint(nid):
            struct_id = nid
            break
    assert struct_id is not None, "expected at least one structural node"
    assert a.fingerprint_text(struct_id) == []
