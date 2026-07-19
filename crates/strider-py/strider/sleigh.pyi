"""Type stubs for strider.sleigh: architecture, calling convention, and the
low-level disassembler and varnode surface."""

from __future__ import annotations

from typing import Any, Optional

class SleighArch:
    """A target architecture: instruction-set specification plus byte order.

    Use one of the presets below, or let `strider.lift.load_elf` pick one from
    the ELF header.
    """
    @classmethod
    def x86_64(cls) -> SleighArch:
        """64-bit x86, little-endian."""
        ...
    @classmethod
    def x86(cls) -> SleighArch:
        """32-bit x86, little-endian."""
        ...
    @classmethod
    def mipsbe32(cls) -> SleighArch:
        """32-bit MIPS, big-endian."""
        ...
    @classmethod
    def mipsle32(cls) -> SleighArch:
        """32-bit MIPS, little-endian."""
        ...
    @classmethod
    def mipsbe64(cls) -> SleighArch:
        """64-bit MIPS, big-endian."""
        ...
    @classmethod
    def mipsle64(cls) -> SleighArch:
        """64-bit MIPS, little-endian."""
        ...
    @classmethod
    def arm(cls) -> SleighArch:
        """32-bit ARM in ARM mode, little-endian."""
        ...
    @classmethod
    def arm_be(cls) -> SleighArch:
        """32-bit ARM in ARM mode, big-endian."""
        ...
    @classmethod
    def arm_be_kernel(cls) -> SleighArch:
        """Big-endian 32-bit ARM as built for kernel code."""
        ...
    @classmethod
    def arm_thumb(cls) -> SleighArch:
        """32-bit ARM in Thumb mode, little-endian. There is no big-endian
        Thumb preset."""
        ...
    @classmethod
    def aarch64(cls) -> SleighArch:
        """64-bit ARM, little-endian."""
        ...
    @classmethod
    def aarch64be(cls) -> SleighArch:
        """64-bit ARM, big-endian."""
        ...
    @classmethod
    def ppc32be(cls) -> SleighArch:
        """32-bit PowerPC, big-endian."""
        ...
    @classmethod
    def ppc32le(cls) -> SleighArch:
        """32-bit PowerPC, little-endian."""
        ...
    @classmethod
    def ppc64be(cls) -> SleighArch:
        """64-bit PowerPC, big-endian."""
        ...
    @classmethod
    def ppc64le(cls) -> SleighArch:
        """64-bit PowerPC, little-endian."""
        ...
    def name(self) -> str:
        """The preset's short name, e.g. `"x86_64"` or `"arm_thumb"`."""
        ...

