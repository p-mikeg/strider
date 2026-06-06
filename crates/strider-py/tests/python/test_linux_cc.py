"""Per-preset existence + smoke tests for the Linux kernel + syscall
calling-convention bindings.

Mirror of `crates/target/tests/linux_cc_presets.rs` for the Python
side.  Each test:

  1. Calls the classmethod on `strider.CallingConvention`.
  2. Asserts the returned object has `name() == <preset>`.
  3. Builds a `strider.Lifter(arch, sleigh, cc)` against an existing
     fixture ELF of the matching arch — this is the smoke check that
     every register name in the new preset resolves through the
     Python binding (a typo would fail at `Strider.__new__`).

The data-layer correctness (which registers each preset declares)
is exhaustively checked in the Rust suite; here we only assert the
classmethod is reachable and Strider construction succeeds.
"""

from __future__ import annotations

import pathlib

import pytest

import strider

from .conftest import fixture_path


def _build_strider(arch_factory, cc_factory, fixture_arch_id: str, fixture_case: str):
    elf = fixture_path(fixture_arch_id, fixture_case)
    arch = arch_factory()
    cc = cc_factory()
    mem = strider.load_elf(str(elf)).memory_map()
    sleigh = strider.Sleigh(arch, mem)
    return strider.Lifter(arch, sleigh, cc)


# ── Per-preset existence + name() round-trip ─────────────────────────────


# (factory_name, expected_name) pairs.  The test below parametrises on
# this table so each preset gets its own visible test row.
KERNEL_PRESETS = [
    ("x86_linux_kernel", "x86_linux_kernel"),
    ("x86_64_linux_kernel", "x86_64_linux_kernel"),
    ("aarch64_linux_kernel", "aarch64_linux_kernel"),
    ("arm_linux_kernel", "arm_linux_kernel"),
    ("mips_linux_kernel_o32", "mips_linux_kernel_o32"),
    ("mips_linux_kernel_n64", "mips_linux_kernel_n64"),
]

SYSCALL_PRESETS = [
    ("x86_linux_syscall", "x86_linux_syscall"),
    ("x86_64_linux_syscall", "x86_64_linux_syscall"),
    ("aarch64_linux_syscall", "aarch64_linux_syscall"),
    ("arm_linux_syscall", "arm_linux_syscall"),
    ("mips_linux_syscall_o32", "mips_linux_syscall_o32"),
    ("mips_linux_syscall_n64", "mips_linux_syscall_n64"),
]


@pytest.mark.parametrize("factory,expected_name", KERNEL_PRESETS + SYSCALL_PRESETS)
def test_preset_classmethod_exists_and_names_round_trip(factory, expected_name):
    cls_method = getattr(strider.CallingConvention, factory)
    cc = cls_method()
    assert isinstance(cc, strider.CallingConvention)
    assert cc.name() == expected_name


# ── Strider construction smoke tests ─────────────────────────────────────


def test_x86_linux_kernel_constructs_strider(x86_indirect_branch_elf):
    # x86_linux_kernel is the only kernel preset that DIFFERS from
    # its userland counterpart (regparm(3) vs cdecl).  Build a
    # Strider and confirm the new arg-passing register names (EAX,
    # EDX, ECX) all resolve through Sleigh.
    s = _build_strider(
        strider.SleighArch.x86,
        strider.CallingConvention.x86_linux_kernel,
        "x86", "indirect_branch",
    )
    assert s is not None


@pytest.mark.parametrize("arch_factory,cc_factory,arch_id,case", [
    (strider.SleighArch.x86_64, strider.CallingConvention.x86_64_linux_kernel, "x64", "indirect_branch"),
    (strider.SleighArch.aarch64, strider.CallingConvention.aarch64_linux_kernel, "aarch64", "indirect_branch"),
    (strider.SleighArch.arm, strider.CallingConvention.arm_linux_kernel, "arm", "indirect_branch"),
    (strider.SleighArch.mipsle32, strider.CallingConvention.mips_linux_kernel_o32, "mips32le", "indirect_branch"),
    (strider.SleighArch.mipsle64, strider.CallingConvention.mips_linux_kernel_n64, "mips64le", "indirect_branch"),
])
def test_kernel_aliases_construct_strider(arch_factory, cc_factory, arch_id, case):
    s = _build_strider(arch_factory, cc_factory, arch_id, case)
    assert s is not None


@pytest.mark.parametrize("arch_factory,cc_factory,arch_id,case", [
    (strider.SleighArch.x86, strider.CallingConvention.x86_linux_syscall, "x86", "indirect_branch"),
    (strider.SleighArch.x86_64, strider.CallingConvention.x86_64_linux_syscall, "x64", "indirect_branch"),
    (strider.SleighArch.aarch64, strider.CallingConvention.aarch64_linux_syscall, "aarch64", "indirect_branch"),
    (strider.SleighArch.arm, strider.CallingConvention.arm_linux_syscall, "arm", "indirect_branch"),
    (strider.SleighArch.mipsle32, strider.CallingConvention.mips_linux_syscall_o32, "mips32le", "indirect_branch"),
    (strider.SleighArch.mipsle64, strider.CallingConvention.mips_linux_syscall_n64, "mips64le", "indirect_branch"),
])
def test_syscall_presets_construct_strider(arch_factory, cc_factory, arch_id, case):
    # Syscall CCs aren't expected to lift typical userland binaries
    # cleanly (link_register_vn is None, the convention's arg regs
    # don't match what the binary uses), but Strider construction
    # itself is convention-agnostic — a typo in the syscall preset's
    # register names would fail here.
    s = _build_strider(arch_factory, cc_factory, arch_id, case)
    assert s is not None
