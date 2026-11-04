"""A width no value type has raises, as an unknown `value_ty` name does,
rather than matching nothing."""

import pytest

import strider
from strider import pattern as p

_BAD = [0, 7, 23, 33, 1000]


@pytest.mark.parametrize(
    "build",
    [
        lambda n: p.anything().of_width(n),
        lambda n: p.value_of_width(n),
        lambda n: p.inputs_of_width(n, p.int_add(p.anything(), p.anything())),
        lambda n: p.load().any_output().of_width(n),
    ],
    ids=["of_width", "value_of_width", "inputs_of_width", "output_of_width"],
)
def test_unsupported_width_raises(build):
    for n in _BAD:
        with pytest.raises(strider.StriderError, match="expected one of 1, 8, 16"):
            build(n)
    for n in (1, 24, 80, 512):
        build(n)
