"""The ELF loader and the `ElfLifter` it returns.

```python
import strider
lift = strider.lift.load_elf("path/to/file.elf")
cfg, function, unresolved = lift.analyze("function_name")
matches = function.find_all(strider.pattern.call())
```

For other sources use `strider.lift.lifter(arch, mem, rom=None)`.
"""

from __future__ import annotations

import os
import stat as _stat
import struct
from typing import TYPE_CHECKING, Any, Mapping, Optional, Union

# By full dotted path, independent of what `__init__.py` binds.
import importlib as _importlib

_ext = _importlib.import_module("strider._strider")

if TYPE_CHECKING:
    # `_ext` attributes are `Any` to a checker; these are the typed stubs.
    from .cfg import Cfg, CfgOptions
    from .lift import AnalyzeResult, Lifter, LifterOptions
    from .pattern import CaptureKey, LoadPat, Match, Pat, StorePat, ValueLike
    from .reader import BufferReader, MemLike, RomLike, Symbol, SymbolIter
    from .sleigh import CallingConvention, SleighArch
else:
    CallingConvention = _ext.sleigh.CallingConvention
    SleighArch = _ext.sleigh.SleighArch
    Lifter = _ext.lift.Lifter
    LifterOptions = _ext.lift.LifterOptions
    CfgOptions = _ext.cfg.CfgOptions

StriderError = _ext.StriderError

#: A filesystem path: `str` or any `os.PathLike[str]`.
StrPath = Union[str, "os.PathLike[str]"]


# e_machine codes (System V gABI).
_EM_386 = 3
_EM_MIPS = 8
_EM_PPC = 20
_EM_PPC64 = 21
_EM_ARM = 40
_EM_X86_64 = 62
_EM_AARCH64 = 183

# ARM `e_flags` float-ABI bit for a soft or softfp image.
_EF_ARM_ABI_FLOAT_SOFT = 0x0000_0200

# MIPS `e_flags`: `EF_MIPS_ABI2` marks n32, and the `EF_MIPS_ABI` field names
# O32 (0x1000, or 0 on an n32/n64 image) or one of these three.
_EF_MIPS_ABI2 = 0x0000_0020
_EF_MIPS_ABI = 0x0000_F000
_EF_MIPS_ABI_NAMES = {0x2000: "O64", 0x3000: "EABI32", 0x4000: "EABI64"}
# `EF_MIPS_ARCH_ASE_MICROMIPS`: the image's code is microMIPS.
_EF_MIPS_MICROMIPS = 0x0200_0000

# PowerPC Linux `KERNELBASE`: at or above the user address space, so no
# userland entry point reaches it.
_PPC32_KERNELBASE = 0xC000_0000
_PPC64_KERNELBASE = 0xC000_0000_0000_0000


class _ElfHeader:
    """The ELF header fields that pick the `SleighArch` and
    `CallingConvention` preset."""

    __slots__ = ("is_64bit", "is_little_endian", "e_machine", "e_entry", "e_flags")

    def __init__(self, path: str) -> None:
        # Before opening: opening a FIFO blocks until a writer appears.
        if not _stat.S_ISREG(os.stat(path).st_mode):
            raise ValueError(f"{path!r}: not a regular file")
        with open(path, "rb") as f:
            header = f.read(52)
        if header.startswith(b"version https://git-lfs.github.com/spec/v1"):
            raise ValueError(f"{path!r}: a Git LFS pointer, not an ELF; run `git lfs pull`")
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
        order = "<" if self.is_little_endian else ">"
        (self.e_machine,) = struct.unpack(order + "H", header[18:20])
        # `e_entry` is at offset 24 on both classes; 0 on a relocatable object.
        addr_fmt = order + ("Q" if self.is_64bit else "I")
        size = 8 if self.is_64bit else 4
        (self.e_entry,) = (
            struct.unpack(addr_fmt, header[24 : 24 + size])
            if len(header) >= 24 + size
            else (0,)
        )
        # `e_flags`: offset 36 on ELF32, 48 on ELF64.
        off = 48 if self.is_64bit else 36
        self.e_flags = (
            struct.unpack(order + "I", header[off : off + 4])[0]
            if len(header) >= off + 4
            else 0
        )


