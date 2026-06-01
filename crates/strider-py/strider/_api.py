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
from typing import Callable, Iterator, Optional, Union

from . import strider as _ext  # the PyO3 cdylib re-exported here
from .strider import (
    CallingConvention,
    Function,
    MemoryMap,
    OptimizerPipeline,
    SleighArch,
)


# Sentinel distinguishing "argument not passed" from an explicit `None`.
# `None` is a meaningful value for `rom` / `function_max_size` /
# `per_address_ccs`, so the per-call overrides on `Analyzer.analyze`
# cannot use `None` as their default — they use `_UNSET` and fall back to
# the frozen default only when the caller left the argument untouched.
_UNSET = object()


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
        return _ext.pcode_at(arch, self._elf.memory_map(), addr, count)

    # ── Configure-once analysis handle ───────────────────────────────

    def analyzer(
        self,
        *,
        allow_code_before_start_addr: bool = False,
        function_max_size: Optional[int] = None,
        rom: Optional[object] = None,
        compact: bool = True,
        per_address_ccs: Optional[dict] = None,
        pipeline_factory: Optional[Callable[[], OptimizerPipeline]] = None,
    ) -> "Analyzer":
        """Build a frozen `Analyzer` over this Program's loaded ELF.

        The returned handle bundles the arch + cc + code reader + symbol
        source once; `analyzer.analyze(target)` then needs only the
        target function (a symbol name or address), and any frozen option
        can be overridden for a single call.  Useful for analysing many
        functions with one shared setup:

        ```python
        azr = prog.analyzer()
        for fn in prog.functions():
            a = azr.analyze(fn)
        ```

        The frozen-option keywords mirror `analyze(...)` plus the extra
        knobs (`compact`, `per_address_ccs`, `pipeline_factory`) that
        `Program.analyze` does not expose directly.
        """
        return Analyzer(
            arch=self._arch,
            cc=self._cc,
            mem=self._elf.memory_map(),
            rom=rom,
            allow_code_before_start_addr=allow_code_before_start_addr,
            function_max_size=function_max_size,
            compact=compact,
            per_address_ccs=per_address_ccs,
            pipeline_factory=pipeline_factory,
            _elf=self._elf,
            _program=self,
        )

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

        For analysing many functions with one shared setup, build an
        `Analyzer` once via `Program.analyzer(...)` and call its
        `analyze(target)` per function instead.
        """
        return self.analyzer().analyze(
            function,
            function_max_size=function_max_size,
            allow_code_before_start_addr=allow_code_before_start_addr,
            rom=rom,
        )


# ── Analysis ──────────────────────────────────────────────────────────────


class Analysis:
    """Wrapper around a `RunResult` — the lifted, optimized IR graph
    for a single function — with convenience methods for pattern
    queries and provenance lookup.
    """

    __slots__ = (
        "_result",
        "_program",
        "_entry",
        "_name",
        "_effective_arch",
        "_mem",
    )

    def __init__(
        self,
        result,  # strider.RunResult
        program: Optional[Program],
        entry: int,
        name: Optional[str] = None,
        effective_arch: Optional[SleighArch] = None,
        mem: Optional[object] = None,
    ) -> None:
        self._result = result
        self._program = program
        self._entry = entry
        self._name = name
        # The arch the lift actually used (Thumb-resolved for ARM
        # interworking entries).  `fingerprint_pcode` lifts the
        # fingerprint addresses through this so a Thumb function's
        # provenance is lifted with the Thumb Sleigh spec.  Defaults
        # to the program's arch for callers constructing an Analysis
        # directly; for a standalone (program=None) analysis the caller
        # MUST supply `effective_arch`.
        if effective_arch is not None:
            self._effective_arch = effective_arch
        elif program is not None:
            self._effective_arch = program._arch
        else:
            raise ValueError(
                "Analysis(program=None) requires an explicit effective_arch"
            )
        # The code reader to lift fingerprint addresses through when this
        # Analysis is not backed by a Program (the standalone `Analyzer`
        # path).  When `program` is set we read its loaded ELF regions
        # instead, preserving the existing Program-backed behaviour.
        self._mem = mem

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
        `Function.find_all` (`ignore_casts`, `ignore_casts_mask`)."""
        return self._result.function.find_all(pattern, **matcher_options)

    def find_one(self, pattern, **matcher_options):
        """Run a pattern and return the first `Match`, or `None` if it
        does not match anywhere.  A one-shot convenience for the
        `hits = a.find(pat); hits[0] if hits else None` idiom.
        Forwards every kwarg to `Function.find_one` (`ignore_casts`,
        `ignore_casts_mask`)."""
        return self._result.function.find_one(pattern, **matcher_options)

    def find_joined(self, patterns, **matcher_options) -> list:
        """Run multiple patterns and return matched sets joined on
        shared `Capture`s — a cross-pattern join.  See
        `Function.find_joined` for the full contract."""
        return self._result.function.find_joined(
            patterns, **matcher_options
        )

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
        return self._result.function.asm_fingerprint(node_id)

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
        # Prefer the Program's loaded regions (the ELF-backed path).
        # For a standalone analysis (program=None) fall back to the code
        # reader the Analyzer handed us at construction.
        if self._program is not None:
            mem = self._program._elf.memory_map()
        elif self._mem is not None:
            mem = self._mem
        else:
            raise ValueError(
                "fingerprint_pcode: no memory source (neither a backing "
                "Program nor a code reader was supplied to this Analysis)"
            )
        pairs = _ext.pcode_at_addrs(self._effective_arch, mem, addrs)
        return sorted(pairs, key=lambda p: p[0])

    # ── Visualisation ────────────────────────────────────────────────

    def dump_html(self, path: str, style: Optional[str] = None) -> None:
        """Dump the IR graph as an interactive HTML file at `path`.
        `style` may be `"dark"` (default) or `"light"`."""
        self._result.function.to_html(path, style)

    def dump_dot(self, path: str) -> None:
        """Dump the IR graph as a raw .dot file at `path`."""
        self._result.function.to_dot(path)


