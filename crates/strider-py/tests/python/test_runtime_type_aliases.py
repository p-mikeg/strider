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


def test_valuety_is_exactly_the_rust_value_type_set():
    """`ValueTy` is hand-written in two `.py`/`.pyi` files against Rust's
    `ValueType::ALL`. Nothing links them, so a width added to the enum would be
    silently unusable from Python. `parse_value_ty` builds its rejection message
    from `ValueType::ALL`, so the message is the list.
    """
    try:
        p.anything().value_ty("not-a-width")  # pyright: ignore[reportArgumentType]
    except strider.StriderError as exc:
        listed = str(exc).split("expected one of ", 1)[1]
    else:
        raise AssertionError("an unknown value type must be rejected")
    rust = {t.strip() for t in listed.split(",")}
    assert rust == {t.lower() for t in typing.get_args(p.ValueTy)}


def test_the_valuety_stub_matches_the_runtime_alias():
    """`ValueTy` is spelled out in the stub as well; pyright checks callers
    against that copy, so drift makes a valid width a type error."""
    import pathlib
    import re

    src = (pathlib.Path(strider.__file__).parent / "pattern" / "__init__.pyi").read_text()
    body = src.split("ValueTy = Literal[", 1)[1].split("]", 1)[0]
    assert set(re.findall(r'"([^"]+)"', body)) == set(typing.get_args(p.ValueTy))
