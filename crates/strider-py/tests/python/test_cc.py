import pytest
import strider


@pytest.mark.parametrize("name", [
    # Userland presets
    "x86_64_systemv", "aarch64_aapcs64", "arm_aapcs", "arm_aapcs_soft",
    "mips_o32", "mips_n64",
    "powerpc_sysv32", "powerpc64_elf_v1", "powerpc64_elf_v2",
    "x86_cdecl",
    # Linux kernel preset (x86 32-bit regparm-3 is the only divergent one)
    "x86_linux_kernel",
])
def test_cc_presets(name):
    cc = getattr(strider.sleigh.CallingConvention, name)()
    assert isinstance(cc, strider.sleigh.CallingConvention)
    assert cc.name() == name


def test_cc_repr():
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    assert "CallingConvention" in repr(cc)
    assert "x86_cdecl" in repr(cc)


def test_arm_soft_float_passes_floats_outside_the_vfp_bank():
    """The soft variant differs from `arm_aapcs` only in the VFP argument and
    return lists, so a float parameter has no float carrier under it."""
    from strider import pattern as p

    from .conftest import fixture_path

    prog = strider.lift.load_elf(str(fixture_path("arm", "floats")))
    counts = {}
    for name in ("arm_aapcs", "arm_aapcs_soft"):
        _cfg, fn, _u = prog.analyze(
            "f64_arith", getattr(strider.sleigh.CallingConvention, name)()
        )
        counts[name] = len(fn.find_all(p.function_arg_float(0).capture(p.Capture())))
    assert counts == {"arm_aapcs": 1, "arm_aapcs_soft": 0}
