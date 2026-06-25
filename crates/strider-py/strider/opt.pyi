"""Type stubs for strider.opt."""

from __future__ import annotations

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
