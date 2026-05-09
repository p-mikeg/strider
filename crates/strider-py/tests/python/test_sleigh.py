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


# ── Hash/eq contract regression tests (round8-1F HIGH / MED) ──────────────


def test_vn_space_hash_consistent_with_eq():
    """Regression for round8-1F HIGH: PyVnSpace.__hash__ must be a function
    of the inner identity, not the field's stack address.  Two separately
    constructed VnSpace.ram() objects must hash equally so they work as
    dict keys / set members.
    """
    a = strider.VnSpace.ram()
    b = strider.VnSpace.ram()
    assert a == b, "two VnSpace.ram() must compare equal"
    assert hash(a) == hash(b), (
        f"equal VnSpaces must hash equally; got hash(a)={hash(a)}, hash(b)={hash(b)}"
    )
    # And they must work as dict / set members.
    d = {a: 1}
    assert d[b] == 1, "VnSpace.ram() must work as a dict key after fresh construction"
    s = {a, b}
    assert len(s) == 1, "set of equal VnSpaces must collapse to a single entry"


def test_vn_space_distinct_spaces_compare_unequal():
    """Different spaces must compare unequal.  Hash inequality is not
    a contract requirement, but the implementation hashes the shortcut
    byte so RAM and REGISTER end up in different buckets — a quality-of-
    bucketing signal worth pinning.
    """
    ram = strider.VnSpace.ram()
    reg = strider.VnSpace.register()
    assert ram != reg
    assert hash(ram) != hash(reg)


def test_vn_hash_includes_addr_space():
    """Regression for round8-1F MED: Vn.__hash__ must mix in addr_space so
    same-offset/same-size varnodes in different spaces don't share a
    bucket.  Without this, `RAM[0x10]:8` and `REGISTER[0x10]:8` would
    collide and hash-table chains would degrade to O(n).
    """
    ram_vn = strider.Vn(strider.VnSpace.ram(), 0x10, 8)
    reg_vn = strider.Vn(strider.VnSpace.register(), 0x10, 8)
    assert ram_vn != reg_vn, "different-space varnodes must compare unequal"
    assert hash(ram_vn) != hash(reg_vn), (
        "Vn.__hash__ must mix in addr_space; otherwise different-space "
        "varnodes share a bucket"
    )
