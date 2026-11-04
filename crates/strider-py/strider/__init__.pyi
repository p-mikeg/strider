"""Type stubs for the strider Python bindings.

The public API is the domain submodules `strider.ir`, `strider.lift`,
`strider.cfg`, `strider.sleigh`, `strider.reader`, `strider.opt`,
`strider.pattern` and `strider.template`, plus `StriderError`. Import classes
from their submodule (`from strider.ir import Function`).
"""

from __future__ import annotations

# Public, though outside `__all__`.
from . import explore as explore
from . import cfg as cfg
from . import ir as ir
from . import lift as lift
from . import opt as opt
from . import pattern as pattern
from . import reader as reader
from . import sleigh as sleigh
from . import template as template

__version__: str
__all__: list[str]

class StriderError(Exception):
    """Raised when an analysis fails, a pattern is malformed, or an argument
    has a bad value, a malformed ELF included. A wrong type raises
    `TypeError`, an int out of range `OverflowError` (an `entry` raises
    `StriderError`), a range-checked option `ValueError`, a file `load_elf`
    finds is not an ELF or targets an unsupported arch or ABI `ValueError`,
    and a missing path `FileNotFoundError`.

    `.backtrace` is the message followed by the Rust backtrace (empty when
    raised from Python); `STRIDER_BACKTRACE=1` appends it to the message."""

    backtrace: str
    ...
