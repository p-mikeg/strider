import pytest
import strider


@pytest.mark.parametrize("name", [
    # Userland presets
    "x86_64_systemv", "aarch64_aapcs64", "arm_aapcs",
    "mips_o32", "mips_n64",
    "powerpc_sysv32", "powerpc64_elf_v1", "powerpc64_elf_v2",
    "x86_cdecl",
    # Linux kernel presets
    "x86_linux_kernel", "x86_64_linux_kernel",
    "aarch64_linux_kernel", "arm_linux_kernel",
    "mips_linux_kernel_o32", "mips_linux_kernel_n64",
    # Linux syscall presets
    "x86_linux_syscall", "x86_64_linux_syscall",
    "aarch64_linux_syscall", "arm_linux_syscall",
    "mips_linux_syscall_o32", "mips_linux_syscall_n64",
])
def test_cc_presets(name):
    cc = getattr(strider.CallingConvention, name)()
    assert isinstance(cc, strider.CallingConvention)
    assert cc.name() == name


def test_cc_repr():
    cc = strider.CallingConvention.x86_cdecl()
    assert "CallingConvention" in repr(cc)
    assert "x86_cdecl" in repr(cc)
