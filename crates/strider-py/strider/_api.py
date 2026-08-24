"""High-level Python facade: the ELF loader and the `ElfLifter` it returns.

```python
import strider
lift = strider.lift.load_elf("path/to/file.elf")
cfg, function, unresolved = lift.analyze("function_name")
matches = function.find_all(strider.pattern.call())
```

For non-ELF or custom sources, build a `Lifter` directly with
`strider.lift.lifter(arch, mem, rom=None)`.
"""

from __future__ import annotations

import os
import struct
from typing import TYPE_CHECKING, Iterator, Mapping, Optional, Union

# Resolve the extension submodule by its full dotted path so `_ext` is
# unambiguous regardless of what `__init__.py` binds into the package
# namespace.
import importlib as _importlib

_ext = _importlib.import_module("strider._strider")

if TYPE_CHECKING:
    # `_ext` is an opaque module to a checker, so its attributes come back as
    # `Any`; the stub imports below are the same classes with their real types.
    from .cfg import CfgOptions
    from .lift import Lifter, LifterOptions
    from .reader import BufferReader, MemLike, RomLike, Symbol
    from .sleigh import CallingConvention, SleighArch
else:
    CallingConvention = _ext.sleigh.CallingConvention
    SleighArch = _ext.sleigh.SleighArch
    CfgOptions = _ext.cfg.CfgOptions
    Lifter = _ext.lift.Lifter
    LifterOptions = _ext.lift.LifterOptions

#: What the loaders accept as a filesystem path: a `str`, a `pathlib.Path`,
#: or any object implementing `__fspath__`.
StrPath = Union[str, "os.PathLike[str]"]


# e_machine codes per the System V ABI gABI and each architecture supplement.
_EM_386 = 3
_EM_MIPS = 8
_EM_PPC = 20
_EM_PPC64 = 21
_EM_ARM = 40
_EM_X86_64 = 62
_EM_AARCH64 = 183


class _ElfHeader:
    """Minimal ELF header reader: enough to pick the right `SleighArch` and
    `CallingConvention` preset."""

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
        endian_fmt = "<H" if self.is_little_endian else ">H"
        (self.e_machine,) = struct.unpack(endian_fmt, header[18:20])


def _arch_and_cc_for_elf(
    header: _ElfHeader,
) -> tuple[SleighArch, CallingConvention]:
    """Pick the `SleighArch` and userland `CallingConvention` preset matching
    this ELF. Raises `ValueError` for an unsupported e_machine.

    ARM defaults to little-endian `arm` (big-endian `arm_be`); a Thumb image
    is loaded by passing an explicit `arch=SleighArch.arm_thumb()`, since the
    ELF header does not distinguish a Thumb build from an ARM one.
    """
    em = header.e_machine
    le = header.is_little_endian
    is64 = header.is_64bit

    if em == _EM_386:
        return SleighArch.x86(), CallingConvention.x86_cdecl()
    if em == _EM_X86_64:
        return SleighArch.x86_64(), CallingConvention.x86_64_systemv()
    if em == _EM_ARM:
        arch = SleighArch.arm() if le else SleighArch.arm_be()
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
        # ELFv2 is the modern Linux PPC64 ABI on both byte orders; ELFv1 is
        # the legacy big-endian-only one. Prefer v2; a v1-only distro can
        # override with an explicit `arch=` / `cc=`.
        arch = SleighArch.ppc64le() if le else SleighArch.ppc64be()
        return arch, CallingConvention.powerpc64_elf_v2()
    raise ValueError(
        f"unsupported ELF e_machine={em}; pass an explicit SleighArch + "
        f"CallingConvention via strider.lift.load_elf(path, arch=..., cc=...) instead"
    )


