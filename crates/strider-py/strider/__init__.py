"""strider — Python bindings for the Strider binary analysis pipeline.

The Rust extension is loaded as `strider.strider` (the cdylib) and
re-exported into this package's top-level namespace below.
"""

from .strider import *  # noqa: F401,F403
from . import strider as _ext  # noqa: F401

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
#   * `strider.load(path)` → `Program` (the loaded ELF)
#   * `strider.Program` — the user-facing loaded-binary handle
#   * `strider.Analysis` — wrapper around a RunResult
#
# Does NOT shadow the cdylib's `strider.Strider` class — that remains
# the low-level lift-driver type (constructed as `Strider(arch, sleigh,
# cc)` and consumed by the orchestrator).  The high-level handle is now
# the distinctly-named `Program`, so the two no longer collide.
from . import _api as _api  # noqa: E402,F401
from ._api import Analysis, Program, load  # noqa: E402,F401
