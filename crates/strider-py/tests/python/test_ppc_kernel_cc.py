"""PowerPC vector registers belong to the convention. A Linux kernel is built
`-mno-altivec -mno-vsx`, so the kernel presets name none of them; naming them
would seed twelve 16-byte varnodes on every function and renumber every
collected stack argument's `arg(n)`. `e_entry` is what separates the two
flavours: Linux links its own text at `KERNELBASE`, above `TASK_SIZE`, so no
userland image can have its entry there."""

import shutil
import struct

import pytest
import strider
from strider._api import _arch_and_cc_for_elf, _ElfHeader

PPC32 = "fixtures/out/ppc32be/calling_convention.elf"
PPC64BE = "fixtures/out/ppc64be/calling_convention.elf"
PPC64LE = "fixtures/out/ppc64le/calling_convention.elf"


def _cc_with_entry(src, entry, tmp_path):
    """`src` copied with its `e_entry` rewritten, as that copy's CC preset."""
    dst = tmp_path / "probe.elf"
    shutil.copy(src, dst)
    header = _ElfHeader(str(dst))
    fmt = ("<" if header.is_little_endian else ">") + ("Q" if header.is_64bit else "I")
    with open(dst, "r+b") as f:
        f.seek(24)
        f.write(struct.pack(fmt, entry))
    return _arch_and_cc_for_elf(_ElfHeader(str(dst)))[1].name()


@pytest.mark.parametrize(
    "src, userland, kernel, kernelbase",
    [
        (PPC32, "powerpc_sysv32", "powerpc_sysv32_linux_kernel", 0xC000_0000),
        (
            PPC64BE,
            "powerpc64_elf_v2",
            "powerpc64_elf_v2_linux_kernel",
            0xC000_0000_0000_0000,
        ),
        (
            PPC64LE,
            "powerpc64_elf_v2",
            "powerpc64_elf_v2_linux_kernel",
            0xC000_0000_0000_0000,
        ),
    ],
)
def test_the_entry_address_picks_the_kernel_or_userland_abi(
    src, userland, kernel, kernelbase, tmp_path
):
    assert _cc_with_entry(src, kernelbase, tmp_path) == kernel
    assert _cc_with_entry(src, kernelbase - 1, tmp_path) == userland
    # A relocatable object has no entry at all; userland is the answer that
    # costs precision rather than correctness.
    assert _cc_with_entry(src, 0, tmp_path) == userland


def test_the_shipped_userland_fixtures_are_not_taken_for_kernels():
    for path in (PPC32, PPC64BE, PPC64LE):
        assert "kernel" not in _arch_and_cc_for_elf(_ElfHeader(path))[1].name(), path


def test_the_abi_level_still_decides_between_elfv1_and_elfv2(tmp_path):
    """A big-endian ELFv1 kernel is `powerpc64_elf_v1_linux_kernel`: the two
    axes are independent."""
    dst = tmp_path / "probe.elf"
    shutil.copy(PPC64BE, dst)
    with open(dst, "r+b") as f:
        f.seek(24)
        f.write(struct.pack(">Q", 0xC000_0000_0000_0000))
        f.seek(48)
        f.write(struct.pack(">I", 1))
    assert (
        _arch_and_cc_for_elf(_ElfHeader(str(dst)))[1].name()
        == "powerpc64_elf_v1_linux_kernel"
    )


def test_the_kernel_preset_tracks_no_vector_register():
    """The cost the split removes: under the userland preset every function
    carries v2-v13 as 16-byte tracked varnodes."""
    import re

    counts = {}
    for name in ("powerpc64_elf_v2", "powerpc64_elf_v2_linux_kernel"):
        cc = getattr(strider.sleigh.CallingConvention, name)()
        prog = strider.lift.load_elf(PPC64LE, cc=cc)
        fn = prog.analyze("returns_int").function
        tracked = re.search(r"^tracked \[(.*)\]$", fn.to_text(False), re.M)
        assert tracked is not None, "`to_text` heads the function with its tracked set"
        counts[name] = sum(1 for vn in tracked.group(1).split(", ") if vn.endswith(":16"))
    assert counts == {"powerpc64_elf_v2": 12, "powerpc64_elf_v2_linux_kernel": 0}
