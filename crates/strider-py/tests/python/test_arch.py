import pytest
import strider

_SA = strider.sleigh.SleighArch
# Every preset constructor: the public classmethods, bound to the class.
_PRESETS = sorted(
    n
    for n in dir(_SA)
    if not n.startswith("_") and getattr(getattr(_SA, n), "__self__", None) is _SA
)


def test_the_preset_list_is_found():
    assert _PRESETS == sorted([
        "aarch64", "aarch64be", "arm", "arm_be", "arm_be_kernel", "arm_thumb",
        "mipsbe32", "mipsbe64", "mipsle32", "mipsle64", "ppc32be", "ppc32le",
        "ppc64be", "ppc64le", "x86", "x86_64",
    ])


@pytest.mark.parametrize("name", _PRESETS)
def test_sleigh_arch_presets(name):
    arch = getattr(_SA, name)()
    assert isinstance(arch, _SA)
    assert arch.name() == name


def test_sleigh_arch_repr():
    a = strider.sleigh.SleighArch.x86_64()
    assert "SleighArch" in repr(a)
    assert "x86_64" in repr(a)
