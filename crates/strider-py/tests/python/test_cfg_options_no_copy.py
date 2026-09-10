"""A large `known_targets` / `call_other_abis` table costs its build, not one
deep copy per attribute read.

The kernel-sweep shape seeds tens of thousands of answers once and then
analyses thousands of functions through the same `LifterOptions`; a table
rebuilt on every read makes each of those analyses O(table) again, on top of
the seating pass that is genuinely per-analyse.
"""

from __future__ import annotations

import time

import strider

from .conftest import fixture_path

_ENTRIES = 20_000
_RUNS = 7


def _seeded():
    prog = strider.lift.load_elf(str(fixture_path("x86", "arithmetic")))
    targets = {0x800000 + i * 4: [0x900000 + i * 4] for i in range(_ENTRIES)}
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(known_targets=targets)
    )
    return prog, opts


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
    assert opts.known_targets[0x1000] == [0x2000]
    assert opts.known_targets[0x1010] == "return"
    assert dict(opts.known_targets) == {0x1000: [0x2000], 0x1010: "return"}
    # The read-only view round-trips through the constructor.
    assert strider.cfg.CfgOptions(known_targets=opts.known_targets).known_targets == {
        0x1000: [0x2000],
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
