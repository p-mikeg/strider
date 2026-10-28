import pytest
import strider

_SA = strider.sleigh.SleighArch
# Every preset constructor: the public classmethods, bound to the class.
_PRESETS = sorted(
    n
    for n in dir(_SA)
    if not n.startswith("_") and getattr(getattr(_SA, n), "__self__", None) is _SA
)


def test_every_rust_preset_is_bound():
    """`ARCH_PRESETS` is `ArchPreset::ALL`; the classmethods come from a
    hand-written list in `strider-py`, and this is what compares the two.
    A preset added in Rust and forgotten here fails loudly."""
    assert _PRESETS == sorted(strider.sleigh.ARCH_PRESETS)


@pytest.mark.parametrize("name", _PRESETS)
def test_sleigh_arch_presets(name):
    arch = getattr(_SA, name)()
    assert isinstance(arch, _SA)
    assert arch.name() == name


def test_sleigh_arch_repr():
    a = strider.sleigh.SleighArch.x86_64()
    assert "SleighArch" in repr(a)
    assert "x86_64" in repr(a)
