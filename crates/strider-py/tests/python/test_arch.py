import pytest
import strider


@pytest.mark.parametrize("name", [
    "x86_64", "x86",
    "mipsbe32", "mipsle32", "mipsbe64", "mipsle64",
    "arm", "arm_be", "arm_thumb",
    "aarch64", "aarch64be",
    "ppc32be", "ppc32le", "ppc64be", "ppc64le",
])
def test_sleigh_arch_presets(name):
    arch = getattr(strider.sleigh.SleighArch, name)()
    assert isinstance(arch, strider.sleigh.SleighArch)
    assert arch.name() == name


def test_sleigh_arch_repr():
    a = strider.sleigh.SleighArch.x86_64()
    assert "SleighArch" in repr(a)
    assert "x86_64" in repr(a)
