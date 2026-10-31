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
import stat as _stat
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
    from .cfg import Cfg, CfgOptions
    from .lift import Lifter, LifterOptions
    from .pattern import CaptureKey, LoadPat, Match, Pat, StorePat, ValueLike
    from .reader import BufferReader, MemLike, RomLike, Symbol
    from .sleigh import CallingConvention, SleighArch
else:
    CallingConvention = _ext.sleigh.CallingConvention
    SleighArch = _ext.sleigh.SleighArch
    Lifter = _ext.lift.Lifter
    LifterOptions = _ext.lift.LifterOptions
    CfgOptions = _ext.cfg.CfgOptions

StriderError = _ext.StriderError

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

# ARM `e_flags` float-ABI bit for a soft or softfp image.
_EF_ARM_ABI_FLOAT_SOFT = 0x0000_0200

# MIPS `e_flags`: `EF_MIPS_ABI2` marks n32, and the `EF_MIPS_ABI` field names
# O32 (0x1000, or 0 on an n32/n64 image) or one of these three.
_EF_MIPS_ABI2 = 0x0000_0020
_EF_MIPS_ABI = 0x0000_F000
_EF_MIPS_ABI_NAMES = {0x2000: "O64", 0x3000: "EABI32", 0x4000: "EABI64"}
# `EF_MIPS_ARCH_ASE_MICROMIPS`: the image's code is microMIPS.
_EF_MIPS_MICROMIPS = 0x0200_0000

# PowerPC `KERNELBASE`: Linux links its own text at 0xc000_0000 (ppc32) or
# 0xc000_0000_0000_0000 (ppc64), which is `TASK_SIZE` on ppc32 and far above
# the user address space on ppc64, so no userland image's entry point can
# reach it. The kernel is built `-mno-altivec -mno-vsx`, and its convention
# names no vector register.
_PPC32_KERNELBASE = 0xC000_0000
_PPC64_KERNELBASE = 0xC000_0000_0000_0000


class _ElfHeader:
    """Minimal ELF header reader: enough to pick the right `SleighArch` and
    `CallingConvention` preset."""

    __slots__ = ("is_64bit", "is_little_endian", "e_machine", "e_entry", "e_flags")

    def __init__(self, path: str) -> None:
        # Checked before opening: opening a FIFO read-only BLOCKS until a
        # writer appears, so `load_elf` on one never returns. `os.stat`
        # follows symlinks and does not open, so a link to a regular file
        # still passes and a pipe cannot stall the check itself.
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
        endian_fmt = "<H" if self.is_little_endian else ">H"
        (self.e_machine,) = struct.unpack(endian_fmt, header[18:20])
        word_fmt = "<I" if self.is_little_endian else ">I"
        # `e_entry` is the first of the three addresses, at offset 24 on both
        # classes; 0 on a relocatable object, which has no entry point.
        addr_fmt = ("<" if self.is_little_endian else ">") + ("Q" if self.is_64bit else "I")
        size = 8 if self.is_64bit else 4
        (self.e_entry,) = (
            struct.unpack(addr_fmt, header[24 : 24 + size])
            if len(header) >= 24 + size
            else (0,)
        )
        # `e_flags` follows the three addresses: offset 36 on ELF32, 48 on
        # ELF64. PowerPC64 carries its ABI level in the low two bits.
        off = 48 if self.is_64bit else 36
        self.e_flags = (
            struct.unpack(word_fmt, header[off : off + 4])[0]
            if len(header) >= off + 4
            else 0
        )


def _refuse_micromips(header: _ElfHeader, remedy: str) -> None:
    """Every MIPS preset decodes an ISA-bit address as MIPS16e, so a
    microMIPS image's functions would lift wrong."""
    if header.e_machine == _EM_MIPS and header.e_flags & _EF_MIPS_MICROMIPS:
        raise ValueError(
            f"unsupported microMIPS image: strider has no microMIPS preset; {remedy}"
        )


