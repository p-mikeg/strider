"""`functions()` yields real functions, not every symbol.

The old spelling walked the whole `symbols()` dict and sorted it, so a kernel
with millions of symbols spent minutes there before lifting anything.
"""

import strider

from .conftest import fixture_path


def _lifter():
    return strider.lift.load_elf(str(fixture_path("x64", "switch")))


def test_functions_yields_symbol_records():
    rows = list(_lifter().functions())
    assert rows, "fixture defines functions"
    for sym in rows:
        assert isinstance(sym.name, str) and sym.name
        assert isinstance(sym.address, int) and sym.address > 0
        assert sym.size is None or sym.size > 0
        assert sym.is_function


def test_functions_addresses_match_symbol_lookup():
    lift = _lifter()
    for sym in lift.functions():
        assert lift.symbol(sym.name).address == sym.address


def test_functions_excludes_data_symbols():
    lift = _lifter()
    fn_names = {sym.name for sym in lift.functions()}
    all_names = set(lift.symbols())
    # `main` is code; `_edata`/`__bss_start` are data markers that `symbols()`
    # reports and `functions()` must not.
    assert "main" in fn_names
    assert fn_names < all_names
    for data_sym in ("_edata", "__bss_start", "_end"):
        if data_sym in all_names:
            assert data_sym not in fn_names


def test_iter_symbols_yields_records_lazily():
    """It is an iterator, not a materialised Python container: pulls advance a
    cursor, so the `Symbol` objects are never all live at once.  The Rust
    table IS collected up front; see `iter_symbols`.
    """
    lift = _lifter()
    it = lift.iter_symbols()
    assert iter(it) is it, "self-iterator, not a fresh view per `iter()`"
    total = len(it)
    assert total > 1

    first = next(it)
    assert isinstance(first.name, str) and isinstance(first.address, int)

    # A consumed pull is gone: `it` is a cursor, not a sequence handed out
    # whole on every read.
    rest = list(it)
    assert len(rest) == total - 1
    assert first.name not in {s.name for s in rest}
    assert list(it) == []

    assert {s.name for s in lift.iter_symbols()} == set(lift.symbols())


def test_known_targets_accepts_a_return():
    """A caller can seat a site as a return, not just as a target list.

    `push {lr} ... pop {lr}; bx lr` restores lr from the stack, so the resolver
    cannot prove the value is the incoming return address; the caller often can.
    """
    c = strider.cfg.CfgOptions(known_targets={0x1000: [0x2000], 0x1010: "return"})
    assert c.known_targets[0x1000] == [0x2000]
    assert c.known_targets[0x1010] == "return"


def test_known_targets_accepts_an_empty_target_list():
    """An empty answer means "nothing seats here", which the CFG builder already
    implements as deferring the site."""
    c = strider.cfg.CfgOptions(known_targets={0x1000: []})
    assert c.known_targets[0x1000] == []


def test_known_targets_rejects_an_unknown_string():
    import pytest

    with pytest.raises(ValueError):
        # Deliberate: only "return" is a legal string target.
        strider.cfg.CfgOptions(known_targets={0x1000: "nonsense"})  # type: ignore[dict-item]


def test_object_file_symbols_are_enumerated():
    """An object file's `st_value` is section-relative, so the first symbol of
    every section sits at 0.

    Skipping address 0 as a synthetic linker entry is right for a linked image
    and wrong here: it hid every such symbol, so `functions()` came back empty
    for a `.o` while `symbol(name)` resolved it fine.
    """
    import pathlib

    obj = pathlib.Path("fixtures/out/x64/tzcount.o")
    if not obj.exists():
        import pytest

        pytest.skip(f"fixture missing: {obj} (run `make CASE=tzcount` in fixtures/)")
    o = strider.lift.load_elf(str(obj))

    names = {sym.name for sym in o.functions()}
    assert {"tzcount", "main"} <= names, f"object-file functions() returned {names}"

    # Each resolves to the address `symbol()` reports, and the two are distinct
    # now that colliding sections are rebased apart.
    rows = {sym.name: sym.address for sym in o.functions()}
    assert rows["tzcount"] == o.symbol("tzcount").address
    assert rows["main"] == o.symbol("main").address
    assert rows["tzcount"] != rows["main"]

    # An undefined symbol has no address in either kind of file.
    assert "ext_fn" not in o.symbols()


def test_iter_symbols_len_is_what_is_left():
    """CPython takes `__len__` as a length hint, so `list(it)` on a partly
    consumed iterator over-allocates when it reports the total."""
    import operator

    lift = _lifter()
    it = lift.iter_symbols()
    total = len(it)
    next(it)
    assert len(it) == total - 1
    assert operator.length_hint(it) == total - 1
    for _ in it:
        pass
    assert len(it) == 0
