from __future__ import annotations

import strider
from strider import ir, reader, sleigh
from strider.pattern import Capture, call_other, function_arg, int_add, int_const

BASE = 0x1000
SYSCALL_ENTRY = BASE
ADD_ENTRY = BASE + 0x20

# 32-bit x86 `write(1, 0x2000, 12)` through the Linux int 0x80 gate:
#   mov eax, 4        b8 04 00 00 00     SYS_write
#   mov ebx, 1        bb 01 00 00 00     fd
#   mov ecx, 0x2000   b9 00 20 00 00     buf
#   mov edx, 12       ba 0c 00 00 00     count
#   int 0x80          cd 80
#   ret               c3
SYSCALL_STUB = bytes.fromhex("b804000000bb01000000b900200000ba0c000000cd80c3")

# A callee taking its two arguments in EBX and ECX and returning their sum:
#   mov eax, ebx      89 d8
#   add eax, ecx      01 c8
#   ret               c3
REG_ABI_ADD = bytes.fromhex("89d801c8c3")

IMAGE = SYSCALL_STUB.ljust(ADD_ENTRY - BASE, b"\x00") + REG_ABI_ADD + bytes(16)

mem = reader.BufferReader(BASE, IMAGE)
lft = strider.lift.lifter(sleigh.SleighArch.x86(), mem)
# Register names resolve against a Sleigh, so both `custom` constructors need one.
sl = sleigh.Sleigh(sleigh.SleighArch.x86(), mem)
cdecl = sleigh.CallingConvention.x86_cdecl()


# ---------------------------------------------------------------- discovery

# Every user-op name this architecture can emit, indexed by user-op id.
names = lft.user_op_names()
print(f"x86 user-ops: {len(names)}, including 'swi' ({'swi' in names})")

# What strider already believes about one. `int 0x80` lifts to `swi`, which
# carries every x86 software interrupt, so the table can say only that memory
# is clobbered: which registers a vector reads is the OS's convention.
swi = lft.call_other_abi("swi")
assert swi is not None
print(
    f"swi: reads={swi.implicit_reads} writes={swi.implicit_writes} "
    f"clobbers_memory={swi.clobbers_memory}"
)

# A footprint fixed by the architecture is in the table already: x86-64 has
# one syscall convention, so `syscall` carries it.
x86_64 = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem)
syscall = x86_64.call_other_abi("syscall")
assert syscall is not None
print(
    f"x86-64 syscall: reads={syscall.implicit_reads} "
    f"writes={syscall.implicit_writes}"
)

# `None` is no answer at all, and fails the lift of any function containing it.
print(f"x86 sysenter: {lft.call_other_abi('sysenter')}")


# ------------------------------------------------- CallOtherAbi.custom

# Raw CallOther inputs are [ctrl, mem, implicit reads..., p-code operands...],
# and `CallOtherPat.arg` indexes them unshifted.
SLOTS = range(2, 7)


def syscall_stub(abis: dict[str, sleigh.CallOtherAbi]) -> ir.Function:
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(
            allow_code_before_start_addr=True, call_other_abis=abis
        )
    )
    _cfg, fn, _unresolved = lft.analyze(SYSCALL_ENTRY, cdecl, opts)
    return fn


def operands(fn: ir.Function) -> list[int | None]:
    """The constant in each raw input slot of the `swi` node, or None."""
    k = Capture()
    return [
        fn.find_unique_value(call_other().name("swi").arg(slot, int_const(k)), k)
        for slot in SLOTS
    ]


def setup_constants(fn: ir.Function) -> int:
    """How many of the four register-setup constants survived optimization."""
    return sum(len(fn.find_all(int_const(v))) for v in (4, 1, 0x2000, 12))


default = syscall_stub({})

# The Linux i386 convention: EAX = syscall number, EBX/ECX/EDX = arguments,
# EAX = result. Stated per analysis, the way a calling convention is.
int80 = sleigh.CallOtherAbi.custom(
    sl,
    implicit_reads=["EAX", "EBX", "ECX", "EDX"],
    implicit_writes=["EAX"],
    clobbers_memory=True,
)
overridden = syscall_stub({"swi": int80})

print(f"\ndefault    swi inputs{list(SLOTS)}: {operands(default)}")
print(f"overridden swi inputs{list(SLOTS)}: {operands(overridden)}")
print(
    f"register-setup constants left in the graph: "
    f"default={setup_constants(default)} overridden={setup_constants(overridden)}"
)

# Believing `swi` reads nothing, strider finds the four movs dead and deletes
# them; the only argument left is the p-code-explicit vector number 0x80.
assert operands(default) == [0x80, None, None, None, None]
assert setup_constants(default) == 0

# Under the override they lead the argument list, and the call reads off the
# graph as write(fd=1, buf=0x2000, count=12).
assert operands(overridden) == [4, 1, 0x2000, 12, 0x80]
assert setup_constants(overridden) == 4


# --------------------------------------------- CallingConvention.custom

# EBX and ECX carry the arguments, EAX the result, and `ret` pops the 4-byte
# return address x86's `call` pushed. An unknown register name, or a link
# register missing from callee_saved_regs, raises StriderError here.
reg_abi = sleigh.CallingConvention.custom(
    sleigh=sl,
    arg_passing_regs=["EBX", "ECX"],
    callee_saved_regs=["EBP", "ESI", "EDI"],
    ret_val_regs=["EAX"],
    ret_val_regs_float=[],
    stack_pointer="ESP",
    stack_arg_base=None,
    stack_arg_increment=4,
    ret_stack_pop=4,
    link_register=None,
    preserves_memory=False,
)


def add_routine(cc: sleigh.CallingConvention) -> ir.Function:
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    )
    _cfg, fn, _unresolved = lft.analyze(ADD_ENTRY, cc, opts)
    return fn


sum_of_args = int_add(function_arg(0), function_arg(1))
print()
for label, fn in (("x86_cdecl", add_routine(cdecl)), ("custom", add_routine(reg_abi))):
    print(
        f"{label:>10}: nodes={fn.node_count()} "
        f"arg(0) sites={len(fn.find_all(function_arg(0)))} "
        f"add(arg0, arg1) matches={len(fn.find_all(sum_of_args))}"
    )

# cdecl passes arguments on the stack, so the EBX and ECX this routine reads
# are entry values of registers nobody declared meaningful.
cdecl_fn = add_routine(cdecl)
assert not cdecl_fn.find_all(function_arg(0))
assert not cdecl_fn.find_all(sum_of_args)

# Under the register ABI the same three instructions are a two-argument add.
custom_fn = add_routine(reg_abi)
custom_fn.find_unique(sum_of_args)
assert custom_fn.node_count() < cdecl_fn.node_count()
