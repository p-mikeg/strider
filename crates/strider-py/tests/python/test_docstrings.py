"""Every published module and every public binding in it must carry a
non-empty ``__doc__``, so a new one added without a doc comment trips here
rather than shipping undocumented.

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
            if not (obj.__doc__ and obj.__doc__.strip()):
                missing.append(qual)
            for mname, member in _member_doc_targets(obj):
                mqual = f"{modname}.{name}.{mname}"
                doc = getattr(member, "__doc__", None)
                if not (doc and doc.strip()):
                    missing.append(mqual)
        elif isinstance(obj, (types.BuiltinFunctionType,)) or inspect.isbuiltin(obj):
            qual = f"{modname}.{name}"
            doc = getattr(obj, "__doc__", None)
            if not (doc and doc.strip()):
                missing.append(qual)
    return missing


def _modules():
    """Every published module, derived from `strider.__all__` plus the one
    nested namespace, so a module added later cannot escape the sweep."""
    yield "strider", strider
    for name in strider.__all__:
        obj = getattr(strider, name)
        if isinstance(obj, types.ModuleType):
            yield f"strider.{name}", obj
    yield "strider.pattern.constraints", strider.pattern.constraints


def test_every_module_has_a_docstring():
    missing = [name for name, mod in _modules() if not (mod.__doc__ or "").strip()]
    assert not missing, f"modules missing a non-empty __doc__: {sorted(missing)}"


def test_every_public_binding_has_a_docstring():
    missing = []
    for modname, mod in _modules():
        missing.extend(_collect_missing(modname, mod))
    assert not missing, (
        "public bindings missing a non-empty __doc__:\n  "
        + "\n  ".join(sorted(missing))
    )


def test_the_module_sweep_reaches_every_published_module():
    assert {name for name, _ in _modules()} == {
        "strider", "strider.ir", "strider.lift", "strider.cfg",
        "strider.sleigh", "strider.reader", "strider.opt", "strider.pattern",
        "strider.template", "strider.pattern.constraints",
    }


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


def test_each_pass_doc_is_its_stub_text():
    """A pass's runtime `__doc__` (`help()`) and its stub docstring (an
    editor's) are one text, up to line wrapping."""
    src = (pathlib.Path(strider.__file__).parent / "opt.pyi").read_text()
    stubs = {
        n.name: " ".join((ast.get_docstring(n) or "").split())
        for n in ast.parse(src).body
        if isinstance(n, ast.ClassDef)
    }
    names = strider.opt._MAIN_PASS_NAMES + strider.opt._POST_PASS_NAMES
    differ = {
        name: (" ".join((getattr(strider.opt, name).__doc__ or "").split()), stubs.get(name))
        for name in names
        if " ".join((getattr(strider.opt, name).__doc__ or "").split()) != stubs.get(name)
    }
    assert not differ, differ


def test_each_value_op_doc_is_its_stub_text():
    """A value-op builder's runtime `__doc__` is its stub docstring, up to
    line wrapping, in both `strider.pattern` and `strider.template`."""
    import strider.template

    pkg = pathlib.Path(strider.__file__).parent
    differ = {}
    for mod, stub in (
        (strider.pattern, pkg / "pattern" / "__init__.pyi"),
        (strider.template, pkg / "template.pyi"),
    ):
        stubs = {
            n.name: " ".join((ast.get_docstring(n) or "").split())
            for n in ast.parse(stub.read_text()).body
            if isinstance(n, ast.FunctionDef)
        }
        for name in stubs:
            if not name.startswith(("int_", "bool_", "float_")) or name in (
                "int_const", "int_const_any_width", "bool_const", "float_const",
                "int_binary", "bool_binary", "float_binary", "bool_inputs",
            ):
                continue
            runtime = " ".join((getattr(mod, name).__doc__ or "").split())
            if runtime != stubs[name]:
                differ[f"{mod.__name__}.{name}"] = (runtime, stubs[name])
    assert not differ, differ
