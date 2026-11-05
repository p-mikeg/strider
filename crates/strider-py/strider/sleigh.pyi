"""Architectures, calling conventions, user-op ABIs and varnodes."""

from __future__ import annotations

from typing import Any, Optional

__all__: list[str]

#: Every architecture preset name.
ARCH_PRESETS: tuple[str, ...]

#: Every calling-convention preset name, family by family.
CC_PRESETS: tuple[str, ...]

class SleighArch:
    """A target architecture: a preset below, or the one
    `strider.lift.load_elf` picks from the ELF header."""
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
    @classmethod
    def preset(cls, name: str) -> SleighArch:
        """The preset called `name` (one of `ARCH_PRESETS`); else raises
        `StriderError`."""
        ...
    def calling_conventions(self) -> tuple[CallingConvention, ...]:
        """The calling-convention presets for this arch, in roster order."""
        ...
    def name(self) -> str:
        """The preset's short name, e.g. `"x86_64"` or `"arm_thumb"`."""
        ...
    def endianness(self) -> str:
        """`"little"` or `"big"`."""
        ...
    def reg(self, name: str) -> Optional[Vn]:
        """The varnode of register `name`, or `None`."""
        ...
    def reg_name(self, vn: Vn) -> Optional[str]:
        """The register name of `vn`, or `None`."""
        ...
    def __eq__(self, other) -> bool:
        """Equal when built from the same preset."""
        ...
    def __hash__(self) -> int: ...

class CallingConvention:
    """How a function receives arguments, returns values, and which
    registers it may clobber: a preset, or `custom`."""
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

        Variadic (`printk`) and `asmlinkage` callees take every argument on
        the stack: map them to `x86_cdecl()` in
        `LifterOptions(per_address_ccs=...)`. To analyse one, pass
        `cc=x86_cdecl()` and map its regparm callees to this convention."""
        ...
    @classmethod
    def powerpc_sysv32_linux_kernel(cls) -> CallingConvention:
        """`powerpc_sysv32` for kernel code (`-mno-altivec -mno-vsx`): no
        AltiVec register carries an argument or result."""
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
        entry_address: Optional[str] = ...,
    ) -> CallingConvention:
        """A convention from register names resolved against `sleigh`.

        Stack arguments start `stack_arg_base` bytes from the call-time SP
        (`None`: none on the stack) and advance by `stack_arg_increment`;
        `ret_stack_pop` is how many bytes `ret` pops, its return address
        included (x86: 4, x86-64: 8; usually 0 with a `link_register`).
        `preserves_memory=True` declares that a call cannot write memory.
        `arg_passing_regs_float` are numbered after `arg_passing_regs` in a
        `call()` pattern. `reg_args_adjoin_stack_args` gives each argument
        register a stack home just below the stack arguments (ARM AAPCS,
        MIPS, PowerPC64 ELF). `entry_address` holds the function's own
        address on entry (PPC64 ELFv2 `r12`, MIPS `t9`).

        A `link_register` missing from `callee_saved_regs` is clobbered by a
        call to this convention, so it suits only a `per_address_ccs` callee
        (the ppc32 `-mbss-plt` GOT thunk); `analyze` raises `StriderError`
        for it as the function's own convention.

        Raises `StriderError` for an unknown name or a broken ABI invariant,
        such as a `stack_pointer` (or, with `stack_arg_base`, a
        `stack_arg_increment`) not address-wide, a negative `ret_stack_pop`,
        or, without a `link_register`, one below the address size in bytes.
        Usable only with a `Lifter` sharing `sleigh`'s register table (`arm`,
        `arm_thumb` and `arm_be_kernel` share one); any other raises
        `StriderError` at `analyze`.
        """
        ...
    @classmethod
    def preset(cls, name: str) -> CallingConvention:
        """The preset called `name` (one of `CC_PRESETS`); else raises
        `StriderError`."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """The keyword arguments of `custom` other than `sleigh`:
        `CallingConvention.custom(arch, **cc.to_dict())` rebuilds it. Raises
        `StriderError` for a `preserves_all()` / `preserves_regs()` variant."""
        ...
    def preserves_all(self) -> CallingConvention:
        """A variant that clobbers nothing: every register callee-saved,
        memory unchanged, no arguments or return value; stack geometry kept.
        For a transparent hook (`__fentry__` / `mcount`)."""
        ...
    def preserves_regs(self) -> CallingConvention:
        """Like `preserves_all`, but memory may be clobbered, so the call keeps
        its arguments."""
        ...
    def name(self) -> str:
        """The preset name, e.g. `"x86_64_systemv"`, or `"custom"`."""
        ...
    def __eq__(self, other) -> bool:
        """Equal when built by the same constructor calls; a `custom` never
        equals a preset."""
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
    """A varnode: an address space, an offset, and a size in bytes."""
    def __init__(self, space: VnSpace, off: int, size: int) -> None:
        """The varnode at `off` in `space`, `size` bytes wide."""
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
    """How one Sleigh user-op is lifted: implicit registers, memory effect
    and whether control returns. Pass to
    `strider.cfg.CfgOptions(call_other_abis={name: abi})`;
    `Lifter.call_other_abi(name)` reads back the current one.
    """
    @classmethod
    def noop(cls) -> CallOtherAbi:
        """The op is dropped: no IR node, any p-code result ignored."""
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
        """Control does not pass the op. Empty footprint; use
        `custom(..., no_return=True)` for one."""
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
        """An ABI naming implicit registers resolved against `sleigh`; an
        unknown name raises `StriderError`. `implicit_reads` lead the lifted
        call's arguments. Usable only with a `Lifter` sharing `sleigh`'s
        register table (`arm`, `arm_thumb` and `arm_be_kernel` share one).
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
        """Whether this is `noop()`."""
        ...
    def __eq__(self, other) -> bool: ...
    def __hash__(self) -> int: ...
