"""Type stubs for the strider Python bindings.

The public API is the domain submodules `strider.ir`, `strider.lift`,
`strider.cfg`, `strider.sleigh`, `strider.reader`, `strider.opt`,
`strider.pattern` and `strider.template`, plus the single top-level
`StriderError`. Import classes from their home submodule
(`from strider.ir import Function`, `from strider.lift import Lifter`).
"""

from __future__ import annotations

# `explore` is bound on the package as a side effect of `Lifter.visualize`
# importing it. Declared here so it is part of the surface by intent rather
# than by import order; `explore.shutdown(port)` is the supported way to
# stop an explorer started on another thread.
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

class StriderError(Exception):
    """The single exception type strider raises. The hierarchy is
    deliberately flat: there are no typed subclasses, only an informative
    message."""
    ...
