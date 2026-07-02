"""High-level Python facade for the strider pipeline.

The lower-level building blocks (`BufferReader`, `Sleigh`, `Lifter`,
`Function`, `Matcher`, `OptimizerPipeline`, etc.) remain on the
top-level `strider` namespace for users who need granular control.
This module adds the three-line convenience workflow:

```python
import strider
lift = strider.load_elf("path/to/file.elf")        # → ElfLifter
function, unresolved = lift.analyze("function_name")
matches = function.find_all(strider.pattern.call())  # → list[Match]
```

`strider.load_elf(path)` returns an `ElfLifter` — an `ElfLifter` IS a
`Lifter` (`isinstance(lift, strider.Lifter)` is true): it holds the ELF
symbol backend AND the persistent lift+optimise+resolve state (the same
Rust handle `strider.lifter(...)` builds), wired with the ELF's memory
as both the code reader and the read-only-memory image for
`LoadReadOnly` constant folding.  It exposes symbols, sizes, the entry
point, raw reads, and a name-aware `analyze(target)` that resolves a
`str` symbol name (or accepts an address) and returns the same
`(Function, unresolved_addrs)` tuple as the base `Lifter.analyze`.

`strider.load_elf_from_segments(path)` and
`strider.load_elf_from_sections(path)` pick the ELF region-collection
strategy explicitly (PT_LOAD program headers vs section headers);
`strider.load_elf(path)` is a thin convenience that delegates to
`load_elf_from_segments`.

For non-ELF / firmware / custom-source cases, build a `Lifter`
directly with `strider.lifter(arch, mem, rom=None)` (the native
Rust handle) and call its `analyze(addr, cc, ...)`.  `Analysis` is an
optional convenience wrapper around a bare `(Function, ...)` result —
`find` / `find_one` / `find_joined` / `fingerprint` /
`fingerprint_pcode` — for callers who want that on top of a raw
`Lifter.analyze()` result (an `ElfLifter.analyze()` result can be
wrapped the same way; `analyze()` itself never constructs one
automatically).

Arch detection is done by reading the first 20 bytes of the ELF
header (magic + EI_CLASS + EI_DATA + e_machine).  No pyelftools
dependency at runtime.
"""

from __future__ import annotations

import os
import struct
from typing import Iterator, Optional, Union

# Resolve the cdylib submodule by its full dotted path so `_ext` is
# unambiguously the extension module regardless of what names the
# wildcard import in `__init__.py` binds into the package namespace.
import importlib as _importlib

_ext = _importlib.import_module("strider.strider")
from .strider import (  # noqa: E402
    CallingConvention,
    Function,
    Lifter,
    SleighArch,
)


# ── ELF header parsing ────────────────────────────────────────────────────

# e_machine codes per the System V ABI gABI / each arch supplement.
_EM_386 = 3
_EM_MIPS = 8
_EM_PPC = 20
_EM_PPC64 = 21
_EM_ARM = 40
_EM_X86_64 = 62
_EM_AARCH64 = 183


class _ElfHeader:
    """Minimal ELF header reader — just enough to pick the right
    `SleighArch` + `CallingConvention` preset.

    Reads the first 20 bytes of the file (the e_ident[16] +
    e_type[u16] + e_machine[u16] fields) and exposes:
    * `is_64bit` (EI_CLASS == 2)
    * `is_little_endian` (EI_DATA == 1)
    * `e_machine`
    """

    __slots__ = ("is_64bit", "is_little_endian", "e_machine")

    def __init__(self, path: str) -> None:
        with open(path, "rb") as f:
            header = f.read(20)
        if len(header) < 20 or header[:4] != b"\x7fELF":
            raise ValueError(f"{path!r}: not an ELF file (bad magic)")
        ei_class = header[4]
        ei_data = header[5]
        if ei_class not in (1, 2):
            raise ValueError(f"{path!r}: invalid EI_CLASS={ei_class}")
        if ei_data not in (1, 2):
            raise ValueError(f"{path!r}: invalid EI_DATA={ei_data}")
        self.is_64bit = ei_class == 2
        self.is_little_endian = ei_data == 1
        # e_type is at bytes [16:18]; e_machine is at [18:20].
        endian_fmt = "<H" if self.is_little_endian else ">H"
        (self.e_machine,) = struct.unpack(endian_fmt, header[18:20])


