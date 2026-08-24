"""The shipped `.pyi` stubs must describe the module that actually ships.

Nothing else enforces this. The stubs are hand-written, so a new binding
reaches users' type checkers only if someone remembers to hand-edit the
matching stub. Nobody remembered for a while: `strider.pattern` shipped
1.0 missing `entry`, `region`, `indirect_branch`, `switch`,
`unreachable`, `CallOutputPat`, `CallPat.mem` and `CallPat.output`.

Both directions are checked: a runtime name with no stub entry makes
correct user code fail type-checking, and a stub entry with no runtime
name promises an API that raises `AttributeError`.

A bare module-level assignment (`PatLike = Union[...]`) is a type alias.
It is exempt from the class/function direction, but `strider` binds its
aliases at runtime too so they can be imported and used in an annotation
evaluated eagerly, and a third check holds them to that. The same check
covers an explicit `from . import x as x` re-export, which promises an
attribute on the package.
"""

from __future__ import annotations

import ast
import importlib
import inspect
import pathlib
from typing import Optional

import pytest

import strider

#: Public submodules, each with its stub relative to the package root.
#: `pattern` is a package, so its stub is the `__init__.pyi`.
MODULES = {
    "strider": "__init__.pyi",
    "strider.cfg": "cfg.pyi",
    "strider.ir": "ir.pyi",
    "strider.lift": "lift.pyi",
    "strider.opt": "opt.pyi",
    "strider.reader": "reader.pyi",
    "strider.sleigh": "sleigh.pyi",
    "strider.template": "template.pyi",
    "strider.pattern": "pattern/__init__.pyi",
    "strider.pattern.constraints": "pattern/constraints.pyi",
    # Pure Python in a py.typed package: the source IS what a checker reads.
    "strider.explore": "explore.py",
}

PKG_ROOT = pathlib.Path(strider.__file__).parent


def _stub_ast(rel: str) -> ast.Module:
    return ast.parse((PKG_ROOT / rel).read_text())


def _declared(tree: ast.Module) -> tuple[set[str], set[str]]:
    """`(callable_or_class_names, alias_names)` declared at module level.

    Aliases are bare assignments (type aliases); re-exports bound by
    `from . import x as x` / `from .m import X` count as declarations
    because that is how the stub publishes a name it doesn't define. So do
    imports under `if TYPE_CHECKING:`, which a checker reads and a
    py.typed source module (`explore.py`) uses to name a type without
    importing it at runtime.
    """
    real: set[str] = set()
    aliases: set[str] = set()
    for node in tree.body:
        if (
            isinstance(node, ast.If)
            and isinstance(node.test, ast.Name)
            and node.test.id == "TYPE_CHECKING"
        ):
            for item in node.body:
                if isinstance(item, ast.ImportFrom):
                    aliases.update(a.asname or a.name for a in item.names)
                elif isinstance(item, ast.Import):
                    aliases.update(
                        a.asname or a.name.split(".")[0] for a in item.names
                    )
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            real.add(node.name)
        elif isinstance(node, ast.Assign):
            aliases.update(t.id for t in node.targets if isinstance(t, ast.Name))
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            aliases.add(node.target.id)
        elif isinstance(node, ast.ImportFrom):
            aliases.update(a.asname or a.name for a in node.names)
        elif isinstance(node, ast.Import):
            aliases.update(a.asname or a.name.split(".")[0] for a in node.names)
    return real, aliases


def _bound_names(tree: ast.Module) -> set[str]:
    """Names the stub promises as module attributes: type aliases assigned at
    module level, plus `from . import x as x` re-exports."""
    out: set[str] = set()
    for node in tree.body:
        if isinstance(node, ast.Assign):
            out.update(t.id for t in node.targets if isinstance(t, ast.Name))
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            out.add(node.target.id)
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            out.update(
                a.asname
                for a in node.names
                if a.asname is not None and a.asname == a.name
            )
    return out


