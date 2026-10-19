"""The `*Like` type aliases are bound at runtime, not only in the .pyi stubs,
so `p.PatLike` works in an annotation evaluated without
`from __future__ import annotations` instead of raising `AttributeError`."""

import typing

import pytest

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
    `ValueType::SUPPORTED`. Nothing links them, so a width added to the list
    would be silently unusable from Python. `ValueType`'s `FromStr` builds its
    rejection message from `ValueType::SUPPORTED`, so the message is the list.
    """
    try:
        p.anything().value_ty("not-a-width")  # pyright: ignore[reportArgumentType]
    except strider.StriderError as exc:
        listed = str(exc).split("expected one of ", 1)[1]
    else:
        raise AssertionError("an unknown value type must be rejected")
    rust = {t.strip() for t in listed.split(",")}
    assert rust == {t.lower() for t in typing.get_args(p.ValueTy)}


def test_an_unknown_value_type_is_named_as_the_caller_spelled_it():
    with pytest.raises(strider.StriderError, match='unknown value type "I7"'):
        p.anything().value_ty("I7")  # pyright: ignore[reportArgumentType]


#: Each op-name parser, the `Literal` its argument is typed with, and how many
#: operands follow the name.
_OP_NAME_LITERALS = {
    "int_cmp": ("IntCmpOpName", 2),
    "int_binary": ("IntBinaryOpName", 2),
    "bool_binary": ("BoolBinaryOpName", 2),
    "float_binary": ("FloatBinaryOpName", 2),
    "int_extend": ("ExtendOpName", 1),
}


@pytest.mark.parametrize("fn_name", sorted(_OP_NAME_LITERALS))
def test_op_name_literals_are_the_rust_names(fn_name):
    """The `*OpName` Literals are hand-written against Rust op enums. The
    rejection message lists the one name each variant has, so the message is
    the Rust side and the Literal must be exactly it; an exhaustive Rust
    `match` keeps it complete for the full parsers, and the boolean subset is
    curated."""
    fn = getattr(p, fn_name)
    literal_name, arity = _OP_NAME_LITERALS[fn_name]
    literal = set(typing.get_args(getattr(p, literal_name)))
    operands = [p.anything()] * arity
    try:
        fn("not-an-op", *operands)
    except strider.StriderError as exc:
        listed = str(exc).split("expected one of ", 1)[1]
    else:
        raise AssertionError("an unknown op name must be rejected")
    assert {n.strip() for n in listed.split(",")} == literal
    for name in literal:
        fn(name, *operands)


@pytest.mark.parametrize("literal_name", sorted(n for n, _ in _OP_NAME_LITERALS.values()))
def test_the_op_name_stubs_match_the_runtime_aliases(literal_name):
    """pyright checks callers against the stub's copy of each Literal."""
    import pathlib
    import re

    src = (pathlib.Path(strider.__file__).parent / "pattern" / "__init__.pyi").read_text()
    body = src.split(f"{literal_name} = Literal[", 1)[1].split("]", 1)[0]
    assert set(re.findall(r'"([^"]+)"', body)) == set(typing.get_args(getattr(p, literal_name)))


def test_the_valuety_stub_matches_the_runtime_alias():
    """`ValueTy` is spelled out in the stub as well; pyright checks callers
    against that copy, so drift makes a valid width a type error."""
    import pathlib
    import re

    src = (pathlib.Path(strider.__file__).parent / "pattern" / "__init__.pyi").read_text()
    body = src.split("ValueTy = Literal[", 1)[1].split("]", 1)[0]
    assert set(re.findall(r'"([^"]+)"', body)) == set(typing.get_args(p.ValueTy))
