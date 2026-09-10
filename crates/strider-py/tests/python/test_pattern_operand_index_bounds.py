"""An out-of-range operand index is an error, never wrapped arithmetic.

Every one of these setters shifts `idx` past the node's fixed head slots, so an
unbounded `idx` overflows: a panic in a debug build, and without
`overflow-checks` a wrap onto a real slot in a shipped wheel.
"""

import pytest

import strider
from strider import pattern as p

HUGE = 2**64 - 1


def _setters():
    return [
        ("call().arg", lambda i: p.call().arg(i, p.anything())),
        ("call_other().arg", lambda i: p.call_other().arg(i, p.anything())),
        ("ret().ret_val", lambda i: p.ret().ret_val(i, p.anything())),
        ("phi().phi_input", lambda i: p.phi().phi_input(i, p.anything())),
        ("mem_phi().phi_input", lambda i: p.mem_phi().phi_input(i, p.mem_phi())),
    ]


@pytest.mark.parametrize("name,build", _setters(), ids=[n for n, _ in _setters()])
def test_huge_operand_index_raises(name, build):
    with pytest.raises((strider.StriderError, ValueError)) as e:
        build(HUGE)
    assert "operand index" in str(e.value)


@pytest.mark.parametrize("name,build", _setters(), ids=[n for n, _ in _setters()])
def test_ordinary_operand_index_still_works(name, build):
    build(0).into_pat()


def test_nesting_cap_message_names_the_binding_limit():
    """The count ceiling is the looser of the two nesting bounds, and a
    pattern anywhere near it will not compile: the message has to say so
    rather than read as a usable depth."""
    pat = p.anything()
    with pytest.raises(strider.StriderError) as e:
        for _ in range(2000):
            pat = pat.of_width(32)
    assert "stack budget" in str(e.value)
