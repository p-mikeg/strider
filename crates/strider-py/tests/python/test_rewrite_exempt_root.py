"""A rewrite whose match has no instruction address (an `InitialVar` or `Phi`
root) cannot attribute the nodes its replacement builds, so it raises and
leaves the function valid."""

from __future__ import annotations

import pytest

import strider
from strider import pattern as p
from strider import template as t

from .conftest import lift_bytes

# mov rax, rdi; ret
_PASS_THROUGH = bytes.fromhex("4889f8c3")
# test edi, edi; je 1f; mov eax, 1; jmp 2f; 1: mov eax, 2; 2: ret
_MERGE = bytes.fromhex("85ff7407b801000000eb05b802000000c3")
# vextracti128 xmm1, ymm0, 1; movq rax, xmm1; ret: tracks ZMM1 whole
_FROM_VECTOR = bytes.fromhex("c4e37d39c10166480f7ec8c3")


def _rdi():
    rdi = strider.sleigh.SleighArch.x86_64().reg("RDI")
    assert rdi is not None
    return rdi


@pytest.mark.parametrize(
    "replace",
    [lambda: t.int_const(3), lambda: t.int_add(t.int_const(1), t.int_const(2))],
    ids=["const", "add"],
)
def test_an_initial_var_root_refuses_a_built_replacement(replace):
    fn = lift_bytes(_PASS_THROUGH, optimize=False)
    assert fn.validate() is None
    with pytest.raises(strider.StriderError, match="instruction address"):
        fn.rewrite(find=p.initial_var_for(_rdi()), replace=replace())
    assert fn.validate() is None


def test_a_phi_root_refuses_a_built_replacement():
    fn = lift_bytes(_MERGE, optimize=False)
    assert fn.find_all(p.phi())
    with pytest.raises(strider.StriderError, match="instruction address"):
        fn.rewrite(find=p.phi(), replace=t.int_const(5))
    assert fn.validate() is None


def test_an_initial_var_root_takes_a_capture_replacement():
    fn = lift_bytes(_PASS_THROUGH, optimize=False)
    x = p.Capture()
    assert fn.rewrite(find=p.initial_var_for(_rdi()).capture(x), replace=x) == 0
    assert fn.validate() is None


def test_a_declined_replacement_on_an_initial_var_root_is_no_change():
    fn = lift_bytes(_FROM_VECTOR, optimize=False)
    zmm1 = strider.sleigh.SleighArch.x86_64().reg("ZMM1")
    assert zmm1 is not None
    c = p.Capture()
    find = p.initial_var_for(zmm1)
    # Wider than 128 bits, where a template constant declines rather than
    # truncates.
    assert [m.value_type(c) for m in fn.find_all(find.capture(c))] == ["I512"]
    assert fn.rewrite(find=find, replace=t.int_const(0)) == 0
    assert fn.rewrite_all([(find, t.int_const(0)), (p.initial_var_for(_rdi()), t.int_const(1))]) == 0
    assert fn.validate() is None
