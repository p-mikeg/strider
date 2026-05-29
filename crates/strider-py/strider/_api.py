"""High-level Python facade for the strider pipeline.

The lower-level building blocks (`MemoryMap`, `Sleigh`, `Strider`,
`Function`, `Matcher`, `OptimizerPipeline`, etc.) remain on the
top-level `strider` namespace for users who need granular control.
This module adds the three-line convenience workflow:

```python
import strider
prog = strider.load("path/to/file.elf")   # → Program
a = prog.analyze("function_name")          # → Analysis
matches = a.find(strider.pattern.call())   # → list[Match]
```

`strider.load(path)` returns a `Program` — one object that *is* the
loaded ELF.  It wires the code + ROM readers internally and exposes
symbols, sizes, the entry point, raw reads, and `analyze()`.
`MemoryMap` drops back to a low-level reader of raw byte regions for
non-ELF / custom-source / firmware-blob cases.

Arch detection is done by reading the first 20 bytes of the ELF
header (magic + EI_CLASS + EI_DATA + e_machine).  No pyelftools
dependency at runtime.
"""

from __future__ import annotations

import os
import struct
from typing import Iterator, Optional, Union

from . import strider as _ext  # the PyO3 cdylib re-exported here
from .strider import (
    CallingConvention,
    Function,
    MemoryMap,
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
    explicit `arch=` / `cc=` to `strider.load(...)`.
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
        # explicit `Strider(...)` constructor.
        arch = SleighArch.ppc64le() if le else SleighArch.ppc64be()
        return arch, CallingConvention.powerpc64_elf_v2()
    raise ValueError(
        f"unsupported ELF e_machine={em}; pass an explicit SleighArch + "
        f"CallingConvention via strider.load(path, arch=..., cc=...) instead"
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


# ── Program facade ──────────────────────────────────────────────────────────


def load(
    path: str,
    *,
    arch: Optional[SleighArch] = None,
    cc: Optional[CallingConvention] = None,
    apply_relocations: bool = False,
) -> "Program":
    """Load an ELF binary and return a `Program` — one object that *is*
    the loaded binary, with the arch and userland calling convention
    auto-picked from the ELF header.

    For non-userland workflows (Linux kernel, syscall stubs, embedded
    firmware with a custom CC) pass an explicit `arch=` / `cc=`, e.g.:

    ```python
    prog = strider.load(
        "vmlinux",
        apply_relocations=True,
        cc=strider.CallingConvention.x86_64_linux_kernel(),
    )
    ```

    `apply_relocations` widens the loaded section set (`.data.rel.ro`,
    `.got`, …) and patches every understood relocation in-place — set
    it for ET_DYN binaries (kernels, PIE userland) whose `.text` or
    function-pointer tables ship with unresolved relocations.

    Raises:
      * `FileNotFoundError` — path does not exist.
      * `ValueError` — file is not an ELF or the e_machine is not one
        of the supported architectures (only when `arch`/`cc` aren't
        both supplied).
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
    elf = _ext.load_elf(path, apply_relocations)
    return Program(elf=elf, arch=arch, cc=cc, _elf_path=path, _header=header)


class Program:
    """The loaded ELF binary: one object that wires the code + ROM
    readers internally and exposes symbols, sizes, the entry point,
    raw reads, and `analyze()`.

    Constructed via `strider.load(path)` — for the auto-detected
    common case, or with explicit `arch=` / `cc=` for kernel / syscall
    / custom-ABI workflows:

    ```python
    prog = strider.load(
        "vmlinux",
        apply_relocations=True,
        cc=strider.CallingConvention.x86_64_linux_kernel(),
    )
    a = prog.analyze("schedule")
    ```
    """

    __slots__ = ("_elf", "_arch", "_cc", "_elf_path", "_header")

    def __init__(
        self,
        elf: object,  # strider._LoadedElf
        arch: SleighArch,
        cc: CallingConvention,
        *,
        _elf_path: Optional[str] = None,
        _header: Optional[_ElfHeader] = None,
    ) -> None:
        self._elf = elf
        self._arch = arch
        self._cc = cc
        self._elf_path = _elf_path
        self._header = _header

    # ── Properties for introspection / advanced use ─────────────────

    @property
    def arch(self) -> SleighArch:
        """The target architecture (`SleighArch`) this program uses."""
        return self._arch

    @property
    def cc(self) -> CallingConvention:
        """The default calling convention (`CallingConvention`) applied
        when analysing functions."""
        return self._cc

    def __repr__(self) -> str:
        path = self._elf_path or "<no path>"
        return f"Program(elf={path!r}, arch={self._arch.name()}, cc={self._cc.name()})"

    # ── Symbol lookup / raw reads (delegate to the loaded ELF) ───────

    def functions(self) -> Iterator[str]:
        """Yield every function/data symbol name from every ELF loaded
        into this Program.  Symbols with empty names or zero addresses
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

    def add_elf(self, path: str, *, apply_relocations: bool = False) -> None:
        """Merge another ELF (e.g. a shared library) into this Program:
        extends the loaded regions and the symbol set.  The
        earlier-loaded ELF wins on symbol-name collisions."""
        self._elf.add_elf(path, apply_relocations)

    # ── Lift a function ──────────────────────────────────────────────

    def analyze(
        self,
        function: Union[str, int],
        *,
        function_max_size: Optional[int] = None,
        allow_code_before_start_addr: bool = False,
        rom: Optional[object] = None,
    ) -> "Analysis":
        """Lift the function at `function` (symbol name or absolute
        address) into an `Analysis`.

        Drives the full orchestrator pipeline: builds the CFG,
        lifts to IR, runs the stable + destructive optimizers, and
        resolves indirect branches via the fixed-point loop.

        Optional arguments:
        * `function_max_size` — bound the lift to `[entry, entry+N)`;
          useful when the ELF's `st_size` is zero (stripped binary)
          or when you only want a prefix.  When the symbol's
          recorded size is non-zero and no explicit value is passed,
          the ELF's `st_size` is used automatically.
        * `allow_code_before_start_addr` — let the lifter follow
          backward branches below `entry`.  Required for trampolines
          / cold-jumps that reach a label before the prologue.
        * `rom` — the read-only memory image for `LoadReadOnly`
          constant folding.  Defaults to this Program's loaded regions,
          matching the typical "the ELF's `.rodata` is the rom" case.
        """
        mem = self._elf.memory_map()
        if isinstance(function, str):
            addr, sym_size = self._elf.symbol_addr_and_size(function)
            # Honour the symbol's recorded size when the caller didn't
            # provide an explicit bound.  `function_max_size=0` is
            # rejected by `strider.run`, and zero-size symbols
            # surface as `None` from `symbol_addr_and_size`, so the
            # `or None` is a no-op here for the common case.
            if function_max_size is None:
                function_max_size = sym_size
        elif isinstance(function, int):
            addr = function
        else:
            raise TypeError(
                f"`function` must be a symbol name (str) or address (int), "
                f"got {type(function).__name__}"
            )

        # ARM Thumb interworking: a Thumb function entry (symbol or raw
        # address) has its low bit set.  Switch to the Thumb Sleigh spec
        # and strip the bit so the lift uses the correct decoder and a
        # halfword-aligned address.  Non-ARM arches pass through verbatim.
        arch, addr = _effective_arch_and_addr(self._arch, addr)

        result = _ext.run(
            arch=arch,
            cc=self._cc,
            mem=mem,
            entry=addr,
            rom=rom if rom is not None else mem,
            allow_code_before_start_addr=allow_code_before_start_addr,
            function_max_size=function_max_size,
        )
        return Analysis(result=result, program=self, entry=addr, name=(function if isinstance(function, str) else None))


# ── Analysis ──────────────────────────────────────────────────────────────


class Analysis:
    """Wrapper around a `RunResult` — the lifted, optimized IR graph
    for a single function — with convenience methods for pattern
    queries and provenance lookup.
    """

    __slots__ = ("_result", "_program", "_entry", "_name")

    def __init__(
        self,
        result,  # strider.RunResult
        program: Program,
        entry: int,
        name: Optional[str] = None,
    ) -> None:
        self._result = result
        self._program = program
        self._entry = entry
        self._name = name

    # ── Properties ──────────────────────────────────────────────────

    @property
    def function(self) -> Function:
        """The underlying `Function` for direct access to the IR
        (preorder walks, `node_count`, `validate`, etc.)."""
        return self._result.function

    @property
    def cfg(self):
        """The snapshot CFG built by the orchestrator (pre-resolution
        for binaries that hit the indirect-branch loop)."""
        return self._result.cfg

    @property
    def sleigh(self):
        """The `Sleigh` handle used by the lift — needed for dot
        dumping (`dump_html` / `dump_dot`).  Held for as long as
        this Analysis is alive."""
        return self._result.sleigh

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
        return f"Analysis({head}, nodes={self._result.function.node_count()})"

    # ── Pattern queries ──────────────────────────────────────────────

    def find(self, pattern, **matcher_options) -> list:
        """Run a pattern against this function's IR.  Returns the
        list of `Match` objects.  Forwards every kwarg to
        `Function.find_all` (`ignore_casts`, `ignore_casts_mask`,
        `ignore_regions`)."""
        return self._result.function.find_all(pattern, **matcher_options)

    def find_all_requirements(self, patterns, **matcher_options) -> list:
        """Run multiple patterns and intersect their matches on
        shared `Capture` objects.  See
        `Function.find_all_requirements` for the full contract."""
        return self._result.function.find_all_requirements(
            patterns, **matcher_options
        )

    # ── Provenance ───────────────────────────────────────────────────

    def fingerprint(self, node) -> list[int]:
        """Return the asm-fingerprint of the given node — a
        sorted-deduplicated list of source machine-instruction
        addresses whose lifting (or subsequent rewrite) contributed
        to that node's value.

        `node` can be either:
        * A raw `u32` node id (e.g. `match.root`).
        * A `Match` object — the match's root is used.

        Returns an empty list for "structural" node kinds
        (Entry, InitialMemory, phis, Region, FunctionArg).
        See `ir::Graph::asm_fingerprint` for the full contract.
        """
        if isinstance(node, int):
            node_id = node
        elif hasattr(node, "root"):
            # PyMatch.root is a property returning u32.
            node_id = node.root
        else:
            raise TypeError(
                f"fingerprint(node): expected int or Match, got "
                f"{type(node).__name__}"
            )
        return self._result.function.asm_fingerprint(node_id)

    # ── Visualisation ────────────────────────────────────────────────

    def dump_html(self, path: str, style: Optional[str] = None) -> None:
        """Dump the IR graph as an interactive HTML file at `path`.
        `style` may be `"dark"` (default) or `"light"`."""
        self._result.function.to_html(path, style)

    def dump_dot(self, path: str) -> None:
        """Dump the IR graph as a raw .dot file at `path`."""
        self._result.function.to_dot(path)
