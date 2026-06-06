"""strider — Python bindings for the Strider binary analysis pipeline.

The Rust extension is loaded as `strider.strider` (the cdylib) and
re-exported into this package's top-level namespace below.
"""

# Bind the cdylib submodule FIRST, before the wildcard import below —
# the cdylib now exports a `strider()` function, so `from .strider
# import *` binds the name `strider` (the function) into this package's
# namespace; a subsequent `from . import strider as _ext` would then
# resolve that function instead of the submodule.  Import the submodule
# by its full dotted path so `_ext` is unambiguously the extension
# module regardless of what the wildcard later binds.
import importlib as _importlib

_ext = _importlib.import_module("strider.strider")

from .strider import *  # noqa: F401,F403,E402

# Hoist the submodules registered by the Rust side into Python so
# `from strider import errors` works whether the extension was loaded
# directly or from inside a package.
import sys as _sys

if hasattr(_ext, "errors"):
    _sys.modules["strider.errors"] = _ext.errors
if hasattr(_ext, "opt"):
    _sys.modules["strider.opt"] = _ext.opt
if hasattr(_ext, "pattern"):
    _sys.modules["strider.pattern"] = _ext.pattern

# __version__ comes from the cdylib.
__version__ = _ext.__version__

# High-level Python facade.  Adds:
#   * `strider.load_elf(path)` → `ElfStrider` (the loaded ELF + run handle)
#   * `strider.ElfStrider` — the user-facing loaded-binary handle
#   * `strider.strider(arch, cc, mem, rom=None)` → `Strider` (the native
#     standalone run handle, re-exported here)
#   * `strider.Analysis` — wrapper around a lifted `Function`
#
# The cdylib's `strider.Strider` is the high-level run handle (full
# lift+optimise+resolve); the renamed `strider.Lifter` is the low-level
# lift-driver ("lift one CFG, no resolution").  Both come from `_ext`
# via the wildcard import above.
from . import _api as _api  # noqa: E402,F401
from ._api import (  # noqa: E402,F401
    Analysis,
    ElfStrider,
    load_elf,
    strider,
)
