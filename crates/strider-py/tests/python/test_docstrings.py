"""Every public binding must carry a non-empty ``__doc__``, so a new one
added without a doc comment trips here rather than shipping undocumented.

Public means a name not starting with ``_``: module-level classes and
their own methods / properties / field getters, plus module-level
functions.  Dunders and inherited members are skipped.
"""

import inspect
import types

import strider
import strider.opt
import strider.pattern
import strider.pattern.constraints


# Keep empty unless a binding genuinely cannot carry a doc; document the
# reason inline.
_DOC_ALLOWLIST: set[str] = set()


def _is_public(name: str) -> bool:
    return not name.startswith("_")


def _member_doc_targets(cls):
    """Own public members of ``cls`` that must be documented: methods,
    classmethods, properties, and field getters.  ``vars``, not ``dir``,
    so inherited members belong to the base class instead.
    """
    for mname, member in vars(cls).items():
        if not _is_public(mname):
            continue
        # Methods/classmethods surface as builtin functions or method
        # descriptors, getters as ``property``, fields as
        # ``getset_descriptor``.
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
