from typing import Optional

import pytest

import strider

from .conftest import symbol_addr


def test_a_string_class_is_rejected():
    with pytest.raises(ValueError, match="CallOtherAbi"):
        # Deliberate: a bare string where a CallOtherAbi belongs.
        strider.cfg.CfgOptions(call_other_abis={"trap": "no_return"})  # type: ignore[dict-item]


def test_classes_round_trip():
    abis = {
        "trap": strider.sleigh.CallOtherAbi.no_return(),
        "rdtsc": strider.sleigh.CallOtherAbi.pure(),
    }
    opts = strider.cfg.CfgOptions(call_other_abis=abis)
    assert opts.call_other_abis == abis


def test_override_reaches_the_decode(x86_memory_elf):
    """`swi` carries every x86 INT; reclassifying it must change nothing about
    a function that has none, and must not fault the build."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    plain = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    overridden = s.build_cfg(
        addr,
        strider.cfg.CfgOptions(
            allow_code_before_start_addr=True,
            call_other_abis={"swi": strider.sleigh.CallOtherAbi.no_return()},
        ),
    )
    assert plain is not None and overridden is not None


def test_symbol_entry_carries_every_cfg_field(x86_memory_elf):
    """`ElfLifter.analyze(<name>)` bounds an open lift by the symbol's
    recorded size and reads every other field as given: the answer the
    address gives under that size."""

    def opts(size: Optional[int]) -> strider.lift.LifterOptions:
        cfg = strider.cfg.CfgOptions(
            function_max_size=size,
            allow_code_before_start_addr=size is None,
            known_targets={0x1000: "return"},
            call_other_abis={"swi": strider.sleigh.CallOtherAbi.no_return()},
            data_ranges=[(0x10, 0x18)],
            no_return_targets=[0x2000],
        )
        return strider.lift.LifterOptions(cfg=cfg)

    prog = strider.lift.load_elf(str(x86_memory_elf))
    addr = prog.symbol("array_sum").address
    prog.add_symbols({"array_sum_head": (addr, 4)})
    by_name = prog.analyze("array_sum_head", opts=opts(None))
    by_addr = prog.analyze(addr, opts=opts(4))
    assert by_name.function.to_text() == by_addr.function.to_text()
    assert by_name.cfg.incomplete == by_addr.cfg.incomplete
    assert by_name.cfg.incomplete != prog.analyze(addr, opts=opts(None)).cfg.incomplete


def test_symbol_entry_honours_a_no_return_target(x86_calls_elf):
    """`repeat_call_pair` calls `pair_a` twice; marking `pair_a` no-return
    ends the lift at the first call, through the symbol path too."""
    pair_a = symbol_addr(x86_calls_elf, "pair_a")
    after_first_call = 0x4011F5
    prog = strider.lift.load_elf(str(x86_calls_elf))
    plain = prog.analyze("repeat_call_pair")
    assert plain.cfg.region_at(after_first_call) is not None
    marked = prog.analyze(
        "repeat_call_pair",
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(no_return_targets=[pair_a])
        ),
    )
    assert marked.cfg.region_at(after_first_call) is None
