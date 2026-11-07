import pytest
import strider


@pytest.mark.parametrize("name", [
    # Userland presets
    "x86_64_systemv", "aarch64_aapcs64", "arm_aapcs", "arm_aapcs_soft",
    "mips_o32", "mips_n64",
    "powerpc_sysv32", "powerpc64_elf_v1", "powerpc64_elf_v2",
    "x86_cdecl",
    # Linux kernel presets: x86 32-bit diverges in its integer argument
    # registers (regparm-3), PowerPC in naming no AltiVec register
    "x86_linux_kernel",
    "powerpc_sysv32_linux_kernel",
    "powerpc64_elf_v1_linux_kernel",
    "powerpc64_elf_v2_linux_kernel",
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


def test_a_preset_convention_must_fit_the_lifters_arch():
    """`x86_cdecl` names `ESP`, a 4-byte view of x86-64's `RSP`, so it builds
    on an x86-64 lifter; it must be refused there rather than lift with a
    4-byte stack pointer."""
    mem = strider.reader.BufferReader(0x1000, b"\xc3")
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    with pytest.raises(strider.StriderError, match="stack pointer"):
        lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_cdecl())


def test_load_elf_with_an_overriding_arch_refuses_the_header_convention():
    """`load_elf(arch=...)` without `cc=` keeps the header's convention, which
    for an i386 image is `x86_cdecl`: wrong for the x86-64 arch asked for."""
    from .conftest import fixture_path

    prog = strider.lift.load_elf(
        str(fixture_path("x86", "arithmetic")),
        arch=strider.sleigh.SleighArch.x86_64(),
    )
    with pytest.raises(strider.StriderError, match="stack pointer"):
        prog.analyze("add")