class CallingConvention:
    """How a function receives arguments, returns values, and which
    registers it may clobber.

    Every `analyze` call takes one. Use a preset, `custom` for an unusual ABI,
    or `no_return` to mark a callee that never returns.
    """
    @classmethod
    def x86_64_systemv(cls) -> CallingConvention:
        """The System V AMD64 ABI used by Linux userland on x86-64."""
        ...
    @classmethod
    def aarch64_aapcs64(cls) -> CallingConvention:
        """The AAPCS64 ABI for 64-bit ARM."""
        ...
    @classmethod
    def arm_aapcs(cls) -> CallingConvention:
        """The AAPCS ABI for 32-bit ARM."""
        ...
    @classmethod
    def mips_o32(cls) -> CallingConvention:
        """The MIPS O32 ABI (32-bit)."""
        ...
    @classmethod
    def mips_n64(cls) -> CallingConvention:
        """The MIPS N64 ABI (64-bit)."""
        ...
    @classmethod
    def powerpc_sysv32(cls) -> CallingConvention:
        """The System V ABI for 32-bit PowerPC."""
        ...
    @classmethod
    def powerpc64_elf_v1(cls) -> CallingConvention:
        """The legacy big-endian-only PowerPC64 ELFv1 ABI."""
        ...
    @classmethod
    def powerpc64_elf_v2(cls) -> CallingConvention:
        """The PowerPC64 ELFv2 ABI, used by modern Linux on both byte
        orders."""
        ...
    @classmethod
    def x86_cdecl(cls) -> CallingConvention:
        """The 32-bit x86 cdecl convention (all arguments on the stack)."""
        ...
    @classmethod
    def x86_64_all_preserving(cls) -> CallingConvention:
        """x86-64 with every register listed callee-saved. Use it as a
        per-address override for transparent sites like `__fentry__` or
        `mcount`."""
        ...
    @classmethod
    def x86_linux_kernel(cls) -> CallingConvention:
        """32-bit x86 Linux kernel-internal convention (`-mregparm=3`). This
        is the only architecture whose kernel ABI differs from its userland
        preset."""
        ...
    @classmethod
    def custom(
        cls,
        sleigh: "Sleigh",
        arg_passing_regs: list[str],
        callee_saved_regs: list[str],
        ret_val_regs: list[str],
        ret_val_regs_float: list[str],
        stack_pointer: str,
        stack_arg_base: int | None,
        stack_arg_increment: int,
        ret_stack_pop: int,
        link_register: Optional[str] = ...,
        preserves_memory: bool = ...,
    ) -> CallingConvention:
        """Build a convention from register names resolved against `sleigh`.

        Registers are named as the architecture spells them. Stack arguments
        start `stack_arg_base` bytes from the call-time stack pointer and
        advance by `stack_arg_increment`; `ret_stack_pop` is how many bytes
        the callee pops on return. `preserves_memory=True` declares that a
        call through this convention cannot write memory.
        """
        ...
    def no_return(self) -> CallingConvention:
        """A copy of this convention marked as never returning, for callees
        like `exit`, `abort`, or `__stack_chk_fail`.

        Use it as a per-address override, e.g.
        `CallingConvention.x86_64_systemv().no_return()`. The calling region
        terminates at such a call, so the unreachable fall-through is not
        lifted as live code. That matters mid-function too, e.g.
        `if (err) panic();` followed by real code.
        """
        ...
    def name(self) -> str:
        """The preset's short name, e.g. `"x86_64_systemv"`."""
        ...

class VnSpace:
    """One of the disassembler's built-in address spaces."""
    RAM: VnSpace
    REGISTER: VnSpace
    CONST: VnSpace
    UNIQUE: VnSpace
    def name(self) -> str:
        """The space's name, e.g. `"ram"` or `"register"`."""
        ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Vn:
    """A varnode: an address space, an offset, and a size in bytes.

    Registers, memory locations, constants and lifter temporaries are all
    varnodes.
    """
    def __init__(self, space: VnSpace, off: int, size: int) -> None:
        """Build the varnode at `off` in `space`, `size` bytes wide."""
        ...
    @property
    def space(self) -> VnSpace:
        """The address space this varnode lives in."""
        ...
    @property
    def off(self) -> int:
        """The offset within the space."""
        ...
    @property
    def size(self) -> int:
        """The width in bytes."""
        ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Sleigh:
    """The instruction decoder for one architecture, over one byte source.

    Most workflows never build one directly: `Lifter` owns a `Sleigh` and
    forwards `reg` / `reg_name`.
    """
    def __init__(self, arch: SleighArch, mem: Any) -> None:
        """Build a decoder for `arch` reading bytes from `mem` (a
        `BufferReader` or `MemReader`)."""
        ...
    def arch_name(self) -> str:
        """The architecture's short name."""
        ...
    def reg(self, name: str) -> Optional[Vn]:
        """The varnode for the register called `name`, or `None` when the
        name is not a register."""
        ...
    def reg_name(self, vn: Vn) -> Optional[str]:
        """The register name for `vn`, or `None` when it names no register
        (a non-register space, or an offset and size absent from this
        architecture's table)."""
        ...
