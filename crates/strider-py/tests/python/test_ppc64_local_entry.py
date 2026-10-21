"""A ppc64 ELFv2 `bl` to a function sharing the caller's TOC enters at its
local entry, past the global entry `Symbol.address` names. `ElfLifter.analyze`
extends `no_return_targets` and `per_address_ccs` with the local entry of every
listed global entry, so a table keyed by symbol still matches those calls.

The image is `fixtures/loader/out/ppc64le_local_entry.elf`, built by
`make -C fixtures/loader` from `src/ppc64le_local_entry.s`.
"""

from __future__ import annotations

import pytest
import strider
from strider.cfg import CfgOptions
from strider.lift import LifterOptions
from strider.pattern import call, function_arg

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


def test_the_widened_tables_are_built_once(elf):
    """Every analyse derives its options afresh; the widened tables are
    shared by all of them rather than rebuilt per call."""
    stop, hook = elf.symbol("stop"), elf.symbol("hook")
    opts = LifterOptions(
        cfg=CfgOptions(no_return_targets=[stop.address]),
        per_address_ccs={hook.address: elf.cc.preserves_all()},
    )
    first, second = opts._with_elf(elf._elf), opts._with_elf(elf._elf)
    assert first.cfg.no_return_targets == {stop.address, stop.local_entry}
    assert first.cfg.no_return_targets is second.cfg.no_return_targets
    assert set(first.per_address_ccs or {}) == {hook.address, hook.local_entry}


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


def test_the_widened_per_address_table_is_built_once(elf):
    hook = elf.symbol("hook").address
    opts = LifterOptions(per_address_ccs={hook: elf.cc.preserves_all()})
    first, second = opts._with_elf(elf._elf), opts._with_elf(elf._elf)
    assert first._per_address_ccs_table_id() == second._per_address_ccs_table_id()
    assert first._per_address_ccs_table_id() != opts._per_address_ccs_table_id()


def test_one_options_object_alternating_handles_rebuilds_nothing(elf):
    other = strider.lift.load_elf(_image("ppc64le_local_entry.elf"))
    hook = elf.symbol("hook").address
    opts = LifterOptions(per_address_ccs={hook: elf.cc.preserves_all()})
    # Each derived object is kept alive, so no table id can be reused.
    first, between = opts._with_elf(elf._elf), opts._with_elf(other._elf)
    again = opts._with_elf(elf._elf)
    assert between._per_address_ccs_table_id() != first._per_address_ccs_table_id()
    assert again._per_address_ccs_table_id() == first._per_address_ccs_table_id()


def test_per_address_ccs_keyed_by_a_global_entry_reaches_the_local_call(elf):
    """`relay` passes r4 through `hook` to `sink`: by default `hook` clobbers
    r4, and an all-preserving override keyed at `hook`'s symbol keeps it."""
    sink_arg1 = call().target(elf.symbol("sink").address).arg(1, function_arg(1))
    assert not elf.analyze("relay").function.find_all(sink_arg1)

    hook = elf.symbol("hook").address
    opts = LifterOptions(per_address_ccs={hook: elf.cc.preserves_all()})
    assert len(elf.analyze("relay", opts=opts).function.find_all(sink_arg1)) == 1
    assert opts.per_address_ccs is not None and set(opts.per_address_ccs) == {hook}


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
