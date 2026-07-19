"""Tests for the p-code provenance surface.

p-code has two homes with deliberately different semantics: `Cfg.pcode_at` /
`Cfg.fingerprint_pcode` look up an already-built CFG's stored decodes, while
`Lifter.pcode_at(entry, addr)` re-decodes via a linear sweep from `entry`,
replaying context-register state, and so reaches addresses outside any
analysed CFG.  The returned text is lifted p-code semantics, not native
assembly mnemonics.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def _load_memory():
    """The x86_64 `memory.elf` fixture, which carries `array_sum`."""
    elf = fixture_path("x64", "memory")
    return strider.lift.load_elf(str(elf))


def test_cfg_pcode_at_matches_decoded_instructions():
    """Text for an address the CFG decoded to real p-code, `None` otherwise."""
    prog = _load_memory()
    cfg, function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "expected at least one Add node in array_sum"
    addr = sorted(function.node(matches[0].root).asm_fingerprint())[0]

    text = cfg.pcode_at(addr)
    assert text is not None
    assert isinstance(text, str)

    assert cfg.pcode_at(addr + 0x10000) is None
    assert function.node_count() > 0


def test_cfg_pcode_at_returns_none_for_a_zero_pcode_op_instruction():
    """A CFG stores one record per decoded p-code op, so an instruction that
    lifts to zero ops (here the `endbr64` landing pad at entry) leaves no
    trace and `pcode_at` returns `None`, indistinguishable from an address
    never decoded.  `Lifter.pcode_at` re-decodes and can tell them apart;
    see `test_lifter_pcode_at_returns_empty_text_at_entry`.
    """
    prog = _load_memory()
    cfg, _function, _unresolved = prog.analyze("array_sum")
    entry = prog.symbol("array_sum")
    assert cfg.pcode_at(entry) is None


def test_fingerprint_pcode_renders_a_matched_node():
    """`fingerprint_pcode` returns `(addr, text)` pairs drawn from the node's
    own fingerprint, sorted by address, with non-empty text."""
    prog = _load_memory()
    cfg, function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "expected at least one Add node in array_sum"
    node = function.node(matches[0].root)

    fp = node.asm_fingerprint()
    assert fp, "matched Add node must carry a non-empty fingerprint"

    fpc = cfg.fingerprint_pcode(node)
    assert isinstance(fpc, list)
    assert set(addr for addr, _ in fpc) <= set(fp)
    assert [addr for addr, _ in fpc] == sorted(addr for addr, _ in fpc)
    # An Add lifts from a real arithmetic instruction, so text is non-empty.
    for addr, text in fpc:
        assert isinstance(addr, int)
        assert isinstance(text, str)
        assert text.strip(), f"empty p-code text for {addr:#x}"
        assert cfg.pcode_at(addr) == text


def test_fingerprint_pcode_stable_across_separately_constructed_nodes():
    """Output depends on the (function, node id) a `Node` carries, not on
    `Node` object identity."""
    prog = _load_memory()
    cfg, function, _unresolved = prog.analyze("array_sum")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches
    root = matches[0].root
    via_a = cfg.fingerprint_pcode(function.node(root))
    via_b = cfg.fingerprint_pcode(function.node(root))
    assert via_a == via_b


def test_fingerprint_pcode_empty_for_structural_node():
    """A structural node (no fingerprint, e.g. Entry) yields `[]`."""
    prog = _load_memory()
    cfg, function, _unresolved = prog.analyze("array_sum")
    struct_id = None
    for nid in function.node_ids():
        if not function.node(nid).asm_fingerprint():
            struct_id = nid
            break
    assert struct_id is not None, "expected at least one structural node"
    assert cfg.fingerprint_pcode(function.node(struct_id)) == []


def test_lifter_pcode_at_returns_empty_text_at_entry():
    """The `endbr64` at entry lifts to zero p-code ops.  Because the sweep
    re-decodes, it can represent that as `""`, where the CFG lookup has no
    record of the instruction at all and returns `None`.
    """
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    assert prog.pcode_at(entry, entry) == ""


def test_lifter_pcode_at_matches_cfg_lookup_for_a_real_pcode_address():
    """The sweep must agree exactly with the CFG's stored decode.  The `add`
    fixture is straight-line, so its first fingerprinted address is reachable
    by a pure linear sweep with no context-register divergence.
    """
    elf = fixture_path("x64", "arithmetic")
    prog = strider.lift.load_elf(str(elf))
    entry = prog.symbol("add")
    cfg, function, _unresolved = prog.analyze("add")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "expected at least one Add node in add"
    addr = sorted(function.node(matches[0].root).asm_fingerprint())[0]

    swept = prog.pcode_at(entry, addr)
    looked_up = cfg.pcode_at(addr)
    assert looked_up is not None
    assert swept == looked_up


def test_lifter_pcode_at_rejects_addr_before_entry():
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    with pytest.raises(strider.StriderError):
        prog.pcode_at(entry, entry - 4)


def test_lifter_pcode_at_rejects_misaligned_target():
    """An addr off the instruction boundaries raises rather than silently
    returning the enclosing instruction's text."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    with pytest.raises(strider.StriderError):
        prog.pcode_at(entry, entry + 1)