def _class_members(tree: ast.Module) -> dict[str, set[str]]:
    """Per-class member names declared in the stub (methods + attributes),
    including those a stub base class declares.

    PyO3's `#[pyclass(extends = ...)]` takes exactly one base, so the pattern
    builders share their vocabulary through `typing.Protocol` mixins the stub
    lists as bases while the runtime class carries every method itself. Folding
    the bases in is what lets one stub declaration answer for all of them.
    """
    own: dict[str, set[str]] = {}
    bases: dict[str, list[str]] = {}
    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        members: set[str] = set()
        for item in node.body:
            if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                members.add(item.name)
            elif isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                members.add(item.target.id)
            elif isinstance(item, ast.Assign):
                members.update(t.id for t in item.targets if isinstance(t, ast.Name))
        own[node.name] = members
        bases[node.name] = [b.id for b in node.bases if isinstance(b, ast.Name)]

    def resolve(name: str, seen: frozenset[str]) -> set[str]:
        if name in seen:
            return set()
        out = set(own.get(name, ()))
        for base in bases.get(name, ()):
            out |= resolve(base, seen | {name})
        return out

    return {name: resolve(name, frozenset()) for name in own}


#: Owner key for a function declared at module level rather than in a class.
_MODULE = "<module>"

#: One declared parameter: its name, whether it is optional, and its default
#: source when the stub spells one out (`...`, the conventional `.pyi`
#: placeholder, says "optional" without naming the value).
Param = tuple[str, bool, Optional[str]]


def _stub_params(args: ast.arguments) -> list[Param]:
    positional = args.posonlyargs + args.args
    padding: list[ast.expr | None] = [None] * (len(positional) - len(args.defaults))
    pairs = list(zip(positional, padding + list(args.defaults)))
    pairs += list(zip(args.kwonlyargs, args.kw_defaults))
    out: list[Param] = []
    for p, d in pairs:
        if p.arg in ("self", "cls"):
            continue
        src = None if d is None else ast.unparse(d)
        out.append((p.arg, d is not None, None if src == "..." else src))
    return out


def _stub_method_params(
    tree: ast.Module,
) -> dict[tuple[str, str], list[Param]]:
    """`(owner, function) -> declared parameters`, `self` / `cls` dropped.

    Module-level functions are keyed under `_MODULE`: they are most of the
    `strider.pattern` surface, and skipping them exempted every builder.
    """
    out: dict[tuple[str, str], list[Param]] = {}

    def collect(owner: str, item: ast.stmt) -> None:
        if not isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return
        a = item.args
        if a.vararg is not None or a.kwarg is not None:
            return  # pyo3 emits no signature for these; nothing to compare
        out[(owner, item.name)] = _stub_params(a)

    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            for item in node.body:
                collect(node.name, item)
        else:
            collect(_MODULE, node)
    return out


def _runtime_default(p: inspect.Parameter) -> Optional[str]:
    if p.default is inspect.Parameter.empty or p.default is Ellipsis:
        return None
    return repr(p.default)


def _runtime_params(fn) -> Optional[list[Param]]:
    """Parameters pyo3 published, or `None` when it published none.

    A pyo3 `#[new]` fills `tp_new`, leaving `__init__` as `object.__init__`
    (`*args, **kwargs`); pass the CLASS to read the constructor's own
    signature.
    """
    try:
        sig = inspect.signature(fn)
    except (ValueError, TypeError):
        return None
    kinds = {p.kind for p in sig.parameters.values()}
    if kinds & {inspect.Parameter.VAR_POSITIONAL, inspect.Parameter.VAR_KEYWORD}:
        return None
    return [
        (n, p.default is not inspect.Parameter.empty, _runtime_default(p))
        for n, p in sig.parameters.items()
        if n not in ("self", "cls")
    ]