def _arch_and_cc_for_elf(
    header: _ElfHeader, entry: Optional[int] = None
) -> tuple[SleighArch, CallingConvention]:
    """Pick the SleighArch + userland CallingConvention preset
    matching this ELF.  Raises `ValueError` for unsupported
    e_machine codes.

    For ARM the choice between `arm` and `arm_thumb` is driven by
    the low bit of the entry-point address (Thumb interworking
    convention).  Callers that want a different default can pass an
    explicit `arch=` / `cc=` to `strider.load_elf(...)`.
    """
    em = header.e_machine
    le = header.is_little_endian
    is64 = header.is_64bit

    if em == _EM_386:
        return SleighArch.x86(), CallingConvention.x86_cdecl()
    if em == _EM_X86_64:
        return SleighArch.x86_64(), CallingConvention.x86_64_systemv()
    if em == _EM_ARM:
        # Thumb interworking: bit 0 of the entry-point address signals
        # Thumb mode.  Strip the bit before passing the address through
        # to Sleigh (callers using `analyze(name)` get this for free
        # via the symbol table).
        if entry is not None and (entry & 1):
            arch = SleighArch.arm_thumb()
        elif le:
            arch = SleighArch.arm()
        else:
            arch = SleighArch.arm_be()
        return arch, CallingConvention.arm_aapcs()
    if em == _EM_AARCH64:
        arch = SleighArch.aarch64() if le else SleighArch.aarch64be()
        return arch, CallingConvention.aarch64_aapcs64()
    if em == _EM_MIPS:
        if is64:
            arch = SleighArch.mipsle64() if le else SleighArch.mipsbe64()
            return arch, CallingConvention.mips_n64()
        arch = SleighArch.mipsle32() if le else SleighArch.mipsbe32()
        return arch, CallingConvention.mips_o32()
    if em == _EM_PPC:
        arch = SleighArch.ppc32le() if le else SleighArch.ppc32be()
        return arch, CallingConvention.powerpc_sysv32()
    if em == _EM_PPC64:
        # ELFv2 is the modern Linux PPC64 ABI (used for both BE and LE
        # since glibc ~2.17); ELFv1 is the legacy BE-only convention.
        # Prefer v2 — users on a v1-only distro can override via the
        # explicit `arch=` / `cc=` arguments.
        arch = SleighArch.ppc64le() if le else SleighArch.ppc64be()
        return arch, CallingConvention.powerpc64_elf_v2()
    raise ValueError(
        f"unsupported ELF e_machine={em}; pass an explicit SleighArch + "
        f"CallingConvention via strider.load_elf(path, arch=..., cc=...) instead"
    )


# ── ARM Thumb interworking arch selection ──────────────────────────────────


def _effective_arch_and_addr(
    arch: SleighArch, raw_addr: int
) -> tuple[SleighArch, int]:
    """Resolve the arch + address to actually lift, honouring the ARM
    Thumb interworking convention.

    A Thumb function pointer / entry has its low bit set; the real
    instruction address is `raw_addr & ~1` and it must be decoded with
    the Thumb Sleigh spec.  When `arch` is ARM-family (`arm`, `arm_be`,
    `arm_thumb`) and the low bit is set, return `(arm_thumb, raw_addr & ~1)`.
    Otherwise the arch is returned unchanged and the address verbatim
    (x86 et al. never interwork).

    Pure — no Sleigh / extension calls — so it is unit-testable without
    lifting.
    """
    if raw_addr & 1 and arch.name() in ("arm", "arm_be", "arm_thumb"):
        # `arm_thumb` is little-endian only — there is no big-endian
        # Thumb preset.  For a BE-ARM entry with the interworking bit
        # set we keep `arm_be` (best effort) but still strip the bit.
        if arch.name() == "arm_be":
            return arch, raw_addr & ~1
        return SleighArch.arm_thumb(), raw_addr & ~1
    return arch, raw_addr


# ── ElfLifter facade ─────────────────────────────────────────────────────


