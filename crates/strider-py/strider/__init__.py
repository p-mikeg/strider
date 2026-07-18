"""strider — Python bindings for the Strider binary analysis pipeline.

The Rust extension is loaded as `strider._strider` (the cdylib) and
re-exported into this package's top-level namespace below.
"""

# Bind the cdylib submodule FIRST, before the wildcard import below, by
# its full dotted path so `_ext` is unambiguously the extension module
# regardless of what names the wildcard later binds into this package's
# namespace.
import importlib as _importlib

_ext = _importlib.import_module("strider._strider")

from ._strider import *  # noqa: F401,F403,E402

# Hoist the submodules registered by the Rust side into Python so
# `from strider import opt` works whether the extension was loaded
# directly or from inside a package.
import sys as _sys

if hasattr(_ext, "opt"):
    _sys.modules["strider.opt"] = _ext.opt
if hasattr(_ext, "pattern"):
    _sys.modules["strider.pattern"] = _ext.pattern
if hasattr(_ext, "template"):
    _sys.modules["strider.template"] = _ext.template

# __version__ comes from the cdylib.
__version__ = _ext.__version__

# High-level Python facade.  Adds:
#   * `strider.load_elf(path, from_segments=True)` → `ElfLifter` — the
#     `from_segments` flag picks the ELF region-collection strategy
#     explicitly (PT_LOAD segments, the default, vs section headers)
#   * `strider.ElfLifter` — an `ElfLifter` IS a `Lifter`
#     (`isinstance(x, strider.Lifter)` is true), plus the ELF symbol
#     backend and a name-aware `analyze(target)`
#
# The cdylib's `strider.lifter(arch, mem, rom=None) -> Lifter` is the
# single lift+optimise+resolve handle (`build_cfg` + `analyze`); it comes
# from `_ext` via the wildcard import above.  Sleigh-needing renders
# (`to_dot`/`to_html`) and the entry-relative p-code sweep
# (`pcode_at(entry, addr)`) are `Lifter` methods; the CFG-lookup p-code
# audit trail (`pcode_at(addr)` / `fingerprint_pcode(node)`) lives on
# `Cfg` (returned as element 0 of `analyze`); pattern queries and the
# addr-only `fingerprint` live on `Function`/`Node` directly — there is
# no separate `Analysis` wrapper.
from . import _api as _api  # noqa: E402,F401
from ._api import (  # noqa: E402,F401
    ElfLifter,
    load_elf,
)
