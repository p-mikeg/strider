"""The `*Like` type aliases are bound at runtime, not only in the .pyi stubs,
so `p.PatLike` works in an annotation evaluated without
`from __future__ import annotations` instead of raising `AttributeError`."""

import typing

import strider
from strider import pattern as p


def test_like_aliases_exist_at_runtime():
    assert p.PatLike is not None
    assert p.constraints.ConstraintLike is not None
    assert strider.reader.MemLike is not None
    assert strider.reader.RomLike is not None


def test_patlike_usable_as_evaluated_annotation():
    # No `from __future__ import annotations`, so these are evaluated at def
    # time; a stub-only PatLike would raise AttributeError here.
    def f(x: p.PatLike, c: p.constraints.ConstraintLike) -> int:
        return 0

    assert typing.get_type_hints(f)


def test_value_type_strings_are_valuety_members():
    """`ValueTy` is documented as what `Node.value_type` returns, so every
    value type the bindings emit has to be in it."""
    elf = "fixtures/out/x64/floats.elf"
    _c, fn, _u = strider.lift.load_elf(elf).analyze("f64_arith")
    seen = {t for t in (fn.node(n).value_type() for n in fn.node_ids()) if t}
    assert seen
    assert seen <= set(typing.get_args(p.ValueTy))
