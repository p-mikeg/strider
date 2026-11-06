"""Strider: lift a native function to a sea-of-nodes IR and query it.

The API is the submodules `strider.ir`, `strider.lift`, `strider.cfg`,
`strider.sleigh`, `strider.reader`, `strider.opt`, `strider.pattern` and
`strider.template`, plus `StriderError`.
"""

import importlib as _importlib
import sys as _sys
import typing as _t

_ext = _importlib.import_module("strider._strider")

if _t.TYPE_CHECKING:
    # Bound by the loop below at runtime.
    from . import cfg, ir, lift, opt, pattern, reader, sleigh, template

# The extension's submodules have no docstring of their own.
_SUBMODULE_DOCS = {
    "ir": "The sea-of-nodes IR: `Function`, `Node`, and their readers.",
    "lift": "Lift, optimise and resolve: the `Lifter` handle, its options, "
            "and the ELF loaders.",
    "cfg": "The control-flow graph and its options.",
    "sleigh": "Architecture, calling convention, user-op ABI and varnode "
              "surface.",
    "reader": "The code and read-only-memory readers, and the ELF symbol "
              "table they come with.",
    "opt": "The optimizer pipeline and its passes.",
    "pattern": "The match side of a query: the pattern DSL.",
    "template": "The build side of a rewrite: the `Template` DSL.",
}

for _name in ("ir", "lift", "cfg", "sleigh", "reader", "opt", "pattern", "template"):
    _sub = getattr(_ext, _name)
    _sub.__doc__ = _SUBMODULE_DOCS[_name]
    _sys.modules[f"strider.{_name}"] = _sub
    globals()[_name] = _sub

# So `import strider.pattern.constraints` resolves.
_sys.modules["strider.pattern.constraints"] = _ext.pattern.constraints
_ext.pattern.constraints.__doc__ = (
    "Relational predicates over the captures a pattern binds."
)

StriderError = _ext.StriderError
__version__ = _ext.__version__


def _export(module, *names: str) -> None:
    """Add `names` to `module.__all__` once (the extension submodules survive
    `importlib.reload(strider)`)."""
    module.__all__ += [n for n in names if n not in module.__all__]

from . import _api as _mod_api  # noqa: E402

# So `strider.explore` works without an explicit import.
from . import explore as explore  # noqa: E402

for _sub, _names in ((lift, ("ElfLifter", "load_elf")),
                     (pattern, ("FieldPat", "field", "code_ptr"))):
    for _name in _names:
        getattr(_mod_api, _name).__module__ = f"strider.{_sub.__name__}"
        setattr(_sub, _name, getattr(_mod_api, _name))
    _export(_sub, *_names)

# The stubs' type aliases, bound at runtime too so they can be imported and
# evaluated in annotations.
_p = pattern
_p.PatLike = _t.Union[  # type: ignore[attr-defined]
    int, _p.Capture, _p.Pat, _p.CallPat, _p.CallOtherPat, _p.RetPat,
    _p.IfPat, _p.LoadPat, _p.StorePat, _p.PhiPat, _p.MemPhiPat, _p.EntryPat,
    _p.RegionPat, _p.IndirectBranchPat, _p.UnreachablePat, _p.SwitchPat,
    _p.FunctionArgPat, _p.IntBinaryPat, _p.FloatBinaryPat, _p.BoolBinaryPat,
]
# A value operand slot: builders producing no value are excluded.
_p.ValueLike = _t.Union[  # type: ignore[attr-defined]
    int, _p.Capture, _p.Pat, _p.CallPat, _p.CallOtherPat, _p.LoadPat,
    _p.PhiPat, _p.FunctionArgPat, _p.IntBinaryPat, _p.FloatBinaryPat,
    _p.BoolBinaryPat,
]
# A memory slot: the four memory producers or a wildcard.
_p.MemLike = _t.Union[  # type: ignore[attr-defined]
    _p.Capture, _p.Pat, _p.StorePat, _p.MemPhiPat, _p.CallPat, _p.CallOtherPat,
]
_p.CaptureKey = _t.Union[_p.Capture, str]  # type: ignore[attr-defined]
_p.constraints.ConstraintLike = _t.Union[  # type: ignore[attr-defined]
    _p.constraints.JoinConstraint, _p.constraints.JoinPredicate
]
# Built from the name rosters Rust publishes.
_p.ValueTy = _t.Literal[_p.VALUE_TYPES]  # type: ignore[attr-defined]
_p.IntCmpOpName = _t.Literal[_p.INT_CMP_OPS]  # type: ignore[attr-defined]
_p.IntBinaryOpName = _t.Literal[_p.INT_BINARY_OPS]  # type: ignore[attr-defined]
_p.BoolBinaryOpName = _t.Literal[_p.BOOL_BINARY_OPS]  # type: ignore[attr-defined]
_p.FloatBinaryOpName = _t.Literal[_p.FLOAT_BINARY_OPS]  # type: ignore[attr-defined]
_p.ExtendOpName = _t.Literal[_p.EXTEND_OPS]  # type: ignore[attr-defined]
_export(
    _p, "PatLike", "ValueLike", "MemLike", "CaptureKey", "ValueTy",
    "IntCmpOpName", "IntBinaryOpName", "BoolBinaryOpName", "FloatBinaryOpName",
    "ExtendOpName",
)
_export(_p.constraints, "ConstraintLike")