def _arch_and_cc_for_elf(
    header: _ElfHeader,
    is_arm_be8: bool = False,
) -> tuple[SleighArch, CallingConvention]:
    """Pick the `SleighArch` and `CallingConvention` preset matching this ELF.
    Raises `ValueError` for an unsupported e_machine, for an ELF32 MIPS
    ABI other than O32, and for a microMIPS image.

    The convention is the userland one on every architecture but PowerPC,
    where an `e_entry` at or above `KERNELBASE` selects the kernel flavour:
    kernel code is built `-mno-altivec -mno-vsx`, so naming the AltiVec
    windows there would track twelve 16-byte varnodes on every function for
    nothing. x86's kernel ABI (`x86_linux_kernel`, `-mregparm=3`) has no
    header bit to detect and stays an explicit `cc=`.

    ARM defaults to little-endian `arm`; a big-endian image is
    `arm_be_kernel` when `is_arm_be8` and `arm_be` otherwise. Its convention
    follows the `e_flags` float-ABI bit: `arm_aapcs_soft` for a soft/softfp
    image, `arm_aapcs` otherwise. A Thumb image is loaded by passing an
    explicit `arch=SleighArch.arm_thumb()`, since the ELF header does not
    distinguish a Thumb build from an ARM one.
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
            # BE8 stores instructions little-endian, so `arm_be` decodes every
            # one of them byte-swapped. `EI_DATA` marks both images alike.
            arch = SleighArch.arm_be_kernel()
        else:
            arch = SleighArch.arm_be()
        # EABI float ABI, `e_flags` bits 9/10. A soft or softfp image passes
        # floats in r0-r3, so hard-float's d0-d7 would be phantom carriers.
        # Only a linked image carries either bit: a relocatable object records
        # its float ABI in `.ARM.attributes` alone, whatever it was built with,
        # and falls to hard-float here.
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
        # O32 is the only ELF32 MIPS ABI strider has a preset for. n32 needs a
        # 64-bit decoder with 32-bit addresses, arguments in $4-$11 and 64-bit
        # returns; EABI and O64 have register sets of their own. Lifting any of
        # them as O32 loses the arguments past the fourth.
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
        # The ABI level is the low two bits of `e_flags`. The two differ in
        # linkage-area size, so the wrong one reads stack arguments 16 bytes
        # off and collects none. Little-endian PPC64 is ELFv2 only; a
        # big-endian image predating the flag (0) is ELFv1.
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
    """Load an ELF binary, picking the architecture and calling convention
    from its header, and return an `ElfLifter`.

    `from_segments=True` (the default) walks PT_LOAD program headers, falling
    back to sections for relocatable objects; `from_segments=False` forces
    the section walk for section-granular regions. Override the detected
    `arch=` / `cc=` for kernel or custom-ABI workflows. `arch=` alone keeps
    the header's convention, and `analyze` raises `StriderError` when that
    convention's stack geometry is another arch's.

    `apply_relocations` (default `True`) applies relocations, which shared
    objects and PIE binaries need; it also selects what is mapped, so `False`
    drops writable non-executable sections rather than serving their on-disk
    bytes. A writable-and-executable mapping is kept either way. With it off,
    every relocation field still mapped (in code, a `.rodata` table, a TEXTREL
    image's instructions) serves its file bytes: the addend or the link-time
    value, not the relocated one. With it on, a linked image is relocated from
    the tables its dynamic tags place, as the loader reads them, Android
    packed ones included, whatever its section headers say. An image whose
    relocation tables name more than twice the entries the file holds at their
    smallest entry raises `StriderError`, and so does one whose code
    (`DT_TEXTREL`, or a writable-and-executable mapping) a table the tags
    place but strider cannot read or decode may relocate. Any other image with
    such a table serves none of its writable mappings.

    `relocated_got_is_rom` (default `False`) additionally serves `.got` and
    `.data.rel.ro` as READ-ONLY memory, so a load through a GOT slot folds to
    the address the slot holds. It is what resolves a PIC jump table whose
    base comes out of the GOT.

    An image with a table its dynamic tags place that strider cannot read or
    decode serves neither.

    A slot whose load-time value only the dynamic loader computes stays
    unmapped, and a load through it does not fold: one under a relocation
    strider does not compute, such as one against an imported symbol the
    image gives no address; an IFUNC's (`R_*_IRELATIVE`, or a relocation
    against a defined `STT_GNU_IFUNC` symbol), which holds what the resolver
    returns; every byte of an `R_*_COPY` object, which the loader copies from
    the library defining it; the two GOT header words ARM and secure-PLT
    PPC32 lazy binding writes (`DT_PLTGOT + 4` / `+ 8`, `DT_PPC_GOT + 4` /
    `+ 8`); and on MIPS, whose primary GOT no relocation covers, the
    lazy-resolver word, the module-pointer word when its top bit asks for
    one, and every global entry except those for symbols the image defines
    that already hold their address. `.got.plt` is never served, lazy binding
    rewriting those slots on the first call through each one.

    Inside an executable mapping a relocation against a defined IFUNC takes
    the symbol's own address instead, so the instruction still decodes. An
    address taken in code (`lea f(%rip)`, `mov $f`), or a data word in an
    executable mapping (FreeBSD 12's `.rodata`), then folds to the symbol,
    where at run time it holds what the resolver returns.

    The rest is an unchecked claim about the runtime: that nothing rewrites a
    slot after the loader has run, and that a symbol the image defines binds
    to that definition. Where either is false (a program writing its own GOT;
    a copy relocation, `LD_PRELOAD` or a `dlopen`ed library interposing the
    symbol) the fold is wrong too. It contradicts `apply_relocations=False`,
    which says the loader has NOT run, and the pair raises `StriderError`.

    Raises `FileNotFoundError` when `path` does not exist, and `ValueError`
    when the file is not an ELF, or its architecture or ABI is unsupported,
    a microMIPS image among them. Passing both `arch=` and `cc=` skips the
    detector, and with it the ABI and microMIPS refusals.
    """
    # `os.fspath` makes a non-path argument a clear `TypeError` rather than a
    # confusing `FileNotFoundError` further down.
    path = os.fspath(path)
    if not os.path.exists(path):
        raise FileNotFoundError(path)
    header = _ElfHeader(path)
    loader = _ext._load_elf_from_segments if from_segments else _ext._load_elf_from_sections
    elf = loader(path, apply_relocations, relocated_got_is_rom)
    # Only run the (possibly raising) detector when arch or cc is missing.
    if arch is None or cc is None:
        det_arch, det_cc = _arch_and_cc_for_elf(header, elf.is_arm_be8)
        arch = arch if arch is not None else det_arch
        cc = cc if cc is not None else det_cc
    return ElfLifter(elf, arch, cc, elf.reader(), rom=elf.ro_reader())


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

    __slots__ = ("_elf", "_cc")

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
        """Attach the ELF backend and default calling convention; `arch`,
        `mem` and `rom` were already consumed by `__new__`."""
        del arch, mem, rom
        self._elf = elf
        # Every base `Lifter.analyze` takes its own convention; this is the
        # ELF-derived default.
        self._cc = cc

    @property
    def cc(self) -> CallingConvention:
        """The calling convention `analyze` uses when none is passed."""
        return self._cc

    @property
    def endianness(self) -> str:
        """Byte order of the loaded binary, as the first ELF's `EI_DATA`
        gives it: `"little"` or `"big"`. Read off the ELF, so it answers while
        a call holds the handle."""
        return "little" if self._elf.is_little_endian else "big"

    @property
    def is_arm_be8(self) -> bool:
        """Whether the binary sets ARM's `EF_ARM_BE8`: instructions stored
        little-endian, data big-endian. `EI_DATA` marks BE8 and BE32 images
        alike, so this is what `load_elf` picks `arm_be_kernel` over `arm_be`
        on. `False` off ARM."""
        return self._elf.is_arm_be8

    def __repr__(self) -> str:
        try:
            arch = self.arch.name()
        except StriderError:
            # Another call holds the handle; a repr never raises.
            return f"ElfLifter(cc={self._cc.name()})"
        return f"ElfLifter(arch={arch}, cc={self._cc.name()})"

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
        """The `Symbol` named `name`: its definition in the first ELF that
        defines it, else an undefined symbol that still has an address (a PLT
        stub, or an object file's extern). Raises `StriderError` when there is
        neither."""
        return self._elf.symbol(name)

    def symbol_opt(self, name: str) -> Optional["Symbol"]:
        """`symbol`, but `None` rather than raising when `name` is
        undefined."""
        return self._elf.symbol_opt(name)

    def symbol_at(self, address: int) -> Optional["Symbol"]:
        """The symbol covering `address`: the nearest one at or below it whose
        recorded extent reaches `address`. A symbol with no recorded size
        covers only its own address, and aliases sharing an address are ranked
        by having a recorded extent first, then by being code, so a sized data
        alias wins over an unsized function one. `None` when nothing covers
        `address`."""
        return self._elf.symbol_at(address)

    def symbols(self) -> Mapping[str, "Symbol"]:
        """Every symbol, keyed by the name it resolves under."""
        return self._elf.symbols()

    def entry_point(self) -> int:
        """The entry-point address of the first loaded ELF."""
        return self._elf.entry_point()

    def read(self, addr: int, size: int) -> Optional[bytes]:
        """Up to `size` raw bytes at `addr` from the loaded regions, or `None`
        when `addr` is unmapped or inside a relocation this loader does not
        compute. In writable, non-executable data such a field is not cut out
        but serves the file's bytes, which the loader overwrites. `rom()`
        serves no writable data, except `.got` / `.data.rel.ro` under
        `relocated_got_is_rom`, where such a field is unmapped."""
        return self._elf.read(addr, size)

    def add_elf(
        self,
        path: StrPath,
        *,
        apply_relocations: bool = True,
        relocated_got_is_rom: bool = False,
    ) -> None:
        """Merge another ELF, such as a shared library, into this handle.
        The earlier-loaded ELF wins on name collisions.

        `apply_relocations` defaults to `True`, matching `load_elf`: a merged
        ELF is normally the ET_DYN case relocations exist for, and the flag
        also selects what is mapped, so `False` drops writable non-executable
        sections rather than serving their on-disk bytes, and serves every
        relocation field still mapped as its file bytes, as in `load_elf`.
        With it on, an image whose relocation section headers name more than
        twice the entries the file holds at its smallest entry, or whose code
        a dynamic relocation table strider cannot read or decode may relocate,
        raises `StriderError`, as in `load_elf`.

        `relocated_got_is_rom` is `load_elf`'s unchecked claim, made for THIS
        image only: it does not inherit the handle's, since one shared
        object's link says nothing about another's. It contradicts
        `apply_relocations=False` here too, and the pair raises `StriderError`.

        Raises `StriderError` if the new ELF's architecture, class or byte
        order, or on ARM its `EF_ARM_BE8`, differs from the first image's: one
        handle decodes one target. Raises it too if the new ELF maps code over
        an address already loaded with different bytes: `add_elf` places shared
        objects at distinct addresses, so two ELFs linked at the same base
        cannot be merged.
        Raises `ValueError` for a microMIPS image, which no preset decodes."""
        path = os.fspath(path)
        try:
            header = _ElfHeader(path)
        except (OSError, ValueError):
            # Not a readable ELF: the merge reports it.
            header = None
        if header is not None:
            _refuse_micromips(header, "add_elf cannot merge it")
        # One call, so a handle in use refuses before anything is merged
        # rather than leaving `symbol` and `read` seeing the new ELF and the
        # decoder not.
        self._merge_elf(self._elf, path, apply_relocations, relocated_got_is_rom)

    def add_symbol_file(self, path: StrPath) -> None:
        """Take the symbols of `path` and none of its bytes.

        This is how a separate debug or symbol file attaches after loading: a
        `objcopy --only-keep-debug` companion and distro debuginfo are linked
        at the same addresses as the image they describe, so `add_elf` would
        refuse them as an overlap. Both `.symtab` and `.dynsym` are read, as
        for any ELF, and the already-loaded ELFs keep a colliding name.

        The mapped bytes are untouched, so nothing about lifting changes; only
        `symbol`, `symbol_at`, `symbols`, `functions` and `iter_symbols` see
        more."""
        self._elf.add_symbol_file(os.fspath(path))

    def add_symbols(
        self,
        symbols: "dict[str, Union[int, tuple[int, int]]]",
        *,
        is_function: bool = True,
    ) -> None:
        """Add symbols directly, for names that live in no ELF: a map file, a
        kernel `System.map`, a database, or your own naming.

        Each value is an address, or an `(address, size)` pair when the extent
        is known, which is what lets `symbol_at` resolve an interior address.
        An ELF already carrying a name keeps its own answer for that name."""
        self._elf.add_symbols(symbols, is_function=is_function)

    def build_cfg(self, entry: int, opts: Optional[CfgOptions] = None) -> Cfg:
        """`Lifter.build_cfg`, with the options `analyze` derives from the
        ELF: its `$d` mapping symbols as `data_ranges` unless `opts` names
        some, and the local entry of every `no_return_targets` address that
        is a ppc64 ELFv2 symbol's `address`. `entry` is an address, and no
        symbol size bounds the build, as for `analyze(int)`."""
        return self._build_cfg_elf(self._elf, entry, opts)

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
        set. Unless `opts.cfg.data_ranges` names some, the ELF's `$d` mapping
        symbols supply them. A `no_return_targets` address or
        `per_address_ccs` key that is a ppc64 ELFv2 symbol's `address` also
        covers its `Symbol.local_entry`. `cc` defaults to this handle's
        convention.

        Raises `TypeError` when `entry` is neither `str` nor `int`, and
        `StriderError` when the binary changed on disk since it was loaded.

        Releases the GIL as `Lifter.analyze` does, and a daemon thread inside
        it at interpreter finalization aborts the process as it does there.
        """
        return self._analyze_elf(self._elf, entry, self._cc if cc is None else cc, opts)


class FieldPat:
    """A struct field at `base + offset`. `ConstantFold` rewrites `base + 0`
    to `base`, so a field at offset 0 lifts with no `Add`; `pattern` matches
    both forms and `offset(m)` reads 0 back for the bare one."""

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
        # Ordered, not a union: `base` matches the `Add` too whenever it is
        # permissive, and a union then reports every field twice.
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
    """`target`, or `target & -2`: an ARM interworking or MIPS16 branch clears
    the ISA-mode bit, the one mask strider strips when resolving a target."""
    p = _ext.pattern
    return p.first_of([p.int_and(target, p.int_const(-2)), target])
