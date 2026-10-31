"""Type stubs for the strider Python bindings.

The public API is the domain submodules `strider.ir`, `strider.lift`,
`strider.cfg`, `strider.sleigh`, `strider.reader`, `strider.opt`,
`strider.pattern` and `strider.template`, plus the single top-level
`StriderError`. Import classes from their home submodule
(`from strider.ir import Function`, `from strider.lift import Lifter`).
"""

from __future__ import annotations

# Outside `__all__` but part of the surface: `explore.shutdown(port)` is the
# supported way to stop an explorer `explore.visualize` started on another
# thread.
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
    """Raised when an analysis fails, when a pattern is built into an
    invalid shape, and for a bad argument, unless the conversion itself
    rejects it (`TypeError` for the wrong type, `OverflowError` for an int
    out of range, though an `entry` out of range raises this), it is a
    range-checked option (`ValueError`, such as
    `CfgOptions(function_max_size=0)`, an unknown `known_targets` string, an
    empty `data_ranges` range or a `BufferReader` region past the address
    space), an ELF `load_elf` cannot lift (`ValueError`), or a missing file
    (`FileNotFoundError`). The message
    carries the error and its causes; `.backtrace` carries that same message
    followed by the Rust backtrace, and `STRIDER_BACKTRACE=1` folds the
    backtrace into the message too."""

    backtrace: str
    ...