# `Cfg.incomplete` names each reason by one of these.
cfg.IncompleteReason = _t.Literal[cfg.INCOMPLETE_REASONS]  # type: ignore[attr-defined]
_export(cfg, "IncompleteReason")

# The builders' shared methods as protocols: a pyclass takes one base, so
# they cannot share a Rust one. The stubs list these as the builders' bases.
_S = _t.TypeVar("_S")


@_t.runtime_checkable
class _NodePat(_t.Protocol):
    """A node-pattern builder: captures, guards, and seals into a `Pat`."""

    def capture(self: _S, c: "pattern.CaptureKey") -> _S:
        """Bind the matched node to `c`, a `Capture` or a name."""
        ...

    def when(self: _S, f: _t.Callable[["pattern.Match"], bool]) -> _S:
        """Keep a match only when `f` returns `True`; a rejected binding
        backtracks. A non-`bool` return is a `TypeError`. Raising aborts the
        query and re-raises out of `find_all` / `find_unique`. `f` may run on
        a partial match, more than once, or not at all."""
        ...

    def into_pat(self) -> "pattern.Pat":
        """Finalise to a reusable `Pat`. A wrong-typed operand raises here; one
        that can never match its slot raises at the first query. Rarely
        needed: a builder auto-finalises where a pattern is expected."""
        ...


@_t.runtime_checkable
class _InputPat(_t.Protocol):
    """A builder whose node kind exposes raw input slots."""

    def input(self: _S, idx: int, p: "pattern.PatLike") -> _S:
        """Match `p` against raw input slot `idx`: `Call` is `[ctrl, mem,
        target, sp, arg0, ...]`, `Load` `[mem, addr]`, `If` `[ctrl, cond]`.

        A typed value sub-pattern binds only a value edge; `var` /
        `anything` any edge; a control producer (`entry()`, `region()`, ...)
        its control edge; a `call()` / `call_other()` any of its outputs
        (`.res()` keeps it to the value).
        """
        ...

    def any_input(self: _S, p: "pattern.PatLike") -> _S:
        """Require some input of the node to match `p`, without pinning a
        slot; binding follows `input`. Repeatable, each call a distinct
        slot."""
        ...


@_t.runtime_checkable
class _CtrlPat(_t.Protocol):
    """A builder whose node kind has a control-predecessor input."""

    def ctrl(self: _S, p: "pattern.PatLike") -> _S:
        """Match `p` against the node's direct ctrl predecessor
        (`inputs[0]`)."""
        ...


@_t.runtime_checkable
class _MemPat(_t.Protocol):
    """A builder whose node kind has a memory input."""

    def mem(self: _S, p: "pattern.MemLike") -> _S:
        """Match `p` against the node's memory predecessor; takes a memory
        producer (`store` / `mem_phi` / `call` / `call_other`)."""
        ...


