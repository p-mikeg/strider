import strider


def test_sleigh_construct_with_buffer_reader():
    arch = strider.sleigh.SleighArch.x86_64()
    mem = strider.reader.BufferReader(0x1000, b"\x90\x90\x90\x90")  # 4 NOPs
    sleigh = strider.sleigh.Sleigh(arch, mem)
    assert sleigh is not None
    assert sleigh.arch_name() == "x86_64"
    assert "Sleigh" in repr(sleigh)


# `Vn.__repr__` delegates to rsleigh's own formatter, so a formatter change
# upstream lands here directly and these expectations move with it.


def test_vn_repr_for_register_uses_rsleigh_display():
    # Sleigh always assigns x86_64 RSP to REGISTER:0x20, size 8.
    arch = strider.sleigh.SleighArch.x86_64()
    mem = strider.reader.BufferReader(0x1000, b"\x00")
    sleigh = strider.sleigh.Sleigh(arch, mem)
    rsp = sleigh.reg("RSP")
    assert rsp is not None
    # Registers format as `<space-shortcut>[0x<off>]:<size>`; REGISTER's
    # shortcut character is `%`.
    assert repr(rsp) == "%[0x20]:8"


def test_vn_repr_for_const_drops_space_prefix():
    # Constants format as `0x<off>:<size>`, no space prefix and no `[]`,
    # since the offset alone identifies them.
    const_space = strider.sleigh.VnSpace.CONST
    vn = strider.sleigh.Vn(const_space, 0x42, 4)
    assert repr(vn) == "0x42:4"


def test_vnspace_constants_are_instances_not_callables():
    from strider.sleigh import VnSpace

    assert isinstance(VnSpace.REGISTER, VnSpace)
    assert VnSpace.REGISTER == VnSpace.REGISTER
    assert VnSpace.REGISTER != VnSpace.RAM
    assert VnSpace.REGISTER.name() == "REGISTER"


def test_vn_space_hash_consistent_with_eq():
    """`VnSpace.__hash__` hashes the underlying Sleigh space identity rather
    than the wrapper's address, so two separately obtained `VnSpace.RAM`
    values work as one dict key / set member.
    """
    a = strider.sleigh.VnSpace.RAM
    b = strider.sleigh.VnSpace.RAM
    assert a == b, "two VnSpace.RAM must compare equal"
    assert hash(a) == hash(b), (
        f"equal VnSpaces must hash equally; got hash(a)={hash(a)}, hash(b)={hash(b)}"
    )
    d = {a: 1}
    assert d[b] == 1, "VnSpace.RAM must work as a dict key after fresh construction"
    s = {a, b}
    assert len(s) == 1, "set of equal VnSpaces must collapse to a single entry"


def test_vn_space_distinct_spaces_compare_unequal():
    """Hash inequality is not a contract requirement, but hashing the
    shortcut byte keeps RAM and REGISTER in different buckets, which is
    worth pinning as a bucketing-quality signal.
    """
    ram = strider.sleigh.VnSpace.RAM
    reg = strider.sleigh.VnSpace.REGISTER
    assert ram != reg
    assert hash(ram) != hash(reg)


def test_vn_hash_includes_addr_space():
    """`Vn.__hash__` mixes in addr_space, so `RAM[0x10]:8` and
    `REGISTER[0x10]:8` land in different buckets instead of chaining.
    """
    ram_vn = strider.sleigh.Vn(strider.sleigh.VnSpace.RAM, 0x10, 8)
    reg_vn = strider.sleigh.Vn(strider.sleigh.VnSpace.REGISTER, 0x10, 8)
    assert ram_vn != reg_vn, "different-space varnodes must compare unequal"
    assert hash(ram_vn) != hash(reg_vn), (
        "Vn.__hash__ must mix in addr_space; otherwise different-space "
        "varnodes share a bucket"
    )
