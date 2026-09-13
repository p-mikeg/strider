"""`add_elf` and the read-only view: what one image maps writable is not
read-only memory, and merging images compares what each address serves."""

from __future__ import annotations

import pytest

import strider
import strider._strider as ext

from . import _elf_builder as eb


def _overlapping_r_and_rw(path, blob: bytes) -> None:
    """A read-only PT_LOAD over [0x400000, 0x402000) and a writable one over
    its second page."""
    eb.Elf(
        loads=[(eb.PF_R, 0x400000, 0), (eb.PF_R | eb.PF_W, 0x401000, 1)],
        sections=[
            eb.Section(".rodata", 0x400000, blob, eb.SHF_ALLOC),
            eb.Section(".data", 0x401000, blob[0x1000:], eb.SHF_ALLOC | eb.SHF_WRITE),
        ],
    ).write(path)


@pytest.mark.parametrize("relocate", [False, True])
def test_the_read_only_view_bars_a_writable_load_over_a_read_only_one(tmp_path, relocate):
    path = tmp_path / "overlap.elf"
    _overlapping_r_and_rw(path, bytes(0x1000) + b"\x2a" * 0x1000)
    elf = ext._load_elf_from_segments(str(path), relocate)
    assert elf.ro_reader().read(0x401000, 8) is None
    assert elf.ro_reader().read(0x400ff8, 8) == bytes(8)


def test_add_elf_bars_a_later_images_writable_load_from_the_read_only_view(tmp_path):
    blob = b"\x2a" * 0x1000
    ro, rw = tmp_path / "ro.elf", tmp_path / "rw.elf"
    eb.Elf(
        loads=[(eb.PF_R, 0x400000, 0)],
        sections=[eb.Section(".rodata", 0x400000, blob * 2, eb.SHF_ALLOC)],
    ).write(ro)
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_W, 0x401000, 0)],
        sections=[eb.Section(".data", 0x401000, blob, eb.SHF_ALLOC | eb.SHF_WRITE)],
    ).write(rw)
    lift = strider.lift.load_elf(str(ro))
    lift.add_elf(str(rw))
    assert lift._elf.ro_reader().read(0x401000, 8) is None
    assert lift._elf.ro_reader().read(0x400000, 8) == blob[:8]


def _nested(path, count: int, blob: bytes) -> None:
    """`count` PT_LOADs of `blob`, the i-th starting `i` bytes in, each over
    its own copy of it."""
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, 0x400000 + i, i) for i in range(count)],
        sections=[eb.Section(f".t{i}", 0x400000 + i, blob) for i in range(count)],
    ).write(path)


def test_add_elf_accepts_a_re_merge_of_nested_loads(tmp_path):
    path = tmp_path / "nested.elf"
    _nested(path, 16, b"\x90" * 0x4000)
    lift = strider.lift.load_elf(str(path))
    lift.add_elf(str(path))


def test_add_elf_refuses_nested_loads_serving_different_bytes(tmp_path):
    a, b = tmp_path / "a.elf", tmp_path / "b.elf"
    _nested(a, 16, b"\x90" * 0x4000)
    _nested(b, 16, b"\x90" * 0x2000 + b"\xcc" + b"\x90" * 0x1fff)
    lift = strider.lift.load_elf(str(a))
    with pytest.raises(strider.StriderError, match="differ"):
        lift.add_elf(str(b))
