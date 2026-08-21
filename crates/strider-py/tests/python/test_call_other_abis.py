import pytest

import strider

from .conftest import symbol_addr


def test_a_string_class_is_rejected():
    with pytest.raises(ValueError, match="CallOtherAbi"):
        strider.cfg.CfgOptions(call_other_abis={"trap": "no_return"})


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


def test_symbol_entry_carries_every_cfg_field(monkeypatch, x86_memory_elf):
    """`ElfLifter.analyze(<name>)` rebuilds `CfgOptions` to seat the symbol's
    recorded size; a field it forgets is silently dropped."""
    import strider._api as api

    real = api.CfgOptions
    seen = {}

    def recording(**kwargs):
        seen.update(kwargs)
        return real(**kwargs)

    monkeypatch.setattr(api, "CfgOptions", recording)
    prog = strider.lift.load_elf(str(x86_memory_elf))
    prog.analyze(
        "array_sum",
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(
                allow_code_before_start_addr=True,
                call_other_abis={"swi": strider.sleigh.CallOtherAbi.no_return()},
            )
        ),
    )
    fields = {n for n in dir(real) if not n.startswith("_")}
    assert fields <= set(seen), f"dropped {sorted(fields - set(seen))}"
    assert seen["call_other_abis"] == {
        "swi": strider.sleigh.CallOtherAbi.no_return()
    }