def _load_elf_with(
    loader,
    path: StrPath,
    *,
    apply_relocations: bool,
    arch: Optional[SleighArch],
    cc: Optional[CallingConvention],
) -> "ElfLifter":
    """Shared body of `load_elf`'s two region-collection strategies.

    `os.fspath` converts a `pathlib.Path` (or any `__fspath__` object) here,
    so a non-path argument fails as a clear `TypeError` rather than a
    confusing `FileNotFoundError` further down.
    """
    path = os.fspath(path)
    if not os.path.exists(path):
        raise FileNotFoundError(path)
    header = _ElfHeader(path)
    # Only run the (possibly raising) detector when arch or cc is missing.
    if arch is None or cc is None:
        det_arch, det_cc = _arch_and_cc_for_elf(header)
        arch = arch if arch is not None else det_arch
        cc = cc if cc is not None else det_cc
    elf = loader(path, apply_relocations)
    return ElfLifter(elf, arch, cc, elf.reader(), rom=elf.ro_reader())


def load_elf(
    path: StrPath,
    *,
    from_segments: bool = True,
    apply_relocations: bool = True,
    arch: Optional[SleighArch] = None,
    cc: Optional[CallingConvention] = None,
) -> "ElfLifter":
    """Load an ELF binary, picking the architecture and calling convention
    from its header, and return an `ElfLifter`.

    `from_segments=True` (the default) walks PT_LOAD program headers, falling
    back to sections for relocatable objects; `from_segments=False` forces
    the section walk for section-granular regions. Override the detected
    `arch=` / `cc=` for kernel or custom-ABI workflows. `apply_relocations`
    (default `True`) applies relocations, which shared objects and PIE
    binaries need.

    Raises `FileNotFoundError` when `path` does not exist, and `ValueError`
    when the file is not an ELF or its architecture is unsupported.
    """
    loader = _ext._load_elf_from_segments if from_segments else _ext._load_elf_from_sections
    return _load_elf_with(
        loader, path, apply_relocations=apply_relocations, arch=arch, cc=cc
    )


