"""Type stubs for strider.opt."""

from __future__ import annotations

from typing import Any

# Pure no-arg passes.
class ConstantFold:
    def __init__(self) -> None: ...

class KnownBits:
    def __init__(self) -> None: ...

class FlagCmpCanonicalize:
    def __init__(self) -> None: ...

class IfCondInversion:
    def __init__(self) -> None: ...

class RedundantPhis:
    def __init__(self) -> None: ...

class DeadBranchElimination:
    def __init__(self) -> None: ...

# CC/arch-aware passes.
class LoadForward:
    def __init__(self, sleigh: Any, cc: Any, arch: Any) -> None: ...

class StackOffsetDetect:
    def __init__(self, sleigh: Any, cc: Any) -> None: ...

class FunctionArgDetect:
    def __init__(self, sleigh: Any, cc: Any) -> None: ...

class CallStackArgCollect:
    def __init__(self, sleigh: Any, cc: Any) -> None: ...

class LoadReadOnly:
    def __init__(self) -> None: ...