def _public(names) -> set[str]:
    return {n for n in names if not n.startswith("_")}


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_every_public_runtime_name_is_declared_in_the_stub(mod_name):
    module = importlib.import_module(mod_name)
    real, aliases = _declared(_stub_ast(MODULES[mod_name]))
    missing = _public(dir(module)) - real - aliases
    assert not missing, (
        f"{mod_name} exports {sorted(missing)} at runtime but the stub "
        f"{MODULES[mod_name]} does not declare them, so type checkers will "
        f"reject correct user code"
    )


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_every_stub_declaration_exists_at_runtime(mod_name):
    module = importlib.import_module(mod_name)
    real, _aliases = _declared(_stub_ast(MODULES[mod_name]))
    phantom = {n for n in real if not hasattr(module, n)}
    assert not phantom, (
        f"{MODULES[mod_name]} declares {sorted(phantom)} but {mod_name} has "
        f"no such attribute: the stub promises an API that does not exist"
    )


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_every_stub_alias_is_bound_at_runtime(mod_name):
    """`from strider.template import TemplateLike` must not be an
    ImportError, and `strider.explore` must not be an AttributeError."""
    module = importlib.import_module(mod_name)
    declared = _public(_bound_names(_stub_ast(MODULES[mod_name])))
    unbound = {n for n in declared if not hasattr(module, n)}
    assert not unbound, (
        f"{MODULES[mod_name]} declares {sorted(unbound)} but {mod_name} does "
        f"not bind them at runtime, so importing them raises"
    )


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_class_members_match_the_stub(mod_name):
    """Same rule one level down: a builder that grows a chainable method
    (`CallPat.output`) is the exact drift that shipped in 1.0."""
    module = importlib.import_module(mod_name)
    for cls_name, declared in _class_members(_stub_ast(MODULES[mod_name])).items():
        cls = getattr(module, cls_name, None)
        if cls is None or not isinstance(cls, type):
            continue  # covered by the phantom-declaration test above
        # `vars`, not `dir`: only members the class itself defines. Inherited
        # ones belong to the base's stub entry (`class ElfLifter(Lifter)`
        # already promises Lifter's methods), and `dir` would otherwise
        # demand every `Exception`/`object` member be restated.
        runtime = _public(vars(cls))
        missing = runtime - declared
        assert not missing, (
            f"{mod_name}.{cls_name} has {sorted(missing)} at runtime but the "
            f"stub does not declare them"
        )
        # Dunders are exempt going the other way: the stub declares only the
        # few that are meaningful, while `dir()` reports every inherited one.
        phantom = {
            m for m in declared - runtime if not m.startswith("__")
        } - {m for m in declared if hasattr(cls, m)}
        assert not phantom, (
            f"{mod_name}.{cls_name} stub declares {sorted(phantom)} but the "
            f"runtime class has no such member"
        )


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_function_signatures_match_the_stub(mod_name):
    """Names and default VALUES, not just existence: a keyword argument the
    stub declares must be the one the binding accepts, or
    `LoadPat.bit_width(n=32)` is a TypeError against a stub that says it is
    fine, and a stub default the binding does not share is a silent lie."""
    module = importlib.import_module(mod_name)
    wrong = []
    for (owner_name, method), declared in _stub_method_params(
        _stub_ast(MODULES[mod_name])
    ).items():
        if owner_name == _MODULE:
            owner = module
        else:
            owner = getattr(module, owner_name, None)
            if not isinstance(owner, type):
                continue
        if method == "__init__":
            # `#[new]` fills `tp_new`; the class itself carries the signature.
            fn = owner if owner is not module else None
        elif method.startswith("__"):
            # pyo3 names every operator operand `value`, and no other dunder
            # is called by keyword.
            continue
        else:
            fn = getattr(owner, method, None)
        if fn is None:
            continue
        runtime = _runtime_params(fn)
        if runtime is None:
            continue
        where = f"{owner_name}.{method}"
        if [n for n, _, _ in declared] != [n for n, _, _ in runtime]:
            wrong.append(f"{where}: stub {declared} != runtime {runtime}")
            continue
        for (name, opt, want), (_, got_opt, got) in zip(declared, runtime):
            if opt != got_opt:
                state = "optional" if opt else "required"
                wrong.append(f"{where}({name}=): stub {state}, runtime is not")
            elif want is not None and want != got:
                wrong.append(f"{where}({name}=): stub {want}, runtime {got}")
    assert not wrong, (
        f"{MODULES[mod_name]} declares parameters the module does not "
        f"accept as keywords, or defaults / optionality it does not share: "
        f"{wrong}"
    )


