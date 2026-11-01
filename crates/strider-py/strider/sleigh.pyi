"""Type stubs for strider.sleigh: architecture, calling convention, user-op
ABI and varnode surface."""

from __future__ import annotations

from typing import Optional

__all__: list[str]

#: Every architecture preset, the names `SleighArch`'s classmethods carry.
ARCH_PRESETS: tuple[str, ...]

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
        """32-bit MIPS, big-endian. An odd (ISA-bit) address decodes as
        MIPS16e; no preset decodes microMIPS."""
        ...
    @classmethod
    def mipsle32(cls) -> SleighArch:
        """32-bit MIPS, little-endian. An odd (ISA-bit) address decodes as
        MIPS16e; no preset decodes microMIPS."""
        ...
    @classmethod
    def mipsbe64(cls) -> SleighArch:
        """64-bit MIPS, big-endian. An odd (ISA-bit) address decodes as
        MIPS16e; no preset decodes microMIPS."""
        ...
    @classmethod
    def mipsle64(cls) -> SleighArch:
        """64-bit MIPS, little-endian. An odd (ISA-bit) address decodes as
        MIPS16e; no preset decodes microMIPS."""
        ...
    @classmethod
    def arm(cls) -> SleighArch:
        """32-bit ARM in ARM mode, little-endian."""
        ...
    @classmethod
    def arm_be(cls) -> SleighArch:
        """Legacy BE32 32-bit ARM: big-endian instructions AND data, pre-ARMv6.
        Almost every decode fails on a BE8 image; use `arm_be_kernel` for
        those."""
        ...
    @classmethod
    def arm_be_kernel(cls) -> SleighArch:
        """BE8 32-bit ARM: little-endian instructions, big-endian data. Every
        ARMv6+ big-endian Linux target, kernel and userland, is this, and
        `load_elf` picks it on `EF_ARM_BE8`."""
        ...
    @classmethod
    def arm_thumb(cls) -> SleighArch:
        """32-bit ARM built `-mthumb`, little-endian only. ARM-state code
        (veneers, `call_weak_fn`) still decodes, so interworking holds."""
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
    def endianness(self) -> str:
        """Byte order of this architecture: `"little"` or `"big"`."""
        ...
    def reg(self, name: str) -> Optional[Vn]:
        """The varnode for the register called `name`, or `None` when the
        name is not a register of this arch."""
        ...
    def reg_name(self, vn: Vn) -> Optional[str]:
        """The register name for `vn`, or `None` when it names no register
        (a non-register space, or an offset and size absent from this
        arch's table)."""
        ...
    def __eq__(self, other) -> bool:
        """Equal when built from the same preset."""
        ...
    def __hash__(self) -> int: ...

