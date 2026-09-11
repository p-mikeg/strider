"""The published namespaces: `__all__` completeness and annotation
resolvability."""

import typing

import pytest

import strider

MODULES = [
    strider.ir,
    strider.lift,
    strider.cfg,
    strider.sleigh,
    strider.reader,
    strider.opt,
    strider.pattern,
    strider.pattern.constraints,
    strider.template,
]


@pytest.mark.parametrize("mod", MODULES, ids=lambda m: m.__name__)
def test_all_lists_exactly_the_public_names(mod):
    """`from strider.<mod> import *` has to reach every documented entry
    point, the type aliases bound at import time included."""
    exported = mod.__all__
    assert len(exported) == len(set(exported)), f"{mod.__name__}: duplicate in __all__"
    public = {n for n in dir(mod) if not n.startswith("_")}
    assert public - set(exported) == set()
    assert set(exported) - public == set()


def _protocols():
    for mod in MODULES:
        for name in mod.__all__:
            obj = getattr(mod, name)
            if isinstance(obj, type) and getattr(obj, "_is_protocol", False):
                yield mod, name, obj


@pytest.mark.parametrize(
    "mod,name,proto", list(_protocols()), ids=lambda x: getattr(x, "__name__", str(x))
)
def test_protocol_annotations_resolve(mod, name, proto):
    """Every forward reference in a published protocol has to name something
    still bound in the module that defined it."""
    typing.get_type_hints(proto)
    for attr, member in vars(proto).items():
        if attr.startswith("_") or not callable(member):
            continue
        typing.get_type_hints(member)


def test_the_pattern_protocols_are_published():
    assert [n for _m, n, _p in _protocols()] == [
        "NodePat", "InputPat", "CtrlPat", "MemPat", "MemAccessPat",
        "OrderedPat", "OutputPat",
    ]
