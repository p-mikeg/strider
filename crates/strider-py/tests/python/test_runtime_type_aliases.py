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


def test_valuety_is_the_published_rust_roster():
    """`ValueTy` is now defined from `VALUE_TYPES`, which Rust publishes out
    of `ValueType::SUPPORTED`, so the two cannot differ; what this pins is
    that the alias is still built from it and not re-spelled."""
    assert typing.get_args(p.ValueTy) == p.VALUE_TYPES
    assert len(p.VALUE_TYPES) == 2 * len({t.lower() for t in p.VALUE_TYPES})


def test_the_value_type_rejection_message_lists_the_published_roster():
    """`ValueType`'s `FromStr` builds its rejection message from the same
    `SUPPORTED` list, so a width reaching one and not the other is a desync
    inside Rust that neither alias could show."""
    try:
        p.anything().value_ty("not-a-width")  # pyright: ignore[reportArgumentType]
    except strider.StriderError as exc:
        listed = str(exc).split("expected one of ", 1)[1]
    else:
        raise AssertionError("an unknown value type must be rejected")
    assert {t.strip() for t in listed.split(",")} == {t.lower() for t in p.VALUE_TYPES}


def test_an_unknown_value_type_is_named_as_the_caller_spelled_it():
    with pytest.raises(strider.StriderError, match='unknown value type "I7"'):
        p.anything().value_ty("I7")  # pyright: ignore[reportArgumentType]


#: Each op-name parser, the `Literal` its argument is typed with, the roster
#: Rust publishes that `Literal` from, and how many operands follow the name.
_OP_NAME_LITERALS = {
    "int_cmp": ("IntCmpOpName", "INT_CMP_OPS", 2),
    "int_binary": ("IntBinaryOpName", "INT_BINARY_OPS", 2),
    "bool_binary": ("BoolBinaryOpName", "BOOL_BINARY_OPS", 2),
    "float_binary": ("FloatBinaryOpName", "FLOAT_BINARY_OPS", 2),
    "int_extend": ("ExtendOpName", "EXTEND_OPS", 1),
}


@pytest.mark.parametrize("fn_name", sorted(_OP_NAME_LITERALS))
def test_op_name_literals_are_the_published_rosters(fn_name):
    """Each `*OpName` Literal is defined from the roster its own parser
    publishes, and every name in it parses. An exhaustive Rust `match` keeps
    the roster complete for the full parsers; the boolean subset is curated."""
    fn = getattr(p, fn_name)
    literal_name, roster_name, arity = _OP_NAME_LITERALS[fn_name]
    roster = getattr(p, roster_name)
    assert typing.get_args(getattr(p, literal_name)) == roster
    operands = [p.anything()] * arity
    with pytest.raises(strider.StriderError):
        fn("not-an-op", *operands)
    for name in roster:
        fn(name, *operands)


@pytest.mark.parametrize("literal_name", sorted(n for n, _, _ in _OP_NAME_LITERALS.values()))
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