class CallingConvention:
    """How a function receives arguments, returns values, and which
    registers it may clobber.

    Every `analyze` call takes one. Use a preset, or `custom` for an unusual
    ABI.
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
        """The AAPCS ABI for 32-bit ARM, hard-float (VFP) argument
        variant."""
        ...
    @classmethod
    def arm_aapcs_soft(cls) -> CallingConvention:
        """The AAPCS ABI for 32-bit ARM built `-mfloat-abi=soft` /
        `softfp`, where floats pass and return in the core registers."""
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
    def x86_linux_kernel(cls) -> CallingConvention:
        """32-bit x86 Linux kernel-internal convention (`-mregparm=3`).

        A variadic callee (`printk`) and an `asmlinkage` one take every
        argument on the stack, yet a call to one under this convention still
        lists EAX, EDX and ECX as arguments 0-2, so its first argument reads
        as `arg(3)`. Map such callees to `x86_cdecl()` in
        `LifterOptions(per_address_ccs=...)`. To analyse one of them, pass
        `cc=x86_cdecl()` and map its regparm callees to this convention."""
        ...
    @classmethod
    def powerpc_sysv32_linux_kernel(cls) -> CallingConvention:
        """`powerpc_sysv32` for kernel code, built `-mno-altivec -mno-vsx`:
        no AltiVec register carries an argument or a result, so a function
        tracks none of them."""
        ...
    @classmethod
    def powerpc64_elf_v1_linux_kernel(cls) -> CallingConvention:
        """`powerpc64_elf_v1` without the AltiVec argument and return
        windows; see `powerpc_sysv32_linux_kernel`."""
        ...
    @classmethod
    def powerpc64_elf_v2_linux_kernel(cls) -> CallingConvention:
        """`powerpc64_elf_v2` without the AltiVec argument and return
        windows; see `powerpc_sysv32_linux_kernel`."""
        ...
    @classmethod
    def custom(
        cls,
        sleigh: SleighArch,
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
        arg_passing_regs_float: list[str] = ...,
        reg_args_adjoin_stack_args: bool = ...,
    ) -> CallingConvention:
        """Build a convention from register names resolved against `sleigh`.

        Registers are named as the architecture spells them. Stack arguments
        start `stack_arg_base` bytes from the call-time stack pointer and
        advance by `stack_arg_increment`; `ret_stack_pop` is how many bytes
        the callee pops on return. `preserves_memory=True` declares that a
        call through this convention cannot write memory.
        `arg_passing_regs_float` names the float/vector argument registers;
        they are appended after `arg_passing_regs`, so the first one is
        `arg(len(arg_passing_regs))` in a `call()` pattern.
        `reg_args_adjoin_stack_args=True` says each argument register has a
        stack home just below the stack arguments, one slot each (ARM AAPCS,
        MIPS O32 / N64, PowerPC64 ELF), so a by-value argument split between
        them is one object: its address reaching a callee keeps every
        incoming stack argument read after a call unknown.

        Names are resolved and most ABI invariants checked here, so a bad one
        raises `StriderError` at construction. Among them:

        - a `link_register` must also appear in `callee_saved_regs`;
        - `stack_pointer` must be as wide as the arch's addresses, and so
          must `stack_arg_increment` when `stack_arg_base` is set;
        - with `link_register=None`, `ret_stack_pop` must cover the return
          address `call` pushes (x86: 4, x86-64: 8). A link-register arch
          (ARM, MIPS, PPC) pops 0 and sets `link_register`.

        The resolved varnodes are frozen against `sleigh`'s registers, so the
        result is usable only with a `Lifter` with the same register table
        (`arm`, `arm_thumb` and `arm_be_kernel` share one); any other raises
        `StriderError` at `analyze` time.
        """
        ...
    def preserves_all(self) -> CallingConvention:
        """A variant that clobbers nothing: every register callee-saved and
        memory unchanged, with no arguments or return value. Keeps this
        convention's stack/link-register geometry.

        Use it as a per-address override for a transparent hook that changes
        no caller state (`__fentry__` / `mcount`), e.g.
        `CallingConvention.x86_64_systemv().preserves_all()`.
        """
        ...
    def preserves_regs(self) -> CallingConvention:
        """Like `preserves_all` but leaves memory clobberable: every register
        is preserved, memory is not.

        The argument registers and stack arguments survive, unlike under
        `preserves_all`, so a call still carries its arguments: a callee that
        writes memory has to be seen taking a frame address."""
        ...
    def name(self) -> str:
        """The preset's short name, e.g. `"x86_64_systemv"`, or `"custom"`
        for a `custom(...)` one."""
        ...
    def __eq__(self, other) -> bool:
        """Equal when the same constructor calls built both: a `custom` never
        equals a preset, nor one resolved against another preset."""
        ...
    def __hash__(self) -> int: ...

class VnSpace:
    """One of the disassembler's built-in address spaces."""
    RAM: VnSpace
    REGISTER: VnSpace
    CONST: VnSpace
    UNIQUE: VnSpace
    def name(self) -> str:
        """The space's name, e.g. `"RAM"` or `"REGISTER"`."""
        ...
    def __eq__(self, other) -> bool: ...
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
    def __eq__(self, other) -> bool: ...
    def __hash__(self) -> int: ...

class CallOtherAbi:
    """How one Sleigh user-op is lifted: the implicit register footprint,
    the memory effect, and whether control returns.

    Pass one per user-op name to
    `strider.cfg.CfgOptions(call_other_abis={name: abi})`, ahead of the
    built-in table. `Lifter.user_op_names()` lists the names a binary can
    contain and `Lifter.call_other_abi(name)` reads back the current
    classification.
    """
    @classmethod
    def noop(cls) -> CallOtherAbi:
        """The op lifts to nothing: control and memory unchanged, any
        p-code result ignored."""
        ...
    @classmethod
    def pure(cls) -> CallOtherAbi:
        """Pure compute: a p-code result, no implicit registers, no memory
        effect."""
        ...
    @classmethod
    def mem_clobber(cls) -> CallOtherAbi:
        """No implicit registers, memory conservatively clobbered."""
        ...
    @classmethod
    def no_return(cls) -> CallOtherAbi:
        """Control does not pass the op, which ends its region. Empty
        footprint; `custom(..., no_return=True)` carries one."""
        ...
    @classmethod
    def custom(
        cls,
        sleigh: SleighArch,
        implicit_reads: list[str] = ...,
        implicit_writes: list[str] = ...,
        clobbers_memory: bool = ...,
        no_return: bool = ...,
    ) -> CallOtherAbi:
        """An ABI naming implicit registers, resolved against `sleigh` here,
        so an unknown name raises `StriderError` at construction.

        `implicit_reads` are read beyond the p-code-explicit operands and
        lead the lifted call's argument list; `implicit_writes` are written
        beyond the p-code-explicit result.

        The resolved varnodes are frozen against `sleigh`'s registers, so the
        result is usable only with a `Lifter` with the same register table
        (`arm`, `arm_thumb` and `arm_be_kernel` share one); any other raises
        `StriderError` at `analyze` time.
        """
        ...
    @property
    def implicit_reads(self) -> list[str]:
        """Register names read beyond the p-code-explicit operands."""
        ...
    @property
    def implicit_writes(self) -> list[str]:
        """Register names written beyond the p-code-explicit result."""
        ...
    @property
    def clobbers_memory(self) -> bool:
        """Whether the op advances the IR's memory edge."""
        ...
    @property
    def is_no_return(self) -> bool:
        """Whether control does not pass the op."""
        ...
    @property
    def is_noop(self) -> bool:
        """Whether the op lifts to nothing at all."""
        ...
    def __eq__(self, other) -> bool: ...
    def __hash__(self) -> int: ...
