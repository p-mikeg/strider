"""A large `known_targets` / `call_other_abis` table costs its build, and
nothing per analyse: it is seated once, at construction, and every analyse
borrows that one table.

The kernel-sweep shape seeds tens of thousands of answers once and then
analyses thousands of functions through the same `LifterOptions`; a table
rebuilt, re-seated or copied per call makes each of those analyses O(table).
"""

from __future__ import annotations

import time

import strider

from .conftest import fixture_path

_ENTRIES = 20_000
_RUNS = 7


def _opts(entries: int):
    targets = {0x800000 + i * 4: [0x900000 + i * 4] for i in range(entries)}
    return strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(known_targets=targets))


def _seeded():
    prog = strider.lift.load_elf(str(fixture_path("x86", "arithmetic")))
    return prog, _opts(_ENTRIES)


def _median_analyze_ms(prog, target, opts) -> float:
    times = []
    for _ in range(_RUNS):
        t = time.perf_counter()
        prog.analyze(target, opts=opts)
        times.append((time.perf_counter() - t) * 1000.0)
    times.sort()
    return times[len(times) // 2]


def test_analyzing_by_symbol_does_not_copy_the_seeded_tables():
    """The symbol path derives a `function_max_size` from the symbol; doing
    that through Python attributes rebuilt both tables on the way. Analysing
    by address never touches them, so the two must cost about the same."""
    prog, opts = _seeded()
    addr = prog.symbol("add").address

    by_address = _median_analyze_ms(prog, addr, opts)
    by_symbol = _median_analyze_ms(prog, "add", opts)
    assert by_symbol < by_address + 6.0, (
        f"analyze('add') costs {by_symbol:.2f} ms against {by_address:.2f} ms "
        f"by address with {_ENTRIES} known_targets"
    )


def test_a_seeded_table_costs_nothing_per_analyse():
    """The table is borrowed, not copied or re-seated, so the cost of an
    analysis does not depend on how large the seeded table is."""
    prog, seeded = _seeded()
    addr = prog.symbol("add").address
    empty = _median_analyze_ms(prog, addr, strider.lift.LifterOptions())
    with_table = _median_analyze_ms(prog, addr, seeded)
    assert with_table < empty + 0.4, (
        f"{_ENTRIES} known_targets cost {with_table:.2f} ms an analysis "
        f"against {empty:.2f} ms with no table"
    )

    ten_times = _opts(_ENTRIES * 10)
    bigger = _median_analyze_ms(prog, addr, ten_times)
    assert bigger < with_table + 0.4, (
        f"a {_ENTRIES * 10}-entry table costs {bigger:.2f} ms an analysis "
        f"against {with_table:.2f} ms for {_ENTRIES} entries, so the table is "
        f"being copied per call"
    )


def test_known_targets_attribute_reads_are_cheap():
    opts = strider.cfg.CfgOptions(
        known_targets={0x800000 + i * 4: [0x900000 + i * 4] for i in range(_ENTRIES)}
    )
    opts.known_targets  # the one build
    t = time.perf_counter()
    for _ in range(50):
        opts.known_targets
    elapsed_ms = (time.perf_counter() - t) * 1000.0
    assert elapsed_ms < 10.0, (
        f"50 reads of a {_ENTRIES}-entry table took {elapsed_ms:.1f} ms"
    )


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


def test_with_function_max_size_carries_both_tables():
    abi = strider.sleigh.CallOtherAbi.no_return()
    opts = strider.cfg.CfgOptions(
        allow_code_before_start_addr=True,
        known_targets={0x1000: "return"},
        call_other_abis={"trap": abi},
    )
    narrowed = opts.with_function_max_size(0x40)
    assert narrowed.function_max_size == 0x40
    assert narrowed.allow_code_before_start_addr is True
    assert dict(narrowed.known_targets) == {0x1000: "return"}
    assert list(narrowed.call_other_abis) == ["trap"]
    assert opts.function_max_size is None