# ── Analyzer ────────────────────────────────────────────────────────────────


class Analyzer:
    """A frozen, configure-once analysis handle.

    Bundles the target description (arch + calling convention), the code
    reader, and a set of option defaults once; then `analyze(target)`
    needs only the target function (a symbol name or absolute address).
    Any frozen option can be overridden for a single call.

    Build one from a loaded ELF (`prog.analyzer(...)`) — which keeps
    symbol-name resolution via the ELF — or standalone for a raw
    firmware blob / custom source (address targets only):

    ```python
    # ELF-backed:
    azr = prog.analyzer()
    for fn in prog.functions():
        a = azr.analyze(fn)

    # Standalone (no symbol table — addresses only):
    azr = strider.analyzer(arch, cc, mem)
    a = azr.analyze(0x8000)
    ```

    The handle is frozen — its fields are set once in `__init__` and
    there are no public setters.  `pipeline_factory` (when supplied) is
    called fresh for every `analyze()` to sidestep the drain-on-use
    problem (a single `OptimizerPipeline` cannot be reused across calls).
    """

    __slots__ = (
        "_arch",
        "_cc",
        "_mem",
        "_rom",
        "_allow_code_before_start_addr",
        "_function_max_size",
        "_compact",
        "_per_address_ccs",
        "_pipeline_factory",
        "_elf",
        "_program",
    )

    def __init__(
        self,
        arch: SleighArch,
        cc: CallingConvention,
        mem: object,  # MemoryMap | MemReader
        *,
        rom: Optional[object] = None,
        allow_code_before_start_addr: bool = False,
        function_max_size: Optional[int] = None,
        compact: bool = True,
        per_address_ccs: Optional[dict] = None,
        pipeline_factory: Optional[Callable[[], OptimizerPipeline]] = None,
        _elf: Optional[object] = None,
        _program: Optional[Program] = None,
    ) -> None:
        self._arch = arch
        self._cc = cc
        self._mem = mem
        # Frozen option defaults.
        self._rom = rom
        self._allow_code_before_start_addr = allow_code_before_start_addr
        self._function_max_size = function_max_size
        self._compact = compact
        self._per_address_ccs = per_address_ccs
        self._pipeline_factory = pipeline_factory
        # ELF symbol source for `analyze(name)`.  ELF-backed analyzers
        # carry the loaded ELF (`symbol_addr_and_size` + the symbol's
        # recorded size); standalone analyzers leave this `None` and
        # reject name targets with a clear error.
        self._elf = _elf
        # When this analyzer was built from a `Program`, the resulting
        # `Analysis` is backed by it (so `fingerprint_pcode` uses the
        # ELF regions, matching `Program.analyze`).  Standalone
        # analyzers leave this `None`.
        self._program = _program

    # ── Introspection ───────────────────────────────────────────────

    @property
    def arch(self) -> SleighArch:
        """The target architecture (`SleighArch`)."""
        return self._arch

    @property
    def cc(self) -> CallingConvention:
        """The default calling convention (`CallingConvention`)."""
        return self._cc

    def __repr__(self) -> str:
        return (
            f"Analyzer(arch={self._arch.name()}, cc={self._cc.name()}, "
            f"symbols={self._elf is not None})"
        )

    # ── Target resolution ───────────────────────────────────────────

    def _resolve_target(
        self, function: Union[str, int], function_max_size: Optional[int]
    ) -> tuple[int, Optional[str], Optional[int]]:
        """Resolve a target to `(addr, name, function_max_size)`.

        Mirrors `Program.analyze`'s symbol handling: a name is resolved
        via the ELF (with its recorded size defaulting the bound when no
        explicit bound is given); an int passes through as an address.
        Raises a clear error for a name target on a standalone analyzer
        (no ELF symbol source), or for a wrong-typed target.
        """
        if isinstance(function, str):
            if self._elf is None:
                raise ValueError(
                    f"cannot resolve symbol name {function!r}: this analyzer "
                    f"has no ELF symbol source.  Pass an int address instead, "
                    f"or build the analyzer from a Program "
                    f"(prog.analyzer(...))."
                )
            addr, sym_size = self._elf.symbol_addr_and_size(function)
            # Honour the symbol's recorded size when the caller didn't
            # provide an explicit bound (matching `Program.analyze`;
            # zero-size symbols surface as `None`).
            if function_max_size is None:
                function_max_size = sym_size
            name: Optional[str] = function
        elif isinstance(function, int):
            addr = function
            name = None
        else:
            raise TypeError(
                f"`function` must be a symbol name (str) or address (int), "
                f"got {type(function).__name__}"
            )
        return addr, name, function_max_size

    # ── P-code ───────────────────────────────────────────────────────

    def pcode(self, addr: int, count: int = 1) -> list[tuple[int, str]]:
        """Lift the p-code of `count` machine instructions starting at
        `addr` over this analyzer's memory + effective arch.

        Parity with `Program.pcode`: ARM Thumb interworking is honoured
        (a Thumb pointer `addr & 1` is decoded with the Thumb Sleigh spec
        and a halfword-aligned address).  Raises `StriderError` when
        `addr` is unmapped or a lift fails.
        """
        arch, addr = _effective_arch_and_addr(self._arch, addr)
        return _ext.pcode_at(arch, self._mem, addr, count)

    # ── Analyse a function ───────────────────────────────────────────

    def analyze(
        self,
        function: Union[str, int],
        *,
        function_max_size=_UNSET,
        allow_code_before_start_addr=_UNSET,
        rom=_UNSET,
        compact=_UNSET,
        per_address_ccs=_UNSET,
        pipeline=_UNSET,
    ) -> "Analysis":
        """Lift the function at `function` (symbol name or address) into
        an `Analysis`, using this analyzer's frozen configuration.

        `function` is the only required argument.  Each keyword, when
        passed, OVERRIDES the frozen default for this one call (the
        `_UNSET` sentinel distinguishes "not passed" from an explicit
        `None`, since `None` is meaningful for `rom` /
        `function_max_size` / `per_address_ccs`).  `pipeline` (a one-off
        `OptimizerPipeline`) overrides the frozen `pipeline_factory` for
        this call only.
        """
        # Per-call override resolution: a value other than `_UNSET` wins
        # over the frozen default.
        allow_before = (
            self._allow_code_before_start_addr
            if allow_code_before_start_addr is _UNSET
            else allow_code_before_start_addr
        )
        eff_rom = self._rom if rom is _UNSET else rom
        eff_compact = self._compact if compact is _UNSET else compact
        eff_per_address_ccs = (
            self._per_address_ccs
            if per_address_ccs is _UNSET
            else per_address_ccs
        )
        eff_max_size = (
            self._function_max_size
            if function_max_size is _UNSET
            else function_max_size
        )

        addr, name, eff_max_size = self._resolve_target(function, eff_max_size)

        # ARM Thumb interworking: switch to the Thumb Sleigh spec and
        # strip the low bit for an interworking entry; non-ARM arches
        # pass through verbatim.
        arch, addr = _effective_arch_and_addr(self._arch, addr)

        # The pipeline is drained on use, so a single instance can't be
        # reused across calls.  A per-call `pipeline=` wins; otherwise
        # the frozen `pipeline_factory` is invoked FRESH per call.
        if pipeline is not _UNSET:
            eff_pipeline = pipeline
        elif self._pipeline_factory is not None:
            eff_pipeline = self._pipeline_factory()
        else:
            eff_pipeline = None

        result = _ext.run(
            arch=arch,
            cc=self._cc,
            mem=self._mem,
            entry=addr,
            rom=eff_rom if eff_rom is not None else self._mem,
            pipeline=eff_pipeline,
            allow_code_before_start_addr=allow_before,
            function_max_size=eff_max_size,
            compact=eff_compact,
            per_address_ccs=eff_per_address_ccs,
        )
        return Analysis(
            result=result,
            program=self._program,
            entry=addr,
            name=name,
            effective_arch=arch,
            mem=self._mem,
        )


