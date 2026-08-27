"""The `Symbol` record: `symbol` / `symbol_opt` / `symbol_at` / `symbols` /
`iter_symbols` / `functions` all hand back the same shape."""

import pytest

import strider

from .conftest import fixture_path


def _switch():
    return strider.lift.load_elf(str(fixture_path("x64", "switch")))


def test_symbol_carries_name_address_and_extent():
    lift = _switch()
    sym = lift.symbol("f")
    assert sym.name == "f"
    assert sym.address > 0
    assert sym.size == 8
    assert sym.end == sym.address + 8
    assert sym.is_function is True


def test_symbol_region_is_the_containing_mapped_region():
    lift = _switch()
    sym = lift.symbol("f")
    assert sym.region is not None
    start, end = sym.region
    assert start <= sym.address < end


def test_every_symbol_region_brackets_its_address():
    for sym in _switch().iter_symbols():
        if sym.region is not None:
            assert sym.region[0] <= sym.address < sym.region[1]


def test_size_is_none_when_the_elf_records_no_extent():
    """`st_size == 0` is "never recorded", not "zero bytes long"."""
    lift = _switch()
    sizeless = [s for s in lift.iter_symbols() if s.size is None]
    assert sizeless, "the fixture has crt symbols with no .size directive"
    for s in sizeless:
        assert s.end is None
    for s in lift.iter_symbols():
        assert s.size is None or s.size > 0


def test_symbol_raises_for_undefined():
    with pytest.raises(strider.StriderError):
        _switch().symbol("no_such_symbol_zzz")


def test_symbol_opt_returns_none_for_undefined():
    lift = _switch()
    assert lift.symbol_opt("no_such_symbol_zzz") is None
    f = lift.symbol_opt("f")
    assert f is not None
    assert f.address == lift.symbol("f").address


def test_functions_keeps_symbols_with_no_recorded_size():
    """A hand-written `.S` entry point carries no `.size`, and dropping it
    would hide a real, analysable function."""
    names = {s.name: s for s in _switch().functions()}
    assert "frame_dummy" in names, "a zero-st_size FUNC is still a function"
    assert names["frame_dummy"].size is None
    assert names["f"].size == 8


def test_functions_are_functions_in_address_order():
    rows = list(_switch().functions())
    assert rows
    assert all(s.is_function for s in rows)
    assert [s.address for s in rows] == sorted(s.address for s in rows)
    # A data marker `symbols()` reports is not a function.
    assert "_edata" not in {s.name for s in rows}
    assert "_edata" in _switch().symbols()


def test_functions_excludes_undefined_symbols():
    """A UND `FUNC` (`__libc_start_main`) has no address of its own."""
    assert "__libc_start_main" not in {s.name for s in _switch().functions()}


def test_symbol_at_matches_inside_a_sized_symbol():
    lift = _switch()
    f = lift.symbol("f")
    assert f.size is not None and f.end is not None
    at_start = lift.symbol_at(f.address)
    at_last = lift.symbol_at(f.address + f.size - 1)
    assert at_start is not None and at_start.name == "f"
    assert at_last is not None and at_last.name == "f"
    # `end` is exclusive, and the next function starts further along.
    assert lift.symbol_at(f.end) is None


def test_symbol_at_needs_an_exact_hit_without_a_size():
    lift = _switch()
    sizeless = lift.symbol("frame_dummy")
    assert sizeless.size is None
    at_exact = lift.symbol_at(sizeless.address)
    assert at_exact is not None and at_exact.name == "frame_dummy"
    assert lift.symbol_at(sizeless.address + 1) is None


def test_symbol_at_returns_none_below_every_symbol():
    assert _switch().symbol_at(0) is None


def test_symbols_maps_name_to_record():
    lift = _switch()
    syms = lift.symbols()
    assert syms["f"].address == lift.symbol("f").address
    assert {s.name for s in lift.iter_symbols()} == set(syms)


def test_analyze_bounds_a_named_target_by_the_recorded_size():
    """`analyze(name)` derives `function_max_size` from `Symbol.size`, which is
    what tells the indirect-branch resolver an intra-function jump from a tail
    call."""
    lift = _switch()
    _cfg, function, _unresolved = lift.analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
        ),
    )
    assert function.node_count() > 0


def test_an_explicit_bound_wins_over_the_recorded_size():
    lift = _switch()
    with pytest.raises(strider.StriderError) as exc:
        lift.analyze(
            "dispatch_value",
            opts=strider.lift.LifterOptions(
                cfg=strider.cfg.CfgOptions(function_max_size=4)
            ),
        )
    assert "function-boundary error" in str(exc.value)


def test_symbol_at_sees_past_a_sizeless_symbol_inside_a_function():
    """ARM literal pools put size-0 mapping symbols inside sized functions, so
    the nearest symbol at or below an address is routinely not the covering
    one."""
    lift = strider.lift.load_elf(str(fixture_path("arm", "arithmetic")))
    div = lift.symbol("__divsi3")
    assert div.size is not None and div.size > 0
    end = div.address + div.size
    inner = [
        s
        for s in lift.iter_symbols()
        if div.address < s.address < end and s.size is None
    ]
    assert inner, "the fixture must contain a sizeless symbol inside __divsi3"
    probe = max(s.address for s in inner) + 1
    hit = lift.symbol_at(probe)
    assert hit is not None and hit.name == "__divsi3"


def test_symbol_at_and_functions_agree_on_an_aliased_address():
    """Two code symbols at one address: the one carrying the extent wins in
    both accessors."""
    lift = strider.lift.load_elf(str(fixture_path("arm", "arithmetic")))
    div = lift.symbol("__divsi3")
    aliases = [s for s in lift.iter_symbols() if s.address == div.address]
    assert len(aliases) > 1, "the fixture must alias __divsi3"
    from_functions = [s for s in lift.functions() if s.address == div.address]
    assert len(from_functions) == 1
    at = lift.symbol_at(div.address)
    assert at is not None and at.name == from_functions[0].name


def test_iter_symbols_does_not_repeat_a_symbol_in_both_tables():
    """An exported symbol is in `.symtab` and `.dynsym` alike."""
    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    seen = [(s.name, s.address) for s in lift.iter_symbols()]
    assert len(seen) == len(set(seen))