@_t.runtime_checkable
class _MemAccessPat(_t.Protocol):
    """A builder whose node kind addresses memory: `load()` and `store()`.

    `stack_offset`, `stack_only`, `non_stack` and `heap_only` set one region
    filter; the last one called wins, except that `stack_only` keeps an
    offset an earlier `stack_offset` pinned.
    """

    def addr(self: _S, p: "pattern.ValueLike") -> _S:
        """Constrain the address operand (`inputs[1]`)."""
        ...

    def bit_width(self: _S, n: int) -> _S:
        """Filter by accessed-value width in bits. A width no value type has
        raises `StriderError`."""
        ...

    def space(self: _S, s: "sleigh.VnSpace") -> _S:
        """Restrict the match to a specific memory space."""
        ...

    def stack_offset(self: _S, k: int) -> _S:
        """Match only accesses whose address decomposes to exactly
        `sp + k`."""
        ...

    def stack_only(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a stack base.
        Keeps an earlier `stack_offset`; replaces any other filter."""
        ...

    def non_stack(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a heap base or is
        proven not memory-rooted; an undecided address is rejected."""
        ...

    def heap_only(self: _S) -> _S:
        """Keep only accesses whose address decomposes to a heap base (a
        pure allocator's return pointer)."""
        ...


@_t.runtime_checkable
class _OrderedPat(_t.Protocol):
    """A pattern whose root takes an operand pair `ordered` can pin."""

    def ordered(self: _S) -> _S:
        """Stop the matcher retrying this op with its operands swapped. Pins
        this node alone (for `int_ne` / `float_ne`, the `Equal` under the
        `Xor`). On a `Pat` with no operands to order (a wildcard, a constant,
        a `one_of`) it raises."""
        ...


@_t.runtime_checkable
class _OutputPat(_t.Protocol):
    """A builder whose node kind exposes raw output slots."""

    def output(self, slot: int) -> "pattern.OutputSlotPat":
        """Bind or constrain the value at raw output `slot` (a `Call` is
        `[ctrl, mem, result, ...clobbers]`, a `Load` `[value]`)."""
        ...

    def any_output(self) -> "pattern.OutputSlotPat":
        """Like `output`, over some output rather than a fixed slot."""
        ...


for _proto, _proto_name in (
    (_NodePat, "NodePat"), (_InputPat, "InputPat"), (_CtrlPat, "CtrlPat"),
    (_MemPat, "MemPat"), (_MemAccessPat, "MemAccessPat"),
    (_OrderedPat, "OrderedPat"), (_OutputPat, "OutputPat"),
):
    _proto.__name__ = _proto.__qualname__ = _proto_name
    _proto.__module__ = "strider.pattern"
    setattr(_p, _proto_name, _proto)
    _export(_p, _proto_name)

# `CallPat.output`'s name for the shared output terminal.
_p.CallOutputPat = _p.OutputSlotPat  # type: ignore[attr-defined]
_export(_p, "CallOutputPat")


class _Readable(_t.Protocol):
    def read(self, addr: int, size: int, /) -> _t.Optional[bytes]: ...


_Readable.__module__ = "strider.reader"
reader._Readable = _Readable  # type: ignore[attr-defined]
reader.MemLike = _t.Union[reader.MemReader, reader.BufferReader, _Readable]  # type: ignore[attr-defined]
reader.RomLike = _t.Union[reader.ReadOnlyMemory, reader.BufferReader, _Readable]  # type: ignore[attr-defined]
del _Readable
_export(reader, "MemLike", "RomLike")
template.TemplateLike = _t.Union[  # type: ignore[attr-defined]
    template.Template, _p.Capture, int
]
_export(template, "TemplateLike")
cfg.DotStyle = _t.Literal[cfg.DOT_STYLES]  # type: ignore[attr-defined]
_export(cfg, "DotStyle")
lift.StrPath = _mod_api.StrPath              # type: ignore[attr-defined]
_export(lift, "StrPath")
opt.MainOptimizerPass = _t.Union[  # type: ignore[attr-defined]
    tuple(getattr(opt, _n) for _n in opt._MAIN_PASS_NAMES)
]
opt.PostOptimizerPass = _t.Union[  # type: ignore[attr-defined]
    tuple(getattr(opt, _n) for _n in opt._POST_PASS_NAMES)
]
opt.OptimizerPass = _t.Union[  # type: ignore[attr-defined]
    opt.MainOptimizerPass, opt.PostOptimizerPass
]
_export(opt, "OptimizerPass", "MainOptimizerPass", "PostOptimizerPass")

__all__ = ["StriderError", "ir", "lift", "cfg", "sleigh", "reader",
           "opt", "pattern", "template"]

# Drop import machinery from the public namespace.
del _importlib, _sys, _ext, _name, _sub, _names, _mod_api, _t, _p, _export
del _SUBMODULE_DOCS
del _S, _NodePat, _InputPat, _CtrlPat, _MemPat, _MemAccessPat, _OrderedPat
del _OutputPat, _proto, _proto_name
# Bound by the import system, and already gone on `importlib.reload`.
globals().pop("_strider", None)
globals().pop("_api", None)