def analyzer(
    arch: SleighArch,
    cc: CallingConvention,
    mem: object,  # MemoryMap | MemReader
    *,
    rom: Optional[object] = None,
    allow_code_before_start_addr: bool = False,
    function_max_size: Optional[int] = None,
    compact: bool = True,
    per_address_ccs: Optional[dict] = None,
    pipeline_factory: Optional[Callable[[], OptimizerPipeline]] = None,
) -> Analyzer:
    """Build a standalone `Analyzer` over a raw code reader.

    The non-ELF / firmware-blob counterpart to `Program.analyzer(...)`:
    there is no ELF symbol source, so only address targets are accepted
    — a name target raises a clear error.  If you have a name → address
    map, do the lookup at the call site.

    ```python
    mem = strider.MemoryMap()
    mem.add_region(0x8000, firmware_bytes)
    azr = strider.analyzer(
        strider.SleighArch.arm_thumb(),
        strider.CallingConvention.arm_aapcs(),
        mem,
    )
    a = azr.analyze(0x8000)
    ```
    """
    return Analyzer(
        arch=arch,
        cc=cc,
        mem=mem,
        rom=rom,
        allow_code_before_start_addr=allow_code_before_start_addr,
        function_max_size=function_max_size,
        compact=compact,
        per_address_ccs=per_address_ccs,
        pipeline_factory=pipeline_factory,
    )
