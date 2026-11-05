"""The optimizer pipeline and its passes."""

from __future__ import annotations

from typing import Union

__all__: list[str]

_MAIN_PASS_NAMES: tuple[str, ...]
_POST_PASS_NAMES: tuple[str, ...]

#: A pass `OptimizerPipeline.add` accepts: run repeatedly to a fixed point.
MainOptimizerPass = Union[
    "ConstantFold",
    "KnownBits",
    "FlagCmpCanonicalize",
    "IfCondInversion",
    "PhiCollapse",
    "RegionCollapse",
    "DeadBranchElimination",
    "LoadForward",
    "LoadReadOnly",
]

#: A pass that runs once, after the fixed-point loop. `add` raises for these.
PostOptimizerPass = Union[
    "FunctionArgDetect",
    "CallStackArgCollect",
]

#: Any built-in optimizer pass instance.
OptimizerPass = Union[MainOptimizerPass, PostOptimizerPass]

class OptimizerPipeline:
    """An ordered list of optimizer passes plus post-passes that run once at
    the end. Pass one to `Function.optimize(pipeline=...)` or
    `LifterOptions(pipeline=...)`. Applying a pipeline copies it, so one
    object serves any number of calls.
    """
    @classmethod
    def empty(cls) -> OptimizerPipeline:
        """A pipeline with no passes."""
        ...
    @classmethod
    def default(cls) -> OptimizerPipeline:
        """The pipeline `analyze` runs when none is supplied: every built-in
        pass. The indirect-branch classifier runs after any pipeline."""
        ...
    @property
    def passes(self) -> list[str]:
        """The main passes' names, in order."""
        ...
    @property
    def post_passes(self) -> list[str]:
        """The post-passes' names, in order."""
        ...
    def add(self, pass_obj: MainOptimizerPass) -> None:
        """Append a pass to the main list, run until the graph stops changing.
        Raises `StriderError` for a post-pass."""
        ...
    def add_post(self, pass_obj: OptimizerPass) -> None:
        """Append a post-pass, run once after the main passes. A
        `MainOptimizerPass` is accepted too, and runs once."""
        ...

class ConstantFold:
    """Evaluates constant expressions and applies algebraic identities (`x+0`
    to `x`, `x^x` to `0`, AND-mask merging)."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class KnownBits:
    """Tracks which bits are known to be zero or one and simplifies operations
    whose result is fully determined."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class FlagCmpCanonicalize:
    """Folds an architecture's condition-flag tree into a single direct
    comparison node."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class IfCondInversion:
    """Rewrites a branch on a negated condition (`Xor(C, IntConst(1)):I1`) into
    a branch on the plain condition with its two arms swapped."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class PhiCollapse:
    """Removes trivial phis: a `Phi` / `MemPhi`, or a strongly connected group
    of them, whose inputs other than themselves are one value is replaced by
    that value."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class RegionCollapse:
    """Collapses a `Region` with exactly one control input into that
    predecessor, replacing each phi over it with its lone value input."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class DeadBranchElimination:
    """Folds an `If` or `Switch` on a constant selector to its live successor,
    then strips the `Region` predecessors entry can no longer reach, with
    their `Phi` / `MemPhi` slots."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class LoadForward:
    """Forwards a store's value to a later load when the nearest may-aliasing
    memory definition is a store to the same address covering the whole
    load. A partial overlap, `InitialMemory`, a `MemPhi` whose arms
    disagree, or a call the assumptions cannot see through blocks it."""
    def __init__(self) -> None:
        """Build the pass."""
        ...


class FunctionArgDetect:
    """Records which values carry each incoming stack-passed argument. Register
    arguments are recorded at lift time."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class CallStackArgCollect:
    """Wires the stack slots written before a call in as that call's positional
    arguments."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class LoadReadOnly:
    """Folds a load from a constant address into a constant read from the
    read-only memory image (`rom=`); a no-op without one.

    The fold ignores the load's memory chain, so the rom must map only
    runtime-immutable memory: code and `.rodata`, never `.data` / `.got`."""
    def __init__(self) -> None:
        """Build the pass."""
        ...
