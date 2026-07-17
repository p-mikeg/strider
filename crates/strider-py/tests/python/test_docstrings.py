"""Every public binding must carry a non-empty ``__doc__``.

PyO3 turns a Rust ``///`` doc comment into the Python ``__doc__`` of the
generated class / method / function, and a ``#[pyo3(get)]`` field's
doc comment into its getset-descriptor ``__doc__``.  This test walks the
public surface of ``strider``, ``strider.pattern``,
``strider.pattern.constraints``, and ``strider.opt``
and asserts each public binding is documented, so a future binding added
without a doc comment trips here rather than shipping undocumented.

Scope (public = name not starting with ``_``):
  * module-level classes and their own (non-inherited) public members:
    methods, ``#[classmethod]`` / ``#[staticmethod]``, ``#[getter]``
    properties, and ``#[pyo3(get)]`` field getset-descriptors.
  * module-level functions (``#[pyfunction]``).

Dunder names and inherited members are skipped.
"""

import inspect
import types

import strider
import strider.opt
import strider.pattern
import strider.pattern.constraints


# Tiny, justified allow-list of public bindings exempt from the
# non-empty-doc requirement.  Keep this empty unless a binding genuinely
# cannot carry a doc; document the reason inline.
_DOC_ALLOWLIST: set[str] = set()


def _is_public(name: str) -> bool:
    return not name.startswith("_")


def _member_doc_targets(cls):
    """Yield ``(qualname, obj)`` for each own public member of ``cls``
    whose ``__doc__`` we require — methods, classmethods, properties, and
    ``#[pyo3(get)]`` field getset-descriptors.  Skips inherited members
    (only ``vars(cls)`` entries) and dunders.
    """
    for mname, member in vars(cls).items():
        if not _is_public(mname):
            continue
        # PyO3 surfaces methods/classmethods as builtin functions /
        # method descriptors, getters as ``property``, and
        # ``#[pyo3(get)]`` fields as ``getset_descriptor``.  All carry a
        # meaningful ``__doc__`` when documented on the Rust side.
        if isinstance(member, property) or callable(member) or (
            type(member).__name__ == "getset_descriptor"
        ):
            yield mname, member


def _collect_missing(modname, mod):
    missing = []
    for name in dir(mod):
        if not _is_public(name):
            continue
        obj = getattr(mod, name)
        if inspect.isclass(obj):
            qual = f"{modname}.{name}"
            if qual not in _DOC_ALLOWLIST and not (obj.__doc__ and obj.__doc__.strip()):
                missing.append(qual)
            for mname, member in _member_doc_targets(obj):
                mqual = f"{modname}.{name}.{mname}"
                if mqual in _DOC_ALLOWLIST:
                    continue
                doc = getattr(member, "__doc__", None)
                if not (doc and doc.strip()):
                    missing.append(mqual)
        elif isinstance(obj, (types.BuiltinFunctionType,)) or inspect.isbuiltin(obj):
            qual = f"{modname}.{name}"
            if qual in _DOC_ALLOWLIST:
                continue
            doc = getattr(obj, "__doc__", None)
            if not (doc and doc.strip()):
                missing.append(qual)
    return missing


def test_every_public_binding_has_a_docstring():
    missing = []
    for modname, mod in (
        ("strider", strider),
        ("strider.pattern", strider.pattern),
        ("strider.pattern.constraints", strider.pattern.constraints),
        ("strider.opt", strider.opt),
    ):
        missing.extend(_collect_missing(modname, mod))
    assert not missing, (
        "public bindings missing a non-empty __doc__:\n  "
        + "\n  ".join(sorted(missing))
    )
