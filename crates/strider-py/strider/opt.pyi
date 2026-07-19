"""Type stubs for strider.opt."""

from __future__ import annotations

from typing import Any, List

class OptimizerPipeline:
    """An ordered list of optimizer passes plus a set of post-passes run
    once after the fixed-point loop converges."""
    @classmethod
    def empty(cls) -> OptimizerPipeline: ...
    @classmethod
    def default(cls) -> OptimizerPipeline: ...
    @property
    def passes(self) -> List[str]:
        """The registered fixed-point passes, by name, in order."""
        ...
    @property
    def post_passes(self) -> List[str]:
        """The registered post-passes (run once after convergence), by name."""
        ...
    def add(self, pass_obj: Any) -> None: ...
    def add_post(self, pass_obj: Any) -> None: ...

# Pure no-arg passes.
class ConstantFold:
    def __init__(self) -> None: ...

class KnownBits:
    def __init__(self) -> None: ...

class FlagCmpCanonicalize:
    def __init__(self) -> None: ...

class IfCondInversion:
    def __init__(self) -> None: ...

class PhiCollapse:
    def __init__(self) -> None: ...

class RegionCollapse:
    def __init__(self) -> None: ...

class DeadBranchElimination:
    def __init__(self) -> None: ...

class CfgDetach:
    def __init__(self) -> None: ...

# Formerly CC/arch-aware passes — now zero-arg (the calling convention is
# read from the function under analysis at run time).
class LoadForward:
    def __init__(self) -> None: ...

class StackOffsetDetect:
    def __init__(self) -> None: ...

class FunctionArgDetect:
    def __init__(self) -> None: ...

class CallStackArgCollect:
    def __init__(self) -> None: ...

class LoadReadOnly:
    def __init__(self) -> None: ...