def _load_elf_with(
    loader,
    path: str,
    *,
    apply_relocations: bool,
    arch: Optional[SleighArch],
    cc: Optional[CallingConvention],
) -> "ElfLifter":
    """Shared body of `load_elf_from_segments` / `load_elf_from_sections`:
    validates `path`, auto-detects `arch`/`cc` from the ELF header when
    either is omitted, calls the Rust `loader(path, apply_relocations)`
    (one of `_ext.load_elf_from_segments` / `_ext.load_elf_from_sections`)
    to build the `_LoadedElf` backend, and wraps it in an `ElfLifter`.
    """
    if not os.path.exists(path):
        raise FileNotFoundError(path)
    header = _ElfHeader(path)
    # Auto-detect the arch/cc the header implies, but let any explicit
    # argument win.  Only fall into the (possibly raising) detector when
    # at least one of arch/cc is missing.
    if arch is None or cc is None:
        det_arch, det_cc = _arch_and_cc_for_elf(header)
        arch = arch if arch is not None else det_arch
        cc = cc if cc is not None else det_cc
    elf = loader(path, apply_relocations)
    return ElfLifter(elf, arch, cc, elf.reader(), rom=elf.ro_reader())


def load_elf_from_segments(
    path: str,
    *,
    apply_relocations: bool = True,
    arch: Optional[SleighArch] = None,
    cc: Optional[CallingConvention] = None,
) -> "ElfLifter":
    """Load an ELF binary and return an `ElfLifter` — an `ElfLifter` IS a
    `Lifter` (`isinstance(lift, strider.Lifter)` is true), with the arch
    and userland calling convention auto-picked from the ELF header.

    Regions are collected by walking **PT_LOAD program headers** (the
    runtime memory layout) for ET_EXEC / ET_DYN binaries, falling back to
    the section-walker (first-wins VMA dedup) for ET_REL objects, which
    carry no program headers at all.  This is the strategy strider has
    always used; see `load_elf_from_sections` to force section-header
    walking even for a linked binary.

    For non-userland workflows (Linux kernel, syscall stubs, embedded
    firmware with a custom CC) pass an explicit `arch=` / `cc=`, e.g.:

    ```python
    lift = strider.load_elf_from_segments(
        "vmlinux-i386",
        arch=strider.SleighArch.x86(),
        cc=strider.CallingConvention.x86_linux_kernel(),
    )
    ```

    `apply_relocations` (default `True`) widens the loaded section set
    (`.data.rel.ro`, `.got`, …) and patches every understood relocation
    in-place — needed for ET_DYN binaries (kernels, PIE userland) whose
    `.text` or function-pointer tables ship with unresolved relocations.

    Raises:
      * `FileNotFoundError` — path does not exist.
      * `ValueError` — file is not an ELF or the e_machine is not one
        of the supported architectures (only when `arch`/`cc` aren't
        both supplied).
    """
    return _load_elf_with(
        _ext.load_elf_from_segments,
        path,
        apply_relocations=apply_relocations,
        arch=arch,
        cc=cc,
    )


def load_elf_from_sections(
    path: str,
    *,
    apply_relocations: bool = True,
    arch: Optional[SleighArch] = None,
    cc: Optional[CallingConvention] = None,
) -> "ElfLifter":
    """Like `load_elf_from_segments`, but FORCES the section-header-walk
    region-collection strategy (first-wins VMA dedup) — even for a linked
    ET_EXEC / ET_DYN binary that does carry PT_LOAD segments.  Use this
    when you want section-granular regions (`.text` / `.rodata` / `.plt`
    as separate mappings) instead of the segment loader's coalesced
    PT_LOAD ranges.

    Same arguments, auto-detection, and error contract as
    `load_elf_from_segments`.
    """
    return _load_elf_with(
        _ext.load_elf_from_sections,
        path,
        apply_relocations=apply_relocations,
        arch=arch,
        cc=cc,
    )


def load_elf(
    path: str,
    *,
    apply_relocations: bool = True,
    arch: Optional[SleighArch] = None,
    cc: Optional[CallingConvention] = None,
) -> "ElfLifter":
    """Convenience: delegates to `load_elf_from_segments` — the
    region-collection strategy strider has always used."""
    return load_elf_from_segments(
        path, apply_relocations=apply_relocations, arch=arch, cc=cc
    )


