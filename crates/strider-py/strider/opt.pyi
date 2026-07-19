"""Type stubs for strider.opt: the optimizer pipeline and its passes."""

from __future__ import annotations

from typing import Any, List

class OptimizerPipeline:
    """An ordered list of optimizer passes, plus post-passes that run once
    after the main passes finish.

    Pass one to `Lifter.optimize(function, pipeline)`, or to
    `LifterOptions(pipeline=...)` to override the default for a single
    `analyze` call.
    """
    @classmethod
    def empty(cls) -> OptimizerPipeline:
        """A pipeline with no passes; add them with `add` / `add_post`."""
        ...
    @classmethod
    def default(cls) -> OptimizerPipeline:
        """The canonical pipeline strider uses when none is supplied."""
        ...
    @property
    def passes(self) -> List[str]:
        """The registered main (repeated) passes, by name, in order."""
        ...
    @property
    def post_passes(self) -> List[str]:
        """The registered post-passes (run once at the end), by name."""
        ...
    def add(self, pass_obj: Any) -> None:
        """Append a pass to the main list, which runs repeatedly until the
        graph stops changing."""
        ...
    def add_post(self, pass_obj: Any) -> None:
        """Append a post-pass, run once after the main passes finish."""
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
    """Removes phi nodes whose inputs all agree or that have a single
    reachable predecessor."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class RegionCollapse:
    """Removes control-flow regions with a single reachable predecessor."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class DeadBranchElimination:
    """Removes branches on a constant condition and strips the control
    edges that become dead."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class CfgDetach:
    """Detaches control-flow subgraphs that entry can no longer reach."""
    def __init__(self) -> None:
        """Build the pass."""
        ...

class LoadForward:
    """Forwards a store's value to a later load of the same location when
    no intervening write, call, or control merge can clobber it."""
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
    """Folds a load from a constant address into a constant, reading the
    bytes from the read-only memory image (`rom=`)."""
    def __init__(self) -> None:
        """Build the pass."""
        ...
