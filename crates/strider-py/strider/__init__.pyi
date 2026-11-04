"""Strider: lift a native function to a sea-of-nodes IR and query it.

The API is the submodules `strider.ir`, `strider.lift`, `strider.cfg`,
`strider.sleigh`, `strider.reader`, `strider.opt`, `strider.pattern` and
`strider.template`, plus `StriderError`. Import classes from their submodule
(`from strider.ir import Function`).
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
    (a malformed ELF included) has a bad value. Otherwise a wrong type raises
    `TypeError` (a `bool` address or index included), an out-of-range
    address, node id or operand index `StriderError` and another
    out-of-range int `OverflowError`, a range-checked option `ValueError`, a
    missing path `FileNotFoundError`, and `load_elf` on a non-ELF file or an
    unsupported arch or ABI `ValueError`.

    `.backtrace` is the message plus the Rust backtrace (empty when raised
    from Python); `STRIDER_BACKTRACE=1` appends it to the message."""

    backtrace: str
    ...