class ElfLifter(Lifter):
    """The loaded ELF binary as a `Lifter`: `ElfLifter` IS a `Lifter`
    (`isinstance(lift, strider.Lifter)` is true) — it carries the same
    persistent lift+optimise+resolve state as a plain `strider.lifter(...)`
    handle, wired with the ELF's memory — the writable-inclusive reader
    as the code reader, and the runtime-immutable reader (code +
    read-only only) as the rom for `LoadReadOnly` constant folding — PLUS
    the ELF symbol backend (`symbol` / `symbols` / `symbol_size` /
    `entry_point` / `functions`) and a name-aware `analyze(target)`
    override that resolves a `str` symbol to an address before delegating
    to the base `Lifter.analyze`.

    Constructed via `strider.load_elf(path)` / `load_elf_from_segments`
    / `load_elf_from_sections` — for the auto-detected common case, or
    with explicit `arch=` / `cc=` for kernel / syscall / custom-ABI
    workflows.  Never construct `ElfLifter(...)` directly.  Analyse many
    functions by calling `analyze` repeatedly:

    ```python
    lift = strider.load_elf("vmlinux-i386", arch=strider.SleighArch.x86(),
                            cc=strider.CallingConvention.x86_linux_kernel())
    for fn in lift.functions():
        function, unresolved = lift.analyze(fn)
    ```
    """

    __slots__ = ("_elf", "_arch", "_cc")

    def __new__(
        cls,
        elf: object,  # strider._LoadedElf (internal, returned by the loader)
        arch: SleighArch,
        cc: CallingConvention,
        mem: object,
        rom: Optional[object] = None,
    ) -> "ElfLifter":
        # Build the base `Lifter` (the Rust `PyLifter` payload) via the
        # standard PyO3 "extra Python state on a Rust base" recipe:
        # `Lifter.__new__` only wants `(arch, mem, rom=...)`.
        return super().__new__(cls, arch, mem, rom=rom)

    def __init__(
        self,
        elf: object,
        arch: SleighArch,
        cc: CallingConvention,
        mem: object,
        rom: Optional[object] = None,
    ) -> None:
        del mem, rom  # consumed by __new__ / the base Lifter construction
        self._elf = elf
        self._arch = arch
        # `cc` is NOT stored on the base `Lifter` (it takes no default —
        # every `analyze` call supplies its own); `ElfLifter` keeps the
        # ELF-derived (or explicitly-passed) default here and threads it
        # into `analyze` below when the caller doesn't override it.
        self._cc = cc

    # ── Properties for introspection / advanced use ─────────────────

    @property
    def arch(self) -> SleighArch:
        """The target architecture (`SleighArch`) this binary uses."""
        return self._arch

    @property
    def cc(self) -> CallingConvention:
        """The default calling convention (`CallingConvention`) applied
        when analysing functions and `cc` isn't passed explicitly to
        `analyze`."""
        return self._cc

    def __repr__(self) -> str:
        """`repr(elf_lifter)` — includes the arch + cc names."""
        return f"ElfLifter(arch={self._arch.name()}, cc={self._cc.name()})"

    # ── Symbol lookup / raw reads (delegate to the loaded ELF) ───────

    def functions(self) -> Iterator[str]:
        """Yield every function/data symbol name from every ELF loaded
        into this handle.  Symbols with empty names or zero addresses
        (synthetic linker entries) are skipped.

        The first-loaded ELF wins on name collisions.
        """
        # symbols() returns dict[str, int]; the order isn't guaranteed
        # stable across Python versions for dicts created from a
        # HashMap, but the names themselves are correct.  Sort for
        # determinism.
        for name in sorted(self._elf.symbols().keys()):
            yield name

    def symbol(self, name: str) -> int:
        """Resolve a symbol name to its address.  Raises `StriderError`
        when the name isn't defined in any loaded ELF."""
        return self._elf.symbol(name)

    def symbol_size(self, name: str) -> Optional[int]:
        """The ELF-recorded size in bytes of `name` (`st_size`), or
        `None` when the symbol exists but its size is recorded as 0.
        Raises `StriderError` when the symbol isn't defined."""
        return self._elf.symbol_size(name)

    def symbols(self) -> dict:
        """All function/data symbols as a `dict[str, int]` (name →
        address).  The first-loaded ELF wins on name collisions."""
        return self._elf.symbols()

    def entry_point(self) -> int:
        """The ELF entry-point address (`e_entry`) of the first loaded
        ELF."""
        return self._elf.entry_point()

    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Read up to `size` raw bytes at `addr` from the loaded
        regions, or `None` when `addr` is unmapped."""
        return self._elf.read(addr, size)

    def reader(self) -> object:
        """The raw multi-region `BufferReader` assembled from the ELF's
        loaded sections — the low-level code reader you can hand to
        `strider.lifter` or `strider.Sleigh` when dropping below the
        high-level `analyze` facade."""
        return self._elf.reader()

    def add_elf(self, path: str, *, apply_relocations: bool = False) -> None:
        """Merge another ELF (e.g. a shared library) into this handle:
        extends the loaded regions and the symbol set.  The
        earlier-loaded ELF wins on symbol-name collisions.

        The base `Lifter`'s Sleigh was built from a point-in-time
        snapshot of the ELF's regions, so it is rebuilt here from the
        merged regions — subsequent `analyze` calls (and `read`/`symbol`)
        see the newly-added ELF."""
        self._elf.add_elf(path, apply_relocations)
        self._rebuild(self._arch, self._elf.reader(), rom=self._elf.ro_reader())

    # ── P-code ───────────────────────────────────────────────────────

    def pcode(self, addr: int, count: int = 1) -> list[tuple[int, str]]:
        """Lift the p-code of `count` machine instructions starting at
        `addr`, returning a list of `(insn_addr, text)` tuples in
        address order.

        `text` is the lifted p-code for each machine instruction (the
        instruction's p-code ops joined with `"; "`, empty for ops like
        `endbr64` that lift to no p-code).  rsleigh is a p-code lifter —
        this is the lifted semantics, NOT native assembly mnemonics.

        ARM Thumb interworking is honoured: a Thumb pointer (`addr & 1`)
        is decoded with the Thumb Sleigh spec and a halfword-aligned
        address, matching `analyze(...)`.

        Raises `StriderError` when `addr` is unmapped or a lift fails.
        """
        arch, addr = _effective_arch_and_addr(self._arch, addr)
        return _ext.pcode_at(arch, self._elf.reader(), addr, count)

    # ── Lift a function ──────────────────────────────────────────────

    def analyze(
        self,
        target: Union[str, int],
        cc: Optional[CallingConvention] = None,
        **opts,
    ):
        """Lift the function at `target` (symbol name or absolute
        address), returning the same `(Function, unresolved_addrs)`
        tuple as the base `Lifter.analyze`.

        A `str` target is resolved to an address via the ELF symbol
        table; when the symbol's recorded size is non-zero and the
        caller didn't pass an explicit `function_max_size`, the ELF's
        `st_size` is used as the bound automatically.  An `int` target
        is used verbatim.

        `cc` defaults to the ELF-derived (or explicitly-passed at
        construction) calling convention when omitted.  Every other
        keyword argument (`function_max_size`, `allow_code_before_start_addr`,
        `compact`, `per_address_ccs`, `calls_clobber`,
        `assume_distinct_sp_bases_disjoint`, `alias_mode`) forwards
        verbatim to the base `Lifter.analyze` — see its docstring for
        the full list.

        The read-only memory for `LoadReadOnly` constant folding is the
        ELF's runtime-immutable regions (code + read-only sections only;
        writable sections excluded), wired into the base `Lifter` at
        construction (and rebuilt by `add_elf`).

        Raises `TypeError` when `target` is neither `str` nor `int`.
        """
        if isinstance(target, str):
            addr, sym_size = self._elf.symbol_addr_and_size(target)
            # Honour the symbol's recorded size when the caller didn't
            # provide an explicit bound (zero-size symbols surface as
            # `None`).
            if opts.get("function_max_size") is None:
                opts["function_max_size"] = sym_size
        elif isinstance(target, int):
            addr = target
        else:
            raise TypeError(
                f"`target` must be a symbol name (str) or address (int), "
                f"got {type(target).__name__}"
            )

        # ARM Thumb interworking: strip the low bit for an interworking
        # entry; non-ARM arches pass through verbatim.  The base
        # `Lifter`'s Sleigh was built with the base arch, so — as before
        # this refactor — the lift itself follows that base-arch Sleigh;
        # only the address is adjusted here.
        _eff_arch, addr = _effective_arch_and_addr(self._arch, addr)

        if cc is None:
            cc = self._cc
        return super().analyze(addr, cc, **opts)


# ── Analysis ──────────────────────────────────────────────────────────────


class Analysis:
    """Optional wrapper around a lifted, optimized IR `Function` —
    manually built by a caller around a `(Function, unresolved)` result
    from `Lifter.analyze` / `ElfLifter.analyze` — with convenience
    methods for pattern queries and provenance lookup.  Neither
    `Lifter.analyze` nor `ElfLifter.analyze` constructs one
    automatically; both return the bare tuple, and a caller who wants
    the `find` / `fingerprint` / `fingerprint_pcode` conveniences wraps
    the result in an `Analysis` explicitly.
    """

    __slots__ = (
        "_function",
        "_entry",
        "_name",
        "_effective_arch",
        "_mem",
        "_unresolved_indirect_branches",
    )

    def __init__(
        self,
        function: Function,
        *,
        entry: int,
        name: Optional[str] = None,
        effective_arch: Optional[SleighArch] = None,
        mem: Optional[object] = None,
        unresolved_indirect_branches: Optional[list] = None,
    ) -> None:
        self._function = function
        self._entry = entry
        self._name = name
        # Machine addresses of indirect branches the orchestrator could
        # not resolve (empty when fully resolved).
        self._unresolved_indirect_branches = list(unresolved_indirect_branches or [])
        # The arch the lift actually used (Thumb-resolved for ARM
        # interworking entries).  `fingerprint_pcode` lifts the
        # fingerprint addresses through this so a Thumb function's
        # provenance is lifted with the Thumb Sleigh spec.  Required —
        # `fingerprint_pcode` cannot resolve provenance without it.
        if effective_arch is None:
            raise ValueError("Analysis requires an explicit effective_arch")
        self._effective_arch = effective_arch
        # The code reader to lift fingerprint addresses through.  For an
        # ELF-backed analysis this is the ELF's loaded regions.
        self._mem = mem

    # ── Properties ──────────────────────────────────────────────────

    @property
    def function(self) -> Function:
        """The underlying `Function` for direct access to the IR
        (preorder walks, `node_count`, `validate`, etc.)."""
        return self._function

    @property
    def unresolved_indirect_branches(self) -> list:
        """Machine addresses of indirect branches that could not be
        resolved (empty when fully resolved).  Assert this is empty to
        require complete indirect-branch resolution:
        `assert not a.unresolved_indirect_branches`."""
        return self._unresolved_indirect_branches

    @property
    def cfg(self):
        """The snapshot CFG the function was lifted from (pre-resolution
        for binaries that hit the indirect-branch loop)."""
        return self._function.cfg

    @property
    def sleigh(self):
        """A `Sleigh` handle for the arch the lift used — useful for
        register-name lookups (`reg(...)`).  The lift's own Sleigh is
        owned internally by the lifter and not handed out; this builds a
        fresh register-table handle for the same arch + code reader.
        Requires the analysis to carry a code reader (`mem`)."""
        if self._mem is None:
            raise ValueError(
                "Analysis.sleigh requires a code reader; this analysis was "
                "built without one (mem=None)"
            )
        from .strider import Sleigh  # noqa: PLC0415

        return Sleigh(self._effective_arch, self._mem)

    @property
    def entry(self) -> int:
        """The entry-point address that was lifted."""
        return self._entry

    @property
    def name(self) -> Optional[str]:
        """The symbol name passed to `analyze(...)`, or `None` if
        `analyze(<int>)` was used."""
        return self._name

    def __repr__(self) -> str:
        if self._name is not None:
            head = f"name={self._name!r}, entry={self._entry:#x}"
        else:
            head = f"entry={self._entry:#x}"
        return f"Analysis({head}, nodes={self._function.node_count()})"

    # ── Pattern queries ──────────────────────────────────────────────

    def find(self, pattern, **matcher_options) -> list:
        """Run a pattern against this function's IR.  Returns the
        list of `Match` objects.  Forwards every kwarg to
        `Function.find_all` (`ignore_casts`, `ignore_casts_mask`)."""
        return self._function.find_all(pattern, **matcher_options)

    def find_one(self, pattern, **matcher_options):
        """Run a pattern and return the first `Match`, or `None` if it
        does not match anywhere.  A one-shot convenience for the
        `hits = a.find(pat); hits[0] if hits else None` idiom.
        Forwards every kwarg to `Function.find_one` (`ignore_casts`,
        `ignore_casts_mask`)."""
        return self._function.find_one(pattern, **matcher_options)

    def find_joined(self, patterns, **matcher_options) -> list:
        """Run multiple patterns and return matched sets joined on
        shared `Capture`s — a cross-pattern join.  See
        `Function.find_joined` for the full contract."""
        return self._function.find_joined(patterns, **matcher_options)

    # ── Provenance ───────────────────────────────────────────────────

    @staticmethod
    def _coerce_node_id(node) -> int:
        """Coerce a node argument to a raw `u32` node id.

        Accepts a raw `int` id (e.g. `match.root`), a `Match` (its
        `.root` is used), or a `Node` handle (its `.id` is used).
        Raises `TypeError` for anything else.
        """
        if isinstance(node, int):
            return node
        if hasattr(node, "root"):
            # PyMatch.root is a property returning u32.
            return node.root
        if hasattr(node, "id"):
            # PyNode.id is a property returning the raw u32 arena index.
            return node.id
        raise TypeError(
            f"expected int, Match, or Node, got {type(node).__name__}"
        )

    def fingerprint(self, node) -> list[int]:
        """Return the asm-fingerprint of the given node — a
        sorted-deduplicated list of source machine-instruction
        addresses whose lifting (or subsequent rewrite) contributed
        to that node's value.

        `node` can be:
        * A raw `u32` node id (e.g. `match.root`).
        * A `Match` object — the match's root is used.
        * A `Node` handle — its `.id` is used.

        Returns an empty list for "structural" node kinds
        (Entry, InitialMemory, InitialVar, Region, and phis).
        See `ir::Graph::asm_fingerprint` for the full contract.
        """
        node_id = self._coerce_node_id(node)
        return self._function.asm_fingerprint(node_id)

    def fingerprint_pcode(self, node) -> list[tuple[int, str]]:
        """Return the asm-fingerprint of `node` as `(addr, text)` pairs,
        sorted by address — the p-code companion to `fingerprint`.

        Resolves the node's fingerprint addresses (see `fingerprint`),
        then lifts each through a single shared Sleigh build, so the
        audit trail reads as the lifted semantics without leaving
        strider.  `text` is the lifted p-code for that machine
        instruction (the instruction's p-code ops joined with `"; "`,
        empty for ops like `endbr64` that lift to no p-code).  rsleigh
        is a p-code lifter — this is the lifted semantics, NOT native
        assembly mnemonics.

        `node` can be a raw `u32` id, a `Match`, or a `Node` handle.

        Returns `[]` for "structural" nodes that carry no fingerprint
        (Entry, InitialMemory, InitialVar, Region, and phis).
        """
        addrs = self.fingerprint(node)
        if not addrs:
            return []
        if self._mem is None:
            raise ValueError(
                "fingerprint_pcode: no memory source was supplied to this "
                "Analysis"
            )
        pairs = _ext.pcode_at_addrs(self._effective_arch, self._mem, addrs)
        return sorted(pairs, key=lambda p: p[0])

    # ── Visualisation ────────────────────────────────────────────────

    def dump_html(self, path: str, style: Optional[str] = None) -> None:
        """Dump the IR graph as an interactive HTML file at `path`.
        `style` may be `"dark"` (default) or `"light"`."""
        self._function.to_html(path, style)

    def dump_dot(self, path: str) -> None:
        """Dump the IR graph as a raw .dot file at `path`."""
        self._function.to_dot(path)
