import strider


def test_sleigh_construct_with_memory_map():
    arch = strider.SleighArch.x86_64()
    mem = strider.MemoryMap()
    mem.add_region(0x1000, b"\x90\x90\x90\x90")  # 4 NOPs
    sleigh = strider.Sleigh(arch, mem)
    assert sleigh is not None
    assert sleigh.arch_name() == "x86_64"
    assert "Sleigh" in repr(sleigh)


# ── Vn.__repr__ uses rsleigh's native `Display` impl ────────────────────
#
# rsleigh 4.0.0 ships `impl Display for Vn` (see core_types.rs:139), which
# formats a register varnode as `<space-shortcut>[0x<off>]:<size>` and a
# CONST-space varnode as `0x<off>:<size>`.  Strider-py's `PyVn.__repr__`
# delegates to that — there is no Python-side spelling drift, so when
# rsleigh updates its formatter, strider-py picks up the change for free.


def test_vn_repr_for_register_uses_rsleigh_display():
    # x86_64 RSP — Sleigh always assigns it REGISTER:0x20, size 8.
    arch = strider.SleighArch.x86_64()
    mem = strider.MemoryMap()
    sleigh = strider.Sleigh(arch, mem)
    rsp = sleigh.reg("RSP")
    assert rsp is not None
    # rsleigh's `impl Display for Vn` (core_types.rs:139) formats register
    # varnodes as `<space-shortcut>[0x<off>]:<size>` — the REGISTER space
    # shortcut character is `%`.
    assert repr(rsp) == "%[0x20]:8"


def test_vn_repr_for_const_drops_space_prefix():
    # CONST-space varnodes (`Vn(VnSpace.const_(), off, size)`) are formatted
    # by rsleigh's `Display` as `0x<off>:<size>` — no space prefix and no
    # `[]`, since the offset alone identifies the constant.
    const_space = strider.VnSpace.const_()
    vn = strider.Vn(const_space, 0x42, 4)
    assert repr(vn) == "0x42:4"