def _refuse_micromips(header: _ElfHeader, remedy: str) -> None:
    """Refuse a microMIPS image: every MIPS preset decodes an ISA-bit address
    as MIPS16e."""
    if header.e_machine == _EM_MIPS and header.e_flags & _EF_MIPS_MICROMIPS:
        raise ValueError(
            f"unsupported microMIPS image: strider has no microMIPS preset; {remedy}"
        )


def _arch_and_cc_for_elf(
    header: _ElfHeader,
    is_arm_be8: bool = False,
) -> tuple[SleighArch, CallingConvention]:
    """The `SleighArch` and `CallingConvention` preset for this ELF. Raises
    `ValueError` for an unsupported e_machine, an ELF32 MIPS ABI other than
    O32, and a microMIPS image.

    The convention is the userland one except on PowerPC, where an `e_entry`
    at or above `KERNELBASE` selects the kernel one (no vector registers).
    x86's kernel ABI has no header bit and needs an explicit `cc=`; a Thumb
    image needs `arch=SleighArch.arm_thumb()`.
    """
    em = header.e_machine
    le = header.is_little_endian
    is64 = header.is_64bit

    if em == _EM_386:
        return SleighArch.x86(), CallingConvention.x86_cdecl()
    if em == _EM_X86_64:
        return SleighArch.x86_64(), CallingConvention.x86_64_systemv()
    if em == _EM_ARM:
        if le:
            arch = SleighArch.arm()
        elif is_arm_be8:
            # BE8 stores instructions little-endian.
            arch = SleighArch.arm_be_kernel()
        else:
            arch = SleighArch.arm_be()
        # A soft/softfp image passes floats in r0-r3. Only a linked image sets
        # the bit; a relocatable object falls to hard-float.
        if header.e_flags & _EF_ARM_ABI_FLOAT_SOFT:
            return arch, CallingConvention.arm_aapcs_soft()
        return arch, CallingConvention.arm_aapcs()
    if em == _EM_AARCH64:
        arch = SleighArch.aarch64() if le else SleighArch.aarch64be()
        return arch, CallingConvention.aarch64_aapcs64()
    if em == _EM_MIPS:
        _refuse_micromips(header, "pass arch= and cc= to lift its MIPS32 code")
        if is64:
            arch = SleighArch.mipsle64() if le else SleighArch.mipsbe64()
            return arch, CallingConvention.mips_n64()
        # O32 is the only ELF32 MIPS preset; lifting another ABI as O32 loses
        # the arguments past the fourth.
        abi = _EF_MIPS_ABI_NAMES.get(header.e_flags & _EF_MIPS_ABI)
        if header.e_flags & _EF_MIPS_ABI2:
            abi = "n32"
        if abi is not None:
            raise ValueError(
                f"unsupported MIPS ABI {abi}: strider has no preset for it; "
                f"pass an explicit SleighArch + CallingConvention via "
                f"strider.lift.load_elf(path, arch=..., cc=...) instead"
            )
        arch = SleighArch.mipsle32() if le else SleighArch.mipsbe32()
        return arch, CallingConvention.mips_o32()
    if em == _EM_PPC:
        arch = SleighArch.ppc32le() if le else SleighArch.ppc32be()
        if header.e_entry >= _PPC32_KERNELBASE:
            return arch, CallingConvention.powerpc_sysv32_linux_kernel()
        return arch, CallingConvention.powerpc_sysv32()
    if em == _EM_PPC64:
        arch = SleighArch.ppc64le() if le else SleighArch.ppc64be()
        is_kernel = header.e_entry >= _PPC64_KERNELBASE
        # ABI level: low two bits of `e_flags`. LE is ELFv2 only; BE with 0
        # predates the flag and is ELFv1.
        if le or (header.e_flags & 0x3) == 2:
            if is_kernel:
                return arch, CallingConvention.powerpc64_elf_v2_linux_kernel()
            return arch, CallingConvention.powerpc64_elf_v2()
        if is_kernel:
            return arch, CallingConvention.powerpc64_elf_v1_linux_kernel()
        return arch, CallingConvention.powerpc64_elf_v1()
    raise ValueError(
        f"unsupported ELF e_machine={em}; pass an explicit SleighArch + "
        f"CallingConvention via strider.lift.load_elf(path, arch=..., cc=...) instead"
    )