class ElfLifter(Lifter):
    """A loaded ELF binary as a `Lifter`, returned by
    `strider.lift.load_elf(path)`.

    It carries the ELF's memory as both code reader and read-only image, plus
    the symbol table (`symbol`, `symbol_at`, `symbols`, `entry_point`,
    `functions`) and an `analyze(target)` that accepts a symbol name. Analyse
    many functions by calling `analyze` repeatedly:

    ```python
    lift = strider.lift.load_elf("path/to/file.elf")
    for sym in lift.functions():
        cfg, function, unresolved = lift.analyze(sym.address)
    ```
    """

    __slots__ = ("_elf", "_arch", "_cc")

    def __new__(
        cls,
        elf,  # the internal loaded-ELF handle the loader returns
        arch: SleighArch,
        cc: CallingConvention,
        mem: "MemLike",
        rom: Optional["RomLike"] = None,
    ) -> "ElfLifter":
        """Build the base `Lifter`, which takes only `(arch, mem, rom=...)`;
        the ELF-specific state is attached in `__init__`."""
        return super().__new__(cls, arch, mem, rom=rom)

    def __init__(
        self,
        elf,
        arch: SleighArch,
        cc: CallingConvention,
        mem: "MemLike",
        rom: Optional["RomLike"] = None,
    ) -> None:
        """Attach the ELF backend, architecture and default calling
        convention; `mem` and `rom` were already consumed by `__new__`."""
        del mem, rom
        self._elf = elf
        self._arch = arch
        # Every base `Lifter.analyze` takes its own convention; this is the
        # ELF-derived default.
        self._cc = cc

    @property
    def arch(self) -> SleighArch:
        """The architecture this binary was loaded as."""
        return self._arch

    @property
    def cc(self) -> CallingConvention:
        """The calling convention `analyze` uses when none is passed."""
        return self._cc

    @property
    def endianness(self) -> str:
        """Byte order of the loaded binary: `"little"` or `"big"`."""
        return self._arch.endianness()

    @property
    def is_arm_be8(self) -> bool:
        """Whether the binary sets ARM's `EF_ARM_BE8`: instructions stored
        little-endian, data big-endian. `EI_DATA` marks BE8 and BE32 images
        alike, so this is what chooses `arm_be_kernel` over `arm_be`. `False`
        off ARM."""
        return self._elf.is_arm_be8

    def __repr__(self) -> str:
        return f"ElfLifter(arch={self._arch.name()}, cc={self._cc.name()})"

    def functions(self) -> Iterator["Symbol"]:
        """The function symbols in address order, one per address: aliases of
        an address already listed are excluded, preferring the one whose size
        the ELF records. A function with no recorded size is still yielded,
        with `Symbol.size` `None`."""
        return iter(self._elf.functions())

    def iter_symbols(self) -> Iterator["Symbol"]:
        """Every symbol pulled one at a time, so the `Symbol` objects are
        never all live at once. The Rust table is built up front."""
        return iter(self._elf.iter_symbols())

    def symbol(self, name: str) -> "Symbol":
        """The `Symbol` named `name`. Raises `StriderError` when it is not
        defined in any loaded ELF."""
        return self._elf.symbol(name)

    def symbol_opt(self, name: str) -> Optional["Symbol"]:
        """`symbol`, but `None` rather than raising when `name` is
        undefined."""
        return self._elf.symbol_opt(name)

    def symbol_at(self, address: int) -> Optional["Symbol"]:
        """The symbol covering `address`: the nearest one at or below it whose
        recorded extent reaches `address`. A symbol with no recorded size
        covers only its own address, and aliases sharing an address resolve to
        the code symbol among them. `None` when nothing covers `address`."""
        return self._elf.symbol_at(address)

    def symbols(self) -> Mapping[str, "Symbol"]:
        """Every symbol, keyed by the name it resolves under."""
        return self._elf.symbols()

    def entry_point(self) -> int:
        """The entry-point address of the first loaded ELF."""
        return self._elf.entry_point()

    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` raw bytes at `addr` from the loaded regions, or
        `None` when `addr` is unmapped."""
        return self._elf.read(addr, size)

    def reader(self) -> "BufferReader":
        """The `BufferReader` over the regions `load_elf` mapped (PT_LOAD
        segments unless `from_segments=False`)."""
        return self._elf.reader()

    def add_elf(self, path: str, *, apply_relocations: bool = False) -> None:
        """Merge another ELF, such as a shared library, into this handle.
        The earlier-loaded ELF wins on name collisions.

        Raises `StriderError` if the new ELF maps code over an address already
        loaded with different bytes: `add_elf` places shared objects at distinct
        addresses, so two ELFs linked at the same base cannot be merged."""
        self._elf.add_elf(path, apply_relocations)
        self._rebuild(self._arch, self._elf.reader(), rom=self._elf.ro_reader())

    def analyze(
        self,
        entry: Union[str, int],
        cc: Optional[CallingConvention] = None,
        opts: Optional[LifterOptions] = None,
    ):
        """Lift the function at `entry`, a symbol name or an address,
        returning the same `AnalyzeResult` as `Lifter.analyze`.

        A `str` entry is resolved through the ELF symbol table, and its
        recorded size bounds the lift unless `opts.cfg.function_max_size` is
        set. `cc` defaults to this handle's convention.

        Raises `TypeError` when `entry` is neither `str` nor `int`.
        """
        target = entry
        if opts is None:
            opts = LifterOptions()

        if isinstance(target, str):
            sym = self._elf.symbol(target)
            addr = sym.address
            # An unrecorded symbol size leaves the lift unbounded.
            if opts.cfg.function_max_size is None and sym.size is not None:
                # Every other field is carried over by hand: a rebuilt
                # `CfgOptions` silently drops whatever this call forgets.
                opts = opts.with_cfg(
                    CfgOptions(
                        function_max_size=sym.size,
                        allow_code_before_start_addr=opts.cfg.allow_code_before_start_addr,
                        known_targets=opts.cfg.known_targets,
                        call_other_abis=opts.cfg.call_other_abis,
                    )
                )
        elif isinstance(target, int):
            addr = target
        else:
            raise TypeError(
                f"`entry` must be a symbol name (str) or address (int), "
                f"got {type(target).__name__}"
            )

        # The raw address passes through: `Lifter::build_cfg` reads its low bit
        # per function and masks `decode_addr` itself.
        if cc is None:
            cc = self._cc
        return super().analyze(addr, cc, opts)
