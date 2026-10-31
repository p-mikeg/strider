"""A ppc64 ELFv2 `bl` to a function sharing the caller's TOC enters at its
local entry, past the global entry `Symbol.address` names. `ElfLifter.analyze`
matches a `no_return_targets` address or `per_address_ccs` key at the local
entry of the global entry it lists, so a table keyed by symbol still matches
those calls.

The image is `fixtures/loader/out/ppc64le_local_entry.elf`, built by
`make -C fixtures/loader` from `src/ppc64le_local_entry.s`.
"""

from __future__ import annotations

import pytest
import strider
from strider.cfg import CfgOptions
from strider.lift import LifterOptions
from strider.pattern import call, function_arg, ret
from strider.sleigh import CallingConvention

from .conftest import WORKSPACE_ROOT, fixture_path

# `noret_call`: `bl stop+8` at 0x100000b0, its `nop` at 0x100000b4, reached
# only by falling through the call, then the join at 0x100000b8 the
# `beq` also reaches.
NOP_AFTER_STOP = 0x100000B4
JOIN = 0x100000B8


def _image(name: str) -> str:
    path = WORKSPACE_ROOT / "fixtures" / "loader" / "out" / name
    if not path.exists():
        pytest.fail(f"fixture missing: {path} (run `make -C fixtures/loader`)")
    return str(path)


@pytest.fixture(scope="module")
def elf():
    return strider.lift.load_elf(_image("ppc64le_local_entry.elf"))


def test_local_entry_is_the_st_other_offset(elf):
    for name in ("stop", "hook", "noret_call", "relay"):
        sym = elf.symbol(name)
        assert sym.local_entry == sym.address + 8, name
    sink = elf.symbol("sink")
    assert sink.local_entry == sink.address


def test_local_entry_is_the_address_off_ppc64():
    x86 = strider.lift.load_elf(str(fixture_path("x86", "calls")))
    sym = x86.symbol("pair_a")
    assert sym.local_entry == sym.address
    thumb = strider.lift.load_elf(str(fixture_path("arm_thumb", "calls")))
    sym = thumb.symbol("pair_a")
    assert sym.is_thumb and sym.local_entry == sym.address
    x86.add_symbols({"made_up": 0x1234})
    assert x86.symbol("made_up").local_entry == 0x1234


def test_a_no_return_symbol_ends_a_call_to_its_local_entry(elf):
    plain = elf.analyze("noret_call")
    assert plain.cfg.region_at(NOP_AFTER_STOP) is not None

    stop = elf.symbol("stop").address
    opts = LifterOptions(cfg=CfgOptions(no_return_targets=[stop]))
    for entry in ("noret_call", elf.symbol("noret_call").address):
        marked = elf.analyze(entry, opts=opts)
        assert marked.cfg.region_at(NOP_AFTER_STOP) is None, entry
        assert marked.cfg.region_at(JOIN) is not None, entry
        assert marked.cfg.incomplete == [], entry
    # The caller's own table is left as given.
    assert opts.cfg.no_return_targets == frozenset({stop})


def test_build_cfg_ends_a_call_to_a_no_return_symbols_local_entry(elf):
    entry = elf.symbol("noret_call").address
    assert elf.build_cfg(entry).region_at(NOP_AFTER_STOP) is not None
    stop = elf.symbol("stop").address
    cfg = elf.build_cfg(entry, CfgOptions(no_return_targets=[stop]))
    assert cfg.region_at(NOP_AFTER_STOP) is None
    assert cfg.region_at(JOIN) is not None
    assert cfg.incomplete == []


def test_build_cfg_seats_the_elfs_data_ranges_as_analyze_does():
    """`_start` falls through into a `$d` pool, which only the mapping symbols
    mark as data."""
    arm = strider.lift.load_elf(str(fixture_path("arm", "calls")))
    start = arm.symbol("_start").address
    cfg = arm.build_cfg(start)
    assert [r for _, r in cfg.incomplete] == ["undecodable_target"]
    assert cfg.incomplete == arm.analyze(start).cfg.incomplete


