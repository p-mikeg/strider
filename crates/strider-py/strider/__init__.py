"""Python bindings for the Strider binary analysis pipeline.

The public API is the domain submodules `strider.ir`, `strider.lift`,
`strider.cfg`, `strider.sleigh`, `strider.reader`, `strider.opt`,
`strider.pattern` and `strider.template`, plus the top-level
`StriderError`.
"""

import importlib as _importlib
import sys as _sys
import typing as _t

_ext = _importlib.import_module("strider._strider")

if _t.TYPE_CHECKING:
    # The loop below binds these at runtime; the static import is what a type
    # checker sees, so the stubs type the aliases built out of them.
    from . import cfg, ir, lift, opt, pattern, reader, sleigh, template

for _name in ("ir", "lift", "cfg", "sleigh", "reader", "opt", "pattern", "template"):
    _sub = getattr(_ext, _name)
    _sys.modules[f"strider.{_name}"] = _sub
    globals()[_name] = _sub

# Give `strider.pattern.constraints` its own `sys.modules` key so
# `import strider.pattern.constraints` resolves.
_sys.modules["strider.pattern.constraints"] = _ext.pattern.constraints

StriderError = _ext.StriderError
__version__ = _ext.__version__

# Bind the pure-Python facade members into their home submodule.
from . import _api as _mod_api  # noqa: E402

# `explore` is a submodule, not an attribute the extension binds; import it so
# `strider.explore.shutdown(port)` works without `Lifter.visualize` having run.
from . import explore as explore  # noqa: E402

lift.ElfLifter = _mod_api.ElfLifter          # type: ignore[attr-defined]
lift.load_elf = _mod_api.load_elf            # type: ignore[attr-defined]

# The type aliases live in the .pyi stubs; also bind them at runtime so
# `p.PatLike` works in an annotation evaluated without
# `from __future__ import annotations`, and so they can be imported.
_p = pattern
_p.PatLike = _t.Union[  # type: ignore[attr-defined]
    int, _p.Capture, _p.Pat, _p.CallPat, _p.CallOtherPat, _p.RetPat,
    _p.IfPat, _p.LoadPat, _p.StorePat, _p.PhiPat, _p.MemPhiPat, _p.EntryPat,
    _p.RegionPat, _p.IndirectBranchPat, _p.UnreachablePat, _p.SwitchPat,
    _p.FunctionArgPat, _p.IntBinaryPat, _p.FloatBinaryPat, _p.BoolBinaryPat,
]
_p.CaptureKey = _t.Union[_p.Capture, str]  # type: ignore[attr-defined]
_p.constraints.ConstraintLike = _t.Union[  # type: ignore[attr-defined]
    _p.constraints.JoinConstraint, _p.constraints.JoinPredicate
]
_p.ValueTy = _t.Literal[  # type: ignore[attr-defined]
    "i1", "i8", "i16", "i24", "i32", "i40", "i48", "i56", "i64", "i72",
    "i80", "i96", "i112", "i128", "i256", "i512", "f16", "f32", "f64", "f80",
    "f128",
    "I1", "I8", "I16", "I24", "I32", "I40", "I48", "I56", "I64", "I72",
    "I80", "I96", "I112", "I128", "I256", "I512", "F16", "F32", "F64", "F80",
    "F128",
]
_p.IntCmpOpName = _t.Literal[  # type: ignore[attr-defined]
    "Equal", "Less", "Sless", "Carry", "Scarry", "Sborrow", "eq", "lt", "slt",
]
_p.ExtendOpName = _t.Literal[  # type: ignore[attr-defined]
    "zero", "zero_extend", "ZeroExtend", "sign", "sign_extend", "SignExtend",
]

# PyO3's `#[pyclass(extends = ...)]` takes exactly one base, so the builders
# cannot share a Rust base class. The shared vocabulary is declared here as
# structural protocols instead: `isinstance` answers what a caller actually
# wants to know, and the `.pyi` stubs list them as the builders' bases.
_S = _t.TypeVar("_S")


@_t.runtime_checkable
class _NodePat(_t.Protocol):
    """A node-pattern builder: captures, guards, and seals into a `Pat`."""

    def capture(self: _S, c: "_p.CaptureKey") -> _S:
        """Capture the matched node under `c`, a `Capture` or a name."""
        ...

    def when(self: _S, f: _t.Callable[["_p.Match"], bool]) -> _S:
        """Attach a Python predicate that runs after the match."""
        ...

    def into_pat(self) -> "_p.Pat":
        """Finalise into a `Pat`."""
        ...


@_t.runtime_checkable
class _InputPat(_t.Protocol):
    """A builder whose node kind has input slots."""

    def input(self: _S, idx: int, p: "_p.PatLike") -> _S:
        """Match `p` against raw input slot `idx`."""
        ...

    def any_input(self: _S, p: "_p.PatLike") -> _S:
        """Require SOME input of the node to match `p`."""
        ...


@_t.runtime_checkable
class _CtrlPat(_t.Protocol):
    """A builder whose node kind has a control-predecessor input."""

    def ctrl(self: _S, p: "_p.PatLike") -> _S:
        """Match `p` against the node's direct ctrl predecessor."""
        ...


