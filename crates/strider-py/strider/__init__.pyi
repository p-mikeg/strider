"""Type stubs for the strider Python bindings.

The public API is the domain submodules — `strider.ir`, `strider.lift`,
`strider.cfg`, `strider.sleigh`, `strider.reader`, `strider.opt`,
`strider.pattern`, `strider.template` — plus the single top-level
`StriderError`.  Import classes from their home submodule
(`from strider.ir import Function`, `from strider.lift import Lifter`, …).
"""

from __future__ import annotations

# Publish the domain submodules under their public dotted names.
from . import cfg as cfg
from . import ir as ir
from . import lift as lift
from . import opt as opt
from . import pattern as pattern
from . import reader as reader
from . import sleigh as sleigh
from . import template as template

__version__: str

# The one cross-cutting top-level symbol.  Every Rust error lands here
# carrying an informative message; the hierarchy is flat (no typed
# subclasses).
class StriderError(Exception):
    """The single exception type raised by strider."""
    ...