def load_elf(
    path: StrPath,
    *,
    from_segments: bool = True,
    apply_relocations: bool = True,
    relocated_got_is_rom: bool = False,
    arch: Optional[SleighArch] = None,
    cc: Optional[CallingConvention] = None,
) -> "ElfLifter":
    """Load an ELF binary as an `ElfLifter`, detecting the architecture and
    calling convention from its header.

    `from_segments=True` maps PT_LOAD segments (sections for a relocatable
    object); `False` maps sections. `arch=` / `cc=` override the detected
    preset; with `arch=` alone, `analyze` raises `StriderError` if the
    header's convention has another arch's stack geometry.

    `apply_relocations=True` relocates the image (Android packed tables
    included); `False` drops writable non-executable sections and serves
    relocation fields as their file bytes. With it on, `StriderError` is
    raised for tables naming more than twice the entries the file can hold,
    or an unreadable table that may relocate code; any other image with an
    unreadable table serves none of its writable mappings.

    `relocated_got_is_rom=True` also serves `.got` (PPC32 `.got2`) and
    `.data.rel.ro`, never `.got.plt`, as read-only, so a load through a GOT
    slot folds to the address it holds. Slots only the dynamic loader
    computes (imports, IFUNCs, copy relocations, lazy-binding words) stay
    unmapped. The flag is an unchecked claim that nothing rewrites a slot
    after loading and each symbol binds to its own definition (`LD_PRELOAD`
    or code writing its GOT makes it false). With `apply_relocations=False`
    it raises `StriderError`.

    Raises `FileNotFoundError` for a missing `path`, `ValueError` for a
    non-ELF or an unsupported architecture or ABI (microMIPS included), and
    `StriderError` for a malformed ELF. Passing both `arch=` and `cc=` skips
    detection and with it the ABI and microMIPS refusals.
    """
    # A non-path argument is a `TypeError` here, not a `FileNotFoundError`.
    path = os.fspath(path)
    if not os.path.exists(path):
        raise FileNotFoundError(path)
    header = _ElfHeader(path)
    elf = _ext._load_elf(path, from_segments, apply_relocations, relocated_got_is_rom)
    if arch is None or cc is None:
        det_arch, det_cc = _arch_and_cc_for_elf(header, elf.is_arm_be8)
        arch = arch if arch is not None else det_arch
        cc = cc if cc is not None else det_cc
    return ElfLifter(elf, arch, cc, elf.reader(), rom=elf.ro_reader())


