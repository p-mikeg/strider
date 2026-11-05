"""Integer arguments: an `__index__` object reads as the int it gives, an int
outside a fixed-width argument (`int_const_any_width`, float bits, a stack
offset) raises `OverflowError`, and a render `path` takes an
`os.PathLike`."""

from __future__ import annotations

import pathlib

import pytest

import strider
from strider import pattern as p
from strider import template as tpl
from strider.sleigh import VnSpace

from .conftest import fixture_path


class _Index:
    def __init__(self, value: int) -> None:
        self.value = value

    def __index__(self) -> int:
        return self.value


def _ram():
    return VnSpace.RAM


@pytest.mark.parametrize("v", [5, -1, 2**64 + 3, 2**200])
def test_an_index_object_is_the_int_it_gives(v):
    assert repr(p.int_const(_Index(v))) == repr(p.int_const(v))  # type: ignore[arg-type]
    assert repr(tpl.int_const(_Index(v))) == repr(tpl.int_const(v))  # type: ignore[arg-type]
    if abs(v) < 2**127:
        assert repr(p.load().stack_offset(_Index(-v))) == repr(p.load().stack_offset(-v))  # type: ignore[arg-type]
        assert repr(p.function_arg_stack(_ram(), _Index(v))) == repr(  # type: ignore[arg-type]
            p.function_arg_stack(_ram(), v)
        )


def test_a_bound_capture_is_an_int_constant():
    """`BoundCapture` converts directly wherever an int is taken."""
    _c, fn, _u = strider.lift.load_elf(str(fixture_path("x64", "memory"))).analyze("main")
    k = p.Capture()
    m = fn.find_all(p.int_const(k))[0]
    v = int(m[k])
    assert repr(p.int_const(m[k])) == repr(p.int_const(v))  # type: ignore[arg-type]
    assert repr(tpl.int_const(m[k])) == repr(tpl.int_const(v))  # type: ignore[arg-type]
    assert repr(p.load().stack_offset(m[k])) == repr(p.load().stack_offset(v))  # type: ignore[arg-type]
    assert repr(p.int_const_any_width(_Index(3))) == repr(p.int_const_any_width(3))  # type: ignore[arg-type]


@pytest.mark.parametrize(
    "call",
    [
        lambda: p.int_const_any_width(2**63),
        lambda: p.int_const_any_width(2**200),
        lambda: p.int_const_any_width([1, 2**200]),
        lambda: p.int_const_any_width(2**600),
        lambda: p.int_const_any_width(_Index(2**64)),  # type: ignore[arg-type]
        lambda: p.float_const(-1),
        lambda: p.float_const(2**64),
        lambda: tpl.float_const(-1),
        lambda: tpl.float_const(2**64),
        lambda: p.load().stack_offset(2**200),
        lambda: p.function_arg_stack(_ram(), -(2**127) - 1),
    ],
)
def test_an_out_of_range_int_raises_overflow_error(call):
    with pytest.raises(OverflowError, match="out of range"):
        call()


@pytest.mark.parametrize("bad", ["1", 1.0])
def test_a_non_int_still_raises_type_error(bad):
    for call in (p.int_const_any_width, p.float_const, tpl.float_const, p.load().stack_offset):
        with pytest.raises(TypeError):
            call(bad)


def test_render_paths_take_a_path_like(tmp_path):
    cfg, fn, _u = strider.lift.load_elf(str(fixture_path("x64", "memory"))).analyze("main")
    for render, name in [
        (fn.to_dot, "f.dot"),
        (fn.to_html, "f.html"),
        (cfg.to_dot, "c.dot"),
        (cfg.to_html, "c.html"),
    ]:
        path = tmp_path / name
        assert render(pathlib.Path(path)) is None  # type: ignore[arg-type]
        assert path.read_text() == render()
