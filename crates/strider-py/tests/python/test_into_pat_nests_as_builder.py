"""An `into_pat()` `Pat` is its builder: the same answers, captured or not,
and a later edit to the builder reaches it."""

import pytest

from strider import pattern as p

from .conftest import built_function

_BUILDERS = {
    "load": p.load,
    "store": p.store,
    "call": p.call,
    "call_other": p.call_other,
    "phi": p.phi,
    "mem_phi": p.mem_phi,
    "entry": p.entry,
    "region": p.region,
    "ret": p.ret,
    "if_else": p.if_else,
    "switch": p.switch,
    "indirect_branch": p.indirect_branch,
    "unreachable": p.unreachable,
    "function_arg": lambda: p.function_arg(0),
    "int_binary": lambda: p.int_binary("Add", p.anything(), p.anything()),
    "float_binary": lambda: p.float_binary("Add", p.anything(), p.anything()),
    "bool_binary": lambda: p.bool_binary("And", p.anything(), p.anything()),
}


@pytest.fixture(scope="module")
def functions():
    return [
        built_function(arch, case, sym)
        for arch, case, sym in [
            ("x86", "control", "clamp"),
            ("x86", "control", "abs_val"),
            ("x86", "memory", "array_sum"),
            ("x86", "memory", "array_copy"),
        ]
    ]


def _hits(functions, pat):
    return [h for f in functions for h in f.find_all(pat)]


@pytest.mark.parametrize("name", sorted(_BUILDERS))
def test_a_capture_on_into_pat_is_the_builders_own(functions, name):
    make = _BUILDERS[name]
    c = p.Capture()
    own = _hits(functions, make().capture(c))
    wrapped = _hits(functions, make().into_pat().capture(c))
    assert len(wrapped) == len(own)
    assert all(h.has(c) for h in wrapped)


def test_ret_into_pat_capture_binds(functions):
    c = p.Capture()
    assert _hits(functions, p.ret().into_pat().capture(c))


def test_a_later_builder_edit_reaches_its_into_pat(functions):
    b = p.load()
    q = b.into_pat()
    before = len(_hits(functions, q))
    b.addr(p.int_const(0x7FFF_0000))
    assert before
    assert len(_hits(functions, q)) == len(_hits(functions, b)) < before
