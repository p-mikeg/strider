"""The shipped `.pyi` stubs must describe the module that actually ships.

Nothing else enforces this. The stubs are hand-written, so a new binding
reaches users' type checkers only if someone remembers to hand-edit the
matching stub. Nobody remembered for a while: `strider.pattern` shipped
1.0 missing `entry`, `region`, `indirect_branch`, `switch`,
`unreachable`, `CallOutputPat`, `CallPat.mem` and `CallPat.output`.

Both directions are checked: a runtime name with no stub entry makes
correct user code fail type-checking, and a stub entry with no runtime
name promises an API that raises `AttributeError`.

A bare module-level assignment (`PatLike = Union[...]`) is a type alias,
stub-only by nature, so it is exempt from the second direction rather
than needing an allowlist to grow.
"""

from __future__ import annotations

import ast
import importlib
import pathlib

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
}

PKG_ROOT = pathlib.Path(strider.__file__).parent


def _stub_ast(rel: str) -> ast.Module:
    return ast.parse((PKG_ROOT / rel).read_text())


def _declared(tree: ast.Module) -> tuple[set[str], set[str]]:
    """`(callable_or_class_names, alias_names)` declared at module level.

    Aliases are bare assignments (type aliases); re-exports bound by
    `from . import x as x` / `from .m import X` count as declarations
    because that is how the stub publishes a name it doesn't define.
    """
    real: set[str] = set()
    aliases: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            real.add(node.name)
        elif isinstance(node, ast.Assign):
            aliases.update(t.id for t in node.targets if isinstance(t, ast.Name))
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            aliases.add(node.target.id)
        elif isinstance(node, ast.ImportFrom):
            aliases.update(a.asname or a.name for a in node.names)
    return real, aliases


def _class_members(tree: ast.Module) -> dict[str, set[str]]:
    """Per-class member names declared in the stub (methods + attributes)."""
    out: dict[str, set[str]] = {}
    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        members: set[str] = set()
        for item in node.body:
            if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                members.add(item.name)
            elif isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                members.add(item.target.id)
        out[node.name] = members
    return out


def _public(names) -> set[str]:
    return {n for n in names if not n.startswith("_")}


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_every_public_runtime_name_is_declared_in_the_stub(mod_name):
    """Shipping a symbol with no stub entry makes correct user code fail
    type-checking."""
    module = importlib.import_module(mod_name)
    real, aliases = _declared(_stub_ast(MODULES[mod_name]))
    missing = _public(dir(module)) - real - aliases
    assert not missing, (
        f"{mod_name} exports {sorted(missing)} at runtime but the stub "
        f"{MODULES[mod_name]} does not declare them — type checkers will "
        f"reject correct user code"
    )


@pytest.mark.parametrize("mod_name", sorted(MODULES))
def test_every_stub_declaration_exists_at_runtime(mod_name):
    """A stub entry with no runtime symbol promises an API that raises
    `AttributeError`."""
    module = importlib.import_module(mod_name)
    real, _aliases = _declared(_stub_ast(MODULES[mod_name]))
    phantom = {n for n in real if not hasattr(module, n)}
    assert not phantom, (
        f"{MODULES[mod_name]} declares {sorted(phantom)} but {mod_name} has "
        f"no such attribute — the stub promises an API that does not exist"
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
