"""Every public binding must carry a non-empty ``__doc__``, so a new one
added without a doc comment trips here rather than shipping undocumented.

Public means a name not starting with ``_``: module-level classes and
their own methods / properties / field getters, plus module-level
functions.  Dunders and inherited members are skipped.
"""

import ast
import inspect
import pathlib
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


def _stub_files():
    root = pathlib.Path(strider.__file__).parent
    return sorted(root.rglob("*.pyi")) + sorted(root.rglob("*.py"))


def test_capture_stub_states_the_current_bare_string_rule():
    """0.2.0 dropped bare strings as pattern operands; the stub is what
    editors show, so it must not still teach the old rule."""
    src = (pathlib.Path(strider.__file__).parent / "pattern" / "__init__.pyi").read_text()
    tree = ast.parse(src)
    cls = next(
        n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "Capture"
    )
    init = next(
        n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name == "__init__"
    )
    doc = ast.get_docstring(init) or ""
    assert "bare string is not a match-pattern operand" in " ".join(doc.split())


def test_no_stub_teaches_bare_strings_as_pattern_operands():
    offenders = []
    for path in _stub_files():
        text = " ".join(path.read_text().split())
        if "used inline in a pattern" in text:
            offenders.append(str(path))
    assert not offenders, f"pre-0.2.0 bare-string rule survives in: {offenders}"