def test_a_symbol_file_added_later_widens_the_next_analyse():
    """A stripped image has no symbols to widen by; the symbol file brings
    them, and the same options then match the local entry."""
    stripped = strider.lift.load_elf(_image("ppc64le_local_entry_stripped.elf"))
    full = strider.lift.load_elf(_image("ppc64le_local_entry.elf"))
    entry, stop = full.symbol("noret_call").address, full.symbol("stop").address
    opts = LifterOptions(cfg=CfgOptions(no_return_targets=[stop]))
    assert stripped.analyze(entry, opts=opts).cfg.region_at(NOP_AFTER_STOP) is not None
    stripped.add_symbol_file(_image("ppc64le_local_entry.elf"))
    assert stripped.analyze(entry, opts=opts).cfg.region_at(NOP_AFTER_STOP) is None


def test_per_address_ccs_keyed_by_a_global_entry_reaches_the_local_call(elf):
    """`relay` passes r4 through `hook` to `sink`: by default `hook` clobbers
    r4, and an all-preserving override keyed at `hook`'s symbol keeps it."""
    sink_arg1 = call().target(elf.symbol("sink").address).arg(1, function_arg(1))
    assert not elf.analyze("relay").function.find_all(sink_arg1)

    hook = elf.symbol("hook").address
    opts = LifterOptions(per_address_ccs={hook: elf.cc.preserves_all()})
    assert len(elf.analyze("relay", opts=opts).function.find_all(sink_arg1)) == 1
    assert opts.per_address_ccs is not None and set(opts.per_address_ccs) == {hook}


def test_a_tail_call_to_a_local_entry_takes_the_global_entrys_override(elf):
    """`tail_relay` branches to `hook`'s local entry: an all-preserving
    override keyed at `hook`'s symbol leaves r3 as it came in."""
    r3_kept = ret().ret_val(0, function_arg(0))
    assert not elf.analyze("tail_relay").function.find_all(r3_kept)
    hook = elf.symbol("hook").address
    opts = LifterOptions(per_address_ccs={hook: elf.cc.preserves_all()})
    assert len(elf.analyze("tail_relay", opts=opts).function.find_all(r3_kept)) == 1


def test_a_tail_call_to_a_local_entry_clobbers_what_its_override_does(elf):
    """An override keyed at `hook`'s symbol that clobbers r14, which
    `tail_relay` reads and the default preserves, lifts the tail call."""
    clobbers_r14 = CallingConvention.custom(
        elf.arch, ["r3"], ["r2", "LR"], ["r3"], [], "r1", 96, 8, 0, link_register="LR"
    )
    opts = LifterOptions(per_address_ccs={elf.symbol("hook").address: clobbers_r14})
    fn = elf.analyze("tail_relay", opts=opts).function
    assert len(fn.find_all(call().target(elf.symbol("hook").local_entry))) == 1


def test_two_globals_sharing_a_local_entry_answer_by_the_lower(elf):
    """`hook_alt` is 4 bytes into `hook` and shares its local entry, where
    `relay` calls: the lower global's override answers there, whatever order
    the table iterates in."""
    sink_arg1 = call().target(elf.symbol("sink").address).arg(1, function_arg(1))
    hook, alt = elf.symbol("hook"), elf.symbol("hook_alt")
    assert alt.local_entry == hook.local_entry and alt.address == hook.address + 4
    for _ in range(16):
        opts = LifterOptions(
            per_address_ccs={hook.address: elf.cc.preserves_all(), alt.address: elf.cc}
        )
        assert len(elf.analyze("relay", opts=opts).function.find_all(sink_arg1)) == 1


def test_a_plain_lifter_takes_the_local_entry_as_given(elf):
    """A plain `Lifter` has no symbol table: the global entry matches no call,
    and `Symbol.local_entry` is the key that does."""
    lifter = strider.lift.lifter(elf.arch, elf.reader())
    entry = elf.symbol("noret_call").address
    stop = elf.symbol("stop")
    by_symbol = lifter.build_cfg(entry, CfgOptions(no_return_targets=[stop.address]))
    assert by_symbol.region_at(NOP_AFTER_STOP) is not None
    by_local = lifter.build_cfg(entry, CfgOptions(no_return_targets=[stop.local_entry]))
    assert by_local.region_at(NOP_AFTER_STOP) is None