@_t.runtime_checkable
class _MemPat(_t.Protocol):
    """A builder whose node kind has a memory input."""

    def mem(self: _S, p: "_p.PatLike") -> _S:
        """Match `p` against the node's memory predecessor."""
        ...


@_t.runtime_checkable
class _MemAccessPat(_t.Protocol):
    """A builder for a node kind that addresses memory: `load` / `store`."""

    def addr(self: _S, p: "_p.PatLike") -> _S:
        """Constrain the address operand."""
        ...

    def bit_width(self: _S, n: int) -> _S:
        """Filter by accessed-value width in bits."""
        ...

    def space(self: _S, s: "sleigh.VnSpace") -> _S:
        """Restrict the match to a specific memory space."""
        ...

    def stack_offset(self: _S, k: int) -> _S:
        """Match only accesses whose address decomposes to exactly `sp + k`."""
        ...

    def stack_only(self: _S) -> _S:
        """Reject matches where the SP-relative offset is unknown."""
        ...

    def non_stack(self: _S) -> _S:
        """Reject matches proven to be a stack slot."""
        ...

    def heap_only(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a heap base."""
        ...


@_t.runtime_checkable
class _OrderedPat(_t.Protocol):
    """A pattern over a commutative op, which `ordered` can pin."""

    def ordered(self: _S) -> _S:
        """Stop the matcher retrying this op with its operands swapped."""
        ...


@_t.runtime_checkable
class _OutputPat(_t.Protocol):
    """A builder whose node kind has output slots."""

    def output(self, slot: int) -> "_p.OutputSlotPat":
        """Bind or constrain the value at raw output `slot`."""
        ...

    def any_output(self) -> "_p.OutputSlotPat":
        """Some output rather than a fixed slot."""
        ...


for _proto, _proto_name in (
    (_NodePat, "NodePat"), (_InputPat, "InputPat"), (_CtrlPat, "CtrlPat"),
    (_MemPat, "MemPat"), (_MemAccessPat, "MemAccessPat"),
    (_OrderedPat, "OrderedPat"), (_OutputPat, "OutputPat"),
):
    _proto.__name__ = _proto.__qualname__ = _proto_name
    _proto.__module__ = "strider.pattern"
    setattr(_p, _proto_name, _proto)
    _p.__all__.append(_proto_name)

# The output terminal is shared by every builder that has outputs; the name
# `CallPat.output` returned in 0.2.0 still resolves.
_p.CallOutputPat = _p.OutputSlotPat  # type: ignore[attr-defined]
_p.__all__.append("CallOutputPat")
reader.MemLike = _t.Union[reader.MemReader, reader.BufferReader]  # type: ignore[attr-defined]
reader.RomLike = _t.Union[reader.ReadOnlyMemory, reader.BufferReader]  # type: ignore[attr-defined]
template.TemplateLike = _t.Union[  # type: ignore[attr-defined]
    template.Template, _p.Pat, _p.Capture, int
]
cfg.DotStyle = _t.Literal["dark", "dark_cfg", "empty"]  # type: ignore[attr-defined]
lift.StrPath = _mod_api.StrPath              # type: ignore[attr-defined]
lift.AliasMode = _t.Literal["stack_global_disjoint", "strict"]  # type: ignore[attr-defined]
opt.OptimizerPass = _t.Union[tuple(  # type: ignore[attr-defined]
    getattr(opt, _n) for _n in (
        "ConstantFold", "KnownBits", "FlagCmpCanonicalize", "IfCondInversion",
        "PhiCollapse", "RegionCollapse", "DeadBranchElimination", "CfgDetach",
        "LoadForward", "StackOffsetDetect", "FunctionArgDetect",
        "CallStackArgCollect", "LoadReadOnly",
    )
)]

def _banner() -> None:
    """Print the wordmark once, on an interactive import.

    Goes to stderr, and only when stderr is a terminal. `STRIDER_NO_BANNER`
    silences it either way.
    """
    import os

    if os.environ.get("STRIDER_NO_BANNER") or not _sys.stderr.isatty():
        return
    # figlet -f slant, down a green ramp.
    art = [
        r"         __       _     __",
        r"   _____/ /______(_)___/ /__  _____",
        r"  / ___/ __/ ___/ / __  / _ \/ ___/",
        r" (__  ) /_/ /  / / /_/ /  __/ /",
        r"/____/\__/_/  /_/\__,_/\___/_/",
    ]
    ramp = (118, 82, 46, 42, 36)
    out = ["\n"]
    out += [f"  \033[38;5;{c}m{line}\033[0m\n" for c, line in zip(ramp, art)]
    _sys.stderr.write("".join(out))


_banner()


__all__ = ["StriderError", "ir", "lift", "cfg", "sleigh", "reader",
           "opt", "pattern", "template"]

# Drop import machinery from the public namespace, including the
# `strider._strider` / `strider._api` attributes the import system binds.
del _importlib, _sys, _ext, _name, _sub, _mod_api, _t, _p, _banner
del _S, _NodePat, _InputPat, _CtrlPat, _MemPat, _MemAccessPat, _OrderedPat
del _OutputPat, _proto, _proto_name
# No statement here binds these two; they go through the module dict.
del globals()["_strider"], globals()["_api"]
