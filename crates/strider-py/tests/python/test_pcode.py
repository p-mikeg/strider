"""Tests for the p-code provenance surface.

p-code has two homes: `Cfg.pcode_at` / `Cfg.fingerprint_pcode` (an exact
LOOKUP against an already-built CFG's stored decodes — the audit-trail
companion to `Node.asm_fingerprint()`, also reachable via
`Match.asm_fingerprint(key)`), and `Lifter.pcode_at(entry, addr)` (a
stand-alone linear sweep from `entry`, replaying context-register state,
for addresses outside any analysed CFG).

`Cfg.fingerprint_pcode` closes the "audit trail" loop: a matched value
node's fingerprint (machine-instruction *addresses*) can be lifted to
p-code text without leaving strider.  rsleigh is a p-code lifter — the
returned text is the lifted semantics, NOT native assembly mnemonics.

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


# ── Cfg.pcode_at / Cfg.fingerprint_pcode ─────────────────────────────────


def test_cfg_pcode_at_matches_decoded_instructions():
    """`cfg.pcode_at(addr)` returns non-`None` text for a machine
    address the CFG actually decoded to real p-code, and `None` for one
    it didn't."""
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

    # An address far outside any decoded region is absent.
    assert cfg.pcode_at(addr + 0x10000) is None
    assert function.node_count() > 0


def test_cfg_pcode_at_returns_none_for_a_zero_pcode_op_instruction():
    """A machine instruction that lifts to ZERO p-code ops (e.g. the
    `endbr64` CET landing pad at a function's entry) has no
    `RegionInstruction` at all in the CFG — `strider_cfg::Region` only
    stores one entry per decoded p-code OP, so a zero-op instruction
    leaves no trace.  `Cfg.pcode_at` therefore returns `None` for it,
    indistinguishable from an address this CFG never decoded at all.

    `Lifter.pcode_at`, which re-decodes rather than looking up, still
    returns the (empty) text for the same address — see the next test."""
    prog = _load_memory()
    cfg, _function, _unresolved = prog.analyze("array_sum")
    entry = prog.symbol("array_sum")
    assert cfg.pcode_at(entry) is None


def test_fingerprint_pcode_renders_a_matched_node():
    """For a matched value node, `cfg.fingerprint_pcode(node)` returns
    `(addr, text)` pairs whose addresses are drawn from
    `node.asm_fingerprint()` and whose texts are non-empty, sorted by
    address."""
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
    # Every returned address is one of the node's fingerprint addresses.
    assert set(addr for addr, _ in fpc) <= set(fp)
    # Sorted by address.
    assert [addr for addr, _ in fpc] == sorted(addr for addr, _ in fpc)
    # An Add node lifts from a real arithmetic instruction, so its
    # p-code text is non-empty.
    for addr, text in fpc:
        assert isinstance(addr, int)
        assert isinstance(text, str)
        assert text.strip(), f"empty p-code text for {addr:#x}"
        assert cfg.pcode_at(addr) == text


def test_fingerprint_pcode_stable_across_separately_constructed_nodes():
    """Two separately-constructed `Node` handles for the same id produce
    identical `fingerprint_pcode` output — `Cfg.fingerprint_pcode` only
    cares about the (function, node id) the `Node` carries, not `Node`
    object identity."""
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


# ── Lifter.pcode_at (entry-relative linear sweep) ────────────────────────


def test_lifter_pcode_at_returns_empty_text_at_entry():
    """`lifter.pcode_at(entry, entry)` — the first swept instruction, the
    `endbr64` CET landing pad — returns `""` (zero p-code ops), matching
    `Cfg.pcode_at`'s documented `None` for the same address (see
    `test_cfg_pcode_at_returns_none_for_a_zero_pcode_op_instruction`):
    `Lifter.pcode_at` re-decodes, so it CAN represent "zero ops",
    whereas the CFG lookup has no record of the instruction at all."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    assert prog.pcode_at(entry, entry) == ""


def test_lifter_pcode_at_matches_cfg_lookup_for_a_real_pcode_address():
    """For a machine address that decodes to real p-code (not a
    zero-op instruction), a linear sweep from `entry` agrees exactly
    with the CFG's own stored decode of the same address — the
    straight-line-arithmetic `add` fixture has no branches, so its
    first fingerprinted address is reachable via a pure linear sweep
    from entry with no context-register divergence."""
    elf = fixture_path("x64", "arithmetic")
    prog = strider.load_elf(str(elf))
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
    """An `addr` that isn't a machine-instruction boundary on the linear
    path from `entry` raises rather than silently returning the
    enclosing instruction's text."""
    prog = _load_memory()
    entry = prog.symbol("array_sum")
    with pytest.raises(strider.StriderError):
        prog.pcode_at(entry, entry + 1)
