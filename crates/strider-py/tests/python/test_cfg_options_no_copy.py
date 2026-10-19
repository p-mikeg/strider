"""A large `known_targets` table costs its build, and nothing per analyse: it
is seated once, at construction, and every analyse borrows that one table.

The kernel-sweep shape seeds tens of thousands of answers once and then
analyses thousands of functions through the same `LifterOptions`; a table
copied through Python per call makes each of those analyses O(table).
Counted, not timed: such a copy shows in `tracemalloc`'s peak, and the views
are memoized, so every read hands back one object.
"""

from __future__ import annotations

import tracemalloc

import strider

from .conftest import fixture_path

_ENTRIES = 20_000
#: An analyse's Python allocations may differ by this much across table sizes;
#: a dict copy of the 20,000-entry table is about 1 MB.
_SLACK = 16 * 1024


def _opts(entries: int):
    targets = {0x800000 + i * 4: [0x900000 + i * 4] for i in range(entries)}
    return strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(known_targets=targets))


def _program():
    return strider.lift.load_elf(str(fixture_path("x86", "arithmetic")))


def _python_peak(prog, target, opts) -> int:
    """Peak Python-heap bytes of one analyse, after one that warms caches."""
    prog.analyze(target, opts=opts)
    tracemalloc.start()
    try:
        prog.analyze(target, opts=opts)
        return tracemalloc.get_traced_memory()[1]
    finally:
        tracemalloc.stop()


def test_analyzing_by_symbol_does_not_copy_the_seeded_tables():
    """The symbol path derives a `function_max_size` from the symbol; doing
    that through Python attributes rebuilt the tables on the way."""
    prog = _program()
    empty = _python_peak(prog, "add", strider.lift.LifterOptions())
    seeded = _python_peak(prog, "add", _opts(_ENTRIES))
    assert seeded <= empty + _SLACK, (
        f"analyze('add') with {_ENTRIES} known_targets peaked at {seeded} Python "
        f"bytes against {empty} with none"
    )


def test_a_seeded_table_costs_nothing_per_analyse():
    """The table is borrowed, not copied, so what an analysis allocates on
    the Python heap does not depend on how large the seeded table is."""
    prog = _program()
    addr = prog.symbol("add").address
    empty = _python_peak(prog, addr, strider.lift.LifterOptions())
    for entries in (_ENTRIES, _ENTRIES * 10):
        seeded = _python_peak(prog, addr, _opts(entries))
        assert seeded <= empty + _SLACK, (
            f"a {entries}-entry table peaked an analysis at {seeded} Python "
            f"bytes against {empty} with no table"
        )


def test_known_targets_reads_back_one_memoized_view():
    """The view is built on the first read and shared by every options object
    derived from this one, so a read costs nothing per call."""
    opts = strider.cfg.CfgOptions(
        known_targets={0x800000 + i * 4: [0x900000 + i * 4] for i in range(_ENTRIES)},
        no_return_targets=[0x2000],
    )
    view = opts.known_targets
    assert opts.known_targets is view
    assert opts.no_return_targets is opts.no_return_targets
    narrowed = opts._with_function_max_size(0x40)
    assert narrowed.known_targets is view
    assert strider.lift.LifterOptions(cfg=opts).cfg.known_targets is view


def test_call_other_abis_resolve_once_per_arch():
    """The table is resolved on the first analyse and borrowed by every later
    one, through the options an `ElfLifter` derives per call as well."""
    cfg = strider.cfg.CfgOptions(
        call_other_abis={"trap": strider.sleigh.CallOtherAbi.no_return()}
    )
    opts = strider.lift.LifterOptions(cfg=cfg)
    prog = _program()
    for _ in range(3):
        prog.analyze("add", opts=opts)
    assert cfg._call_other_override_builds() == 1
    prog.build_cfg(prog.symbol("add").address, cfg)
    assert cfg._call_other_override_builds() == 1


def test_known_targets_still_reads_back():
    opts = strider.cfg.CfgOptions(known_targets={0x1000: [0x2000], 0x1010: "return"})
    assert opts.known_targets[0x1000] == (0x2000,)
    assert opts.known_targets[0x1010] == "return"
    assert dict(opts.known_targets) == {0x1000: (0x2000,), 0x1010: "return"}
    # The read-only view round-trips through the constructor.
    assert strider.cfg.CfgOptions(known_targets=opts.known_targets).known_targets == {
        0x1000: (0x2000,),
        0x1010: "return",
    }


def test_with_function_max_size_carries_every_table():
    abi = strider.sleigh.CallOtherAbi.no_return()
    opts = strider.cfg.CfgOptions(
        allow_code_before_start_addr=True,
        known_targets={0x1000: "return"},
        call_other_abis={"trap": abi},
        data_ranges=[(0x10, 0x18)],
        no_return_targets=[0x2000],
    )
    narrowed = opts._with_function_max_size(0x40)
    assert narrowed.function_max_size == 0x40
    assert narrowed.allow_code_before_start_addr is False
    assert dict(narrowed.known_targets) == {0x1000: "return"}
    assert list(narrowed.call_other_abis) == ["trap"]
    assert narrowed.data_ranges == [(0x10, 0x18)]
    assert narrowed.no_return_targets == {0x2000}
    assert opts.function_max_size is None