class ElfLifter(Lifter):
    """A loaded ELF as a `Lifter` (from `load_elf`), with its symbol table
    and an `analyze` that accepts a symbol name.

    ```python
    lift = strider.lift.load_elf("path/to/file.elf")
    for sym in lift.functions():
        cfg, function, unresolved = lift.analyze(sym.address)
    ```
    """

    __slots__ = ("_elf", "_cc")

    def __new__(
        cls,
        elf: Any,  # the internal loaded-ELF handle the loader returns
        arch: SleighArch,
        cc: CallingConvention,
        mem: "MemLike",
        rom: Optional["RomLike"] = None,
    ) -> "ElfLifter":
        """Build the base `Lifter`; `__init__` attaches the ELF state."""
        return super().__new__(cls, arch, mem, rom=rom)

    def __init__(
        self,
        elf: Any,
        arch: SleighArch,
        cc: CallingConvention,
        mem: "MemLike",
        rom: Optional["RomLike"] = None,
    ) -> None:
        """Do not call this directly; use `load_elf`."""
        del arch, mem, rom  # consumed by `__new__`
        self._elf = elf
        self._cc = cc

    @property
    def cc(self) -> CallingConvention:
        """The calling convention `analyze` uses when none is passed."""
        return self._cc

    @property
    def endianness(self) -> str:
        """The first ELF's `EI_DATA` byte order: `"little"` or `"big"`.
        Answers even while a call holds the handle."""
        return "little" if self._elf.is_little_endian else "big"

    @property
    def is_arm_be8(self) -> bool:
        """Whether the binary sets ARM's `EF_ARM_BE8` (little-endian code,
        big-endian data; selects `arm_be_kernel`). `False` off ARM."""
        return self._elf.is_arm_be8

    def __repr__(self) -> str:
        try:
            arch = self.arch.name()
        except StriderError:
            # The handle is busy; a repr never raises.
            return f"ElfLifter(cc={self._cc.name()})"
        return f"ElfLifter(arch={arch}, cc={self._cc.name()})"

    def functions(self) -> SymbolIter:
        """The function symbols in address order, one per address (preferring
        a sized alias). A cursor: `len()` is what is left to pull."""
        return self._elf.functions()

    def iter_symbols(self) -> SymbolIter:
        """Every symbol. A cursor: `len()` is what is left to pull."""
        return self._elf.iter_symbols()

    def symbol(self, name: str) -> "Symbol":
        """The `Symbol` named `name`: its definition in the first ELF that
        defines it, else an undefined symbol that still has an address (a PLT
        stub, or an object file's extern). Raises `StriderError` when there is
        neither."""
        return self._elf.symbol(name)

    def symbol_opt(self, name: str) -> Optional["Symbol"]:
        """`symbol`, but `None` rather than raising."""
        return self._elf.symbol_opt(name)

    def symbol_at(self, address: int) -> Optional["Symbol"]:
        """The nearest symbol at or below `address` whose extent reaches it,
        or `None`. An unsized symbol covers only its own address. Aliases rank
        sized first, then code, so a sized data alias outranks an unsized
        function."""
        return self._elf.symbol_at(address)

    def symbols(self) -> Mapping[str, "Symbol"]:
        """Every symbol, keyed by the name it resolves under."""
        return self._elf.symbols()

    def entry_point(self) -> int:
        """The entry-point address of the first loaded ELF."""
        return self._elf.entry_point()

    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` bytes at `addr`, or `None` when unmapped or inside an
        uncomputed relocation. In writable non-executable data such a field
        serves its file bytes instead. `rom()` serves no writable data except
        under `relocated_got_is_rom`, where such a field is unmapped."""
        return self._elf.read(addr, size)

    def add_elf(
        self,
        path: StrPath,
        *,
        apply_relocations: bool = True,
        relocated_got_is_rom: bool = False,
    ) -> None:
        """Merge another ELF, such as a shared library, into this handle; the
        earlier-loaded ELF wins name collisions. `apply_relocations` and
        `relocated_got_is_rom` are as in `load_elf`, for this image only.

        Raises `StriderError` if the new ELF's architecture, class, byte order
        or (ARM) `EF_ARM_BE8` differs from the first image's, or it maps code
        over an address already loaded with different bytes. Raises
        `ValueError` for a microMIPS image."""
        path = os.fspath(path)
        try:
            header = _ElfHeader(path)
        except (OSError, ValueError):
            # Not a readable ELF: the merge reports it.
            header = None
        if header is not None:
            _refuse_micromips(header, "add_elf cannot merge it")
        # One call, so a busy handle refuses before anything is merged.
        self._merge_elf(self._elf, path, apply_relocations, relocated_got_is_rom)

    def add_symbol_file(self, path: StrPath) -> None:
        """Add the symbols (`.symtab` and `.dynsym`) of `path` and none of its
        bytes, e.g. a separate debug file `add_elf` would refuse as an
        overlap. Already-loaded ELFs keep a colliding name."""
        self._elf.add_symbol_file(os.fspath(path))

    def add_symbols(
        self,
        symbols: "dict[str, Union[int, tuple[int, int]]]",
        *,
        is_function: bool = True,
    ) -> None:
        """Add symbols from outside any ELF (a map file, `System.map`). Each
        value is an address or an `(address, size)` pair; the size lets
        `symbol_at` resolve interior addresses. An ELF defining a name keeps
        it."""
        self._elf.add_symbols(symbols, is_function=is_function)

    if TYPE_CHECKING:

        def reader(self) -> BufferReader:
            """The `BufferReader` over the regions `load_elf` mapped."""
            ...

    def build_cfg(self, entry: int, opts: Optional[CfgOptions] = None) -> Cfg:
        """`Lifter.build_cfg` with the ELF-derived options `analyze` uses:
        `$d` mapping symbols as `data_ranges` and the entry's `<name>.cold`
        symbols as `fragment_ranges`, each unless `opts` names some, and
        ppc64 ELFv2 local entries for `no_return_targets`. `entry` is an
        address; no symbol size bounds the build."""
        return self._build_cfg_elf(self._elf, entry, opts)

    def analyze(
        self,
        entry: Union[str, int],
        cc: Optional[CallingConvention] = None,
        opts: Optional[LifterOptions] = None,
    ) -> AnalyzeResult:
        """`Lifter.analyze` at `entry`, a symbol name or an address. `cc`
        defaults to this handle's convention.

        A `str` entry's recorded size bounds the lift (dropping
        `allow_code_before_start_addr`) unless `opts.cfg.function_max_size` is
        set. The ELF's `$d` mapping symbols and the entry's `<name>.cold[.N]`
        symbols fill empty `data_ranges` and `fragment_ranges`. A
        `no_return_targets` address or `per_address_ccs` key at a ppc64 ELFv2
        symbol's `address` also covers its `Symbol.local_entry`.

        Raises `TypeError` when `entry` is neither `str` nor `int`, and
        `StriderError` when the binary changed on disk since loading.
        """
        return self._analyze_elf(self._elf, entry, self._cc if cc is None else cc, opts)


class FieldPat:
    """A struct field at `base + offset`. `pattern` also matches a bare
    `base` (offset 0 folds away), for which `offset(m)` is 0."""

    __slots__ = ("pattern", "capture", "_known")

    def __init__(
        self, base: ValueLike, offset: Union[int, CaptureKey, None] = None
    ) -> None:
        p = _ext.pattern
        #: The offset capture, `None` for a known offset.
        self.capture: Optional[CaptureKey] = None
        self._known: Optional[int] = None
        if isinstance(offset, int):
            self._known = offset
            self.pattern: ValueLike = base if offset == 0 else p.int_add(base, p.int_const(offset))
            return
        cap = p.Capture() if offset is None else offset
        self.capture = cap
        const = p.int_const().capture(cap) if isinstance(cap, str) else p.int_const(cap)
        # Ordered: a permissive `base` also matches the `Add`, and a union
        # would report each field twice.
        self.pattern = p.first_of([p.int_add(base, const), base])

    def offset(self, m: Match) -> int:
        """The field offset `m` matched, signed: a frame slot or a
        `container_of` field sits below its base."""
        if self._known is not None:
            return self._known
        assert self.capture is not None
        return m.sint(self.capture) if m.has(self.capture) else 0

    def load(self) -> LoadPat:
        """A `Load` of the field."""
        return _ext.pattern.load(addr=self.pattern)

    def store(self, data: Optional[ValueLike] = None) -> StorePat:
        """A `Store` into the field, of `data` when given."""
        return _ext.pattern.store(addr=self.pattern, data=data)


def field(base: ValueLike, offset: Union[int, CaptureKey, None] = None) -> FieldPat:
    """The field at `base + offset`. `offset` is an `int` to require that
    offset, a `Capture` or capture name to bind it, or `None` for a fresh
    capture."""
    return FieldPat(base, offset)


def code_ptr(target: ValueLike) -> Pat:
    """`target`, or `target & -2` (an ARM interworking or MIPS16 branch
    clearing the ISA-mode bit)."""
    p = _ext.pattern
    return p.first_of([p.int_and(target, p.int_const(-2)), target])
