"""Type stubs for strider.opt: the optimizer pipeline and its passes."""

from __future__ import annotations

from typing import Union

__all__: list[str]

#: A pass the fixed-point loop can run repeatedly, the only kind
#: `OptimizerPipeline.add` accepts.
MainOptimizerPass = Union[
    "ConstantFold",
    "KnownBits",
    "FlagCmpCanonicalize",
    "IfCondInversion",
    "PhiCollapse",
    "RegionCollapse",
    "DeadBranchElimination",
    "CfgDetach",
    "LoadForward",
    "LoadReadOnly",
]

#: A pass that runs once, after the fixed-point loop converges. `add` raises
#: for these; `add_post` takes them and any `MainOptimizerPass` too.
PostOptimizerPass = Union[
    "StackOffsetDetect",
    "FunctionArgDetect",
    "CallStackArgCollect",
]

#: Any built-in optimizer pass instance.
OptimizerPass = Union[MainOptimizerPass, PostOptimizerPass]

class OptimizerPipeline:
    """An ordered list of optimizer passes, plus post-passes that run once
    after the main passes finish.

    Pass one to `Lifter.optimize(function, pipeline)`, or to
    `LifterOptions(pipeline=...)` to override the default. Applying a pipeline
    copies its passes, so one object serves any number of calls.
    """
    @classmethod
    def empty(cls) -> OptimizerPipeline:
        """A pipeline with no passes; add them with `add` / `add_post`."""
        ...
    @classmethod
    def default(cls) -> OptimizerPipeline:
        """Every built-in pass exposed here, in the order `analyze` runs them
        when no pipeline is supplied. `analyze` also appends its own
        indirect-branch classifier, which is not one of these."""
        ...
    @property
    def passes(self) -> list[str]:
        """The registered main (repeated) passes, by name, in order."""
        ...
    @property
    def post_passes(self) -> list[str]:
        """The registered post-passes (run once at the end), by name."""
        ...
    def add(self, pass_obj: MainOptimizerPass) -> None:
        """Append a pass to the main list, which runs repeatedly until the
        graph stops changing. Raises `StriderError` for a post-pass."""
        ...
    def add_post(self, pass_obj: OptimizerPass) -> None:
        """Append a post-pass, run once after the main passes finish. A
        `MainOptimizerPass` is accepted too, and runs once."""
        ...

class ConstantFold:
    """Evaluates constant expressions and applies algebraic identities
    (`x+0` to `x`, `x^x` to `0`, AND-mask merging)."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class KnownBits:
    """Tracks which bits are known to be zero or one and simplifies
    operations whose result is fully determined."""
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
    """Rewrites a branch on a negated condition into a branch on the plain
    condition with its two arms swapped."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class PhiCollapse:
    """Braun trivial-phi elimination: redirects a `Phi` / `MemPhi` whose value
    inputs resolve to exactly one distinct value, self-references discarded, to
    that value. A genuine two-way merge is left alone."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class RegionCollapse:
    """Collapses a `Region` that already has exactly one control input,
    rewiring its control consumers to that predecessor and each phi over it to
    that phi's lone value input. A multi-predecessor Region is left alone."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class DeadBranchElimination:
    """Folds a branch with a constant selector (an `If` on a constant `I1`,
    a `Switch` on a constant dispatch address), wiring the live successor past
    it and killing the branch node."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class CfgDetach:
    """Removes a `Region` predecessor slot whose control producer entry can no
    longer reach, along with the matching `Phi` / `MemPhi` value slots."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class LoadForward:
    """Forwards a store's value to a later load when the nearest may-aliasing
    memory definition is an exact-match store: same address class, base and
    offset, covering the load's whole range. A partial overlap, a `MemPhi`
    (control merge), `InitialMemory`, or a call the assumptions cannot see
    through all block it."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class StackOffsetDetect:
    """Annotates stack-pointer-relative loads and stores with their frame
    offsets."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class FunctionArgDetect:
    """Records which values carry each incoming stack-passed argument.
    Register arguments are recorded at lift time."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class CallStackArgCollect:
    """Wires the stack slots written before a call in as that call's
    positional arguments."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class LoadReadOnly:
    """Folds a load from a constant address into a constant, reading the bytes
    from the read-only memory image (`rom=`). A no-op when no rom is supplied.

    The fold never consults the load's memory chain, so the rom must map only
    runtime-immutable memory: code and `.rodata`, never `.data` / `.got`."""
    def __init__(self) -> None:
        """Build the pass."""
        ...
