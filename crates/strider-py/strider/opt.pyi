"""Type stubs for strider.opt."""

from __future__ import annotations

from typing import Any

# Pure no-arg passes.
class ConstantFold:
    def __init__(self) -> None: ...

class KnownBits:
    def __init__(self) -> None: ...

class RedundantPhis:
    def __init__(self) -> None: ...

class DeadBranchElim:
    def __init__(self) -> None: ...

class CallOtherElide:
    def __init__(self) -> None: ...

# CC/arch-aware passes.
class StackStoreDetect:
    def __init__(self, sleigh: Any, cc: Any) -> None: ...

class StackLoadForward:
    def __init__(self, sleigh: Any, cc: Any, arch: Any) -> None: ...

class FunctionArgDetect:
    def __init__(self, sleigh: Any, cc: Any) -> None: ...

class CallStackArgCollect:
    def __init__(self, sleigh: Any, cc: Any) -> None: ...

class LoadReadOnly:
    def __init__(self, rom: Any) -> None: ...
