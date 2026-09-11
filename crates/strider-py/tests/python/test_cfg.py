import time

import pytest
import strider

from .conftest import symbol_addr


def test_build_cfg_for_array_sum(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    assert cfg is not None


def test_cfg_to_html_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    out_html = tmp_path / "cfg.html"
    assert cfg.to_html(str(out_html)) is None
    assert out_html.exists()
    assert out_html.stat().st_size > 0


def test_cfg_to_dot_writes_nonempty_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    out_dot = tmp_path / "cfg.dot"
    assert cfg.to_dot(str(out_dot)) is None
    assert out_dot.exists()
    assert out_dot.stat().st_size > 0


def test_cfg_to_html_returns_html_str(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    html = cfg.to_html()
    assert isinstance(html, str)
    assert "<html" in html.lower() or "svg" in html.lower()


def test_cfg_to_dot_str_and_file(x86_memory_elf, tmp_path):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    dot_str = cfg.to_dot()
    assert isinstance(dot_str, str) and "digraph" in dot_str.lower()

    out = tmp_path / "c.dot"
    assert cfg.to_dot(str(out)) is None
    assert out.read_text()

    assert isinstance(cfg.to_html(), str)
    assert not hasattr(cfg, "html_str")
    assert not hasattr(cfg, "raw_neighborhood_dot")


def test_cfg_region_texts_renamed_private(x86_memory_elf):
    """`region_texts` is private (`_region_texts`); it backs explore.py's
    search bar and is not a stable public API."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    assert not hasattr(cfg, "region_texts")
    texts = cfg._region_texts()
    assert isinstance(texts, dict) and len(texts) > 0


def test_cfg_region_at(x86_memory_elf):
    """The CFG unit is a Region, so the accessor is `region_at`; there is
    no `block_at`."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    region_idx = cfg.region_at(addr)
    assert isinstance(region_idx, int)
    assert not hasattr(cfg, "block_at")


def test_build_cfg_leaves_lifter_reusable(x86_memory_elf):
    """build_cfg must not consume the Lifter: it stays usable for a second
    build, and earlier Cfgs still render (they render through the Lifter,
    which owns the Sleigh)."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)

    cfg1 = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    cfg2 = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    assert cfg2 is not None

    # Still renders after the Lifter was reused for cfg2.
    html1 = cfg1.to_html()
    assert isinstance(html1, str)
    assert len(html1) > 0


def _branching_cfg():
    """x86-64 with a two-byte terminator, so the first region's last
    instruction owns a byte past its own address:

        0x1000  31 c0     xor eax, eax
        0x1002  85 ff     test edi, edi
        0x1004  74 02     je 0x1008
        0x1006  89 f8     mov eax, edi
        0x1008  c3        ret
    """
    code = bytes.fromhex("31c085ff740289f8c3")
    mem = strider.reader.BufferReader(0x1000, code + bytes(16))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    return lift.build_cfg(0x1000)


def test_region_at_owns_its_last_instructions_bytes():
    cfg = _branching_cfg()
    entry = cfg.region_at(0x1004)
    assert entry is not None
    assert cfg.region_at(0x1005) == entry


def test_region_at_rejects_an_address_past_every_region():
    assert _branching_cfg().region_at(0x1009) is None


def _chain_cfg(branches: int):
    """x86-64 `test edi,edi; je +0` repeated, then `ret`: one region per
    branch, all four bytes apart."""
    body = bytes.fromhex("85ff7400") * branches + b"\xc3"
    mem = strider.reader.BufferReader(0x1000, body + bytes(32))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    return lift.build_cfg(0x1000), 0x1000 + len(body)


def _region_at_seconds_per_call(cfg, addr, n=20000):
    cfg.region_at(addr)  # warm whatever the first call builds
    start = time.perf_counter()
    for _ in range(n):
        cfg.region_at(addr)
    return (time.perf_counter() - start) / n


def test_region_at_does_not_scale_with_region_count():
    """A linear scan over regions costs ~60x more on 1500 regions than on
    11; a lookup index costs the same on both. The bound is a ratio, so it
    does not depend on how fast the machine is."""
    small, small_end = _chain_cfg(10)
    big, big_end = _chain_cfg(1500)

    for addr_small, addr_big in (
        (small_end + 0x10000, big_end + 0x10000),  # past every region
        (small_end - 4, big_end - 4),  # in the last region
    ):
        ratio = _region_at_seconds_per_call(
            big, addr_big
        ) / _region_at_seconds_per_call(small, addr_small)
        assert ratio < 10, f"region_at is {ratio:.0f}x slower on 1500 regions"


def test_region_at_answers_every_address_the_same_as_a_full_scan():
    """The index must not change which region owns an address, including the
    bytes past an instruction's start and the addresses no region owns."""
    cfg, end = _chain_cfg(40)
    seen = {addr: cfg.region_at(addr) for addr in range(0x1000, end + 8)}
    assert seen[0x1000] is not None
    assert all(v is None for a, v in seen.items() if a >= end)
    # Every byte of the covered span is owned, interior bytes included.
    assert all(v is not None for a, v in seen.items() if a < end)
    assert len(set(v for v in seen.values() if v is not None)) > 1


def test_cfg_style_reaches_both_renderers(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    cfg = s.build_cfg(addr, strider.cfg.CfgOptions(allow_code_before_start_addr=True))

    # `dark_cfg` paints the graph background; `empty` sets no attributes.
    for render in (cfg.to_dot, cfg.to_html):
        default, dark, empty = render(), render(style="dark_cfg"), render(style="empty")
        assert default is not None and dark is not None and empty is not None
        assert "#1e1e1e" in default
        assert "#1e1e1e" in dark
        assert "#1e1e1e" not in empty
        with pytest.raises(strider.StriderError):
            # Deliberate: an unknown theme name is a runtime error.
            render(style="not_a_theme")  # type: ignore[arg-type]
