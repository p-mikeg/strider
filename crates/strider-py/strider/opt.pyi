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
        ...

class KnownBits:
    """Tracks which bits are known to be zero or one and simplifies operations
    whose result is fully determined."""
    def __init__(self) -> None:
        ...

class FlagCmpCanonicalize:
    """Folds an architecture's condition-flag tree into a single direct
    comparison node."""
    def __init__(self) -> None:
        ...

class IfCondInversion:
    """Rewrites a branch on a negated condition (`Xor(C, IntConst(1)):I1`) into
    a branch on the plain condition with its two arms swapped."""
    def __init__(self) -> None:
        ...

class PhiCollapse:
    """Removes trivial phis: a `Phi` / `MemPhi`, or a strongly connected group
    of them, whose inputs other than themselves are one value is replaced by
    that value. An `and` of a phi with a constant mask becomes the value
    every arm agrees on under that mask. Below the edge where an `If` finds
    `phi + c` equal to a `y` computed from entry values and constants, the
    phi reads as `y - c`."""
    def __init__(self) -> None:
        ...

class RegionCollapse:
    """Collapses a `Region` with exactly one control input into that
    predecessor, replacing each phi over it with its lone value input."""
    def __init__(self) -> None:
        ...

class DeadBranchElimination:
    """Folds an `If` or `Switch` whose constant selector names an arm to that
    arm, when the arm still reaches a terminator, then strips the `Region`
    predecessors entry can no longer reach, with their `Phi` / `MemPhi`
    slots."""
    def __init__(self) -> None:
        ...

class LoadForward:
    """Forwards a store's value to a later load when the nearest may-aliasing
    memory definition is a store to the same address covering the whole
    load. A partial overlap, `InitialMemory`, a `MemPhi` whose arms
    disagree, or a call the assumptions cannot see through blocks it."""
    def __init__(self) -> None:
        ...

class FunctionArgDetect:
    """Numbers the incoming stack arguments the entry-SP loads read, after
    the register arguments: one ordinal per run of slots one load spans, up to
    the first slot no clean load reads (a slot a store or call may have
    overwritten is not clean). A `long long` read as two 4-byte loads takes
    two. Register arguments are recorded at lift time."""
    def __init__(self) -> None:
        ...

class CallStackArgCollect:
    """Wires the stores into a call's outgoing argument slots in as its
    positional arguments, up to the first slot no whole store reaches or a
    prologue save; incidental stores just above can come along."""
    def __init__(self) -> None:
        ...

class LoadReadOnly:
    """Folds a load from a constant address into a constant read from the
    read-only memory image (`rom=`); a no-op without one.

    The fold ignores the load's memory chain, so the rom must map only memory
    the program never writes: code, `.rodata`, and a relocated GOT only under
    `relocated_got_is_rom`."""
    def __init__(self) -> None:
        ...
