"""Python bindings for the Strider binary analysis pipeline.

The public API is the domain submodules `strider.ir`, `strider.lift`,
`strider.cfg`, `strider.sleigh`, `strider.reader`, `strider.opt`,
`strider.pattern` and `strider.template`, plus the top-level
`StriderError`.
"""

import importlib as _importlib
import sys as _sys

_ext = _importlib.import_module("strider._strider")

for _name in ("ir", "lift", "cfg", "sleigh", "reader", "opt", "pattern", "template"):
    _sub = getattr(_ext, _name)
    _sys.modules[f"strider.{_name}"] = _sub
    globals()[_name] = _sub

# Give `strider.pattern.constraints` its own `sys.modules` key so
# `import strider.pattern.constraints` resolves.
_sys.modules["strider.pattern.constraints"] = _ext.pattern.constraints

StriderError = _ext.StriderError
__version__ = _ext.__version__

# Bind the pure-Python facade members into their home submodule.
from . import _api as _mod_api  # noqa: E402

lift.ElfLifter = _mod_api.ElfLifter          # type: ignore[attr-defined]
lift.load_elf = _mod_api.load_elf            # type: ignore[attr-defined]

__all__ = ["StriderError", "ir", "lift", "cfg", "sleigh", "reader",
           "opt", "pattern", "template"]

# Drop import machinery from the public namespace, including the
# `strider._strider` / `strider._api` attributes the import system binds.
del _importlib, _sys, _ext, _name, _sub, _mod_api, _strider, _api