def _annotation_names(tree: ast.Module) -> set[str]:
    """Every bare name a stub annotation refers to, forward references
    (`-> "LoadPat"`) parsed too. A `Literal[...]` subscript is skipped: its
    strings are values, not type names."""
    out: set[str] = set()

    def visit(node: ast.expr | None) -> None:
        if node is None:
            return
        if isinstance(node, ast.Constant):
            if isinstance(node.value, str):
                try:
                    visit(ast.parse(node.value, mode="eval").body)
                except SyntaxError:
                    pass
            return
        if isinstance(node, ast.Name):
            out.add(node.id)
            return
        if isinstance(node, ast.Subscript):
            visit(node.value)
            head = node.value
            if isinstance(head, ast.Name) and head.id == "Literal":
                return
            visit(node.slice)
            return
        if isinstance(node, ast.Tuple):
            for e in node.elts:
                visit(e)
            return
        if isinstance(node, ast.Attribute):
            visit(node.value)
            return
        if isinstance(node, ast.BinOp):
            visit(node.left)
            visit(node.right)
            return

    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            visit(node.returns)
            for arg in node.args.posonlyargs + node.args.args + node.args.kwonlyargs:
                visit(arg.annotation)
        elif isinstance(node, ast.AnnAssign):
            visit(node.annotation)
    return out


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_every_annotated_type_resolves(mod_name):
    """A stub that annotates a type it never declares or imports type-checks
    as `Any` at best and errors at worst; either way the annotation is a
    decoration rather than a promise."""
    import builtins

    tree = _stub_ast(MODULES[mod_name])
    real, aliases = _declared(tree)
    known = real | aliases | set(dir(builtins))
    # Nested class bodies and quoted forward references may name anything the
    # module declares, so no scoping beyond module level is needed.
    unknown = {n for n in _annotation_names(tree) if n not in known}
    assert not unknown, (
        f"{MODULES[mod_name]} annotates {sorted(unknown)}, which it neither "
        f"declares nor imports"
    )


def _literal_aliases(tree: ast.Module) -> dict[str, tuple]:
    """`name -> the literal values` for each module-level `X = Literal[...]`."""
    out: dict[str, tuple] = {}
    for node in tree.body:
        if not isinstance(node, ast.Assign) or len(node.targets) != 1:
            continue
        target = node.targets[0]
        value = node.value
        if not isinstance(target, ast.Name) or not isinstance(value, ast.Subscript):
            continue
        head = value.value
        if not (isinstance(head, ast.Name) and head.id == "Literal"):
            continue
        items = value.slice.elts if isinstance(value.slice, ast.Tuple) else [value.slice]
        out[target.id] = tuple(
            i.value for i in items if isinstance(i, ast.Constant)
        )
    return out


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_literal_aliases_hold_the_same_values_at_runtime(mod_name):
    """`ValueTy` and friends are written twice, in the stub and in
    `strider/__init__.py`; existence alone lets the two drift."""
    module = importlib.import_module(mod_name)
    import typing

    for name, want in _literal_aliases(_stub_ast(MODULES[mod_name])).items():
        runtime = getattr(module, name, None)
        assert runtime is not None, f"{mod_name}.{name} is stub-only"
        assert typing.get_args(runtime) == want, (
            f"{mod_name}.{name} lists {want} in the stub but "
            f"{typing.get_args(runtime)} at runtime"
        )


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_dunder_all_is_bound_and_declared(mod_name):
    """`__all__` decides what `from strider import *` gives and what a
    checker treats as re-exported, so it needs both directions too."""
    module = importlib.import_module(mod_name)
    names = getattr(module, "__all__", None)
    if names is None:
        return
    real, aliases = _declared(_stub_ast(MODULES[mod_name]))
    unbound = [n for n in names if not hasattr(module, n)]
    assert not unbound, f"{mod_name}.__all__ names unbound {unbound}"
    undeclared = [n for n in names if n not in real and n not in aliases]
    assert not undeclared, (
        f"{mod_name}.__all__ exports {undeclared}, which "
        f"{MODULES[mod_name]} does not declare"
    )
