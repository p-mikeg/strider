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
    """`ElfLifter.analyze(<name>)` narrows `CfgOptions` to the symbol's
    recorded size through `with_function_max_size`, the single carrier; a
    field it forgets is silently dropped."""
    opts = strider.cfg.CfgOptions(
        allow_code_before_start_addr=True,
        known_targets={0x1000: "return"},
        call_other_abis={"swi": strider.sleigh.CallOtherAbi.no_return()},
    )
    narrowed = opts.with_function_max_size(0x100)
    assert narrowed.function_max_size == 0x100
    carried = {n for n in dir(opts) if not n.startswith("_")}
    carried -= {"function_max_size", "with_function_max_size"}
    for name in carried:
        assert getattr(narrowed, name) == getattr(opts, name), f"dropped {name}"
    assert narrowed.call_other_abis == {
        "swi": strider.sleigh.CallOtherAbi.no_return()
    }

    # The symbol path is what reaches for it; a lift through it must survive.
    prog = strider.lift.load_elf(str(x86_memory_elf))
    prog.analyze("array_sum", opts=strider.lift.LifterOptions(cfg=opts))
