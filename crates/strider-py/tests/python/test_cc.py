import pytest
import strider


@pytest.mark.parametrize("name", [
    "x86_64_systemv", "aarch64_aapcs64", "arm_aapcs",
    "mips_o32", "mips_n64",
    "powerpc_sysv32", "powerpc64_elf_v1", "powerpc64_elf_v2",
    "x86_cdecl",
])
def test_cc_presets(name):
    cc = getattr(strider.CallingConvention, name)()
    assert isinstance(cc, strider.CallingConvention)
    assert cc.name() == name


def test_cc_repr():
    cc = strider.CallingConvention.x86_cdecl()
    assert "CallingConvention" in repr(cc)
    assert "x86_cdecl" in repr(cc)
