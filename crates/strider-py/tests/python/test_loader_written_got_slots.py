"""`load_elf(relocated_got_is_rom=True)` leaves unmapped a GOT slot whose
load-time value only the dynamic loader computes, so a load through it stays
opaque instead of folding to what the file holds.

The images are `fixtures/loader/out/*`, built by `make -C fixtures/loader`.
"""

from __future__ import annotations

import pytest
import strider
import strider.pattern as p

from .conftest import WORKSPACE_ROOT

LOADER_OUT = WORKSPACE_ROOT / "fixtures" / "loader" / "out"


def _image(name: str) -> str:
    p = LOADER_OUT / name
    if not p.exists():
        pytest.fail(f"fixture missing: {p} (run `make -C fixtures/loader`)")
    with p.open("rb") as f:
        if f.read(42) == b"version https://git-lfs.github.com/spec/v1":
            pytest.fail(f"{p} is a Git LFS pointer; run `git lfs pull`")
    return str(p)


@pytest.mark.parametrize("image", ["mips32be_got.elf", "mips64le_got.elf"])
def test_a_mips_imported_symbols_address_is_not_folded_to_null(image):
    """No relocation covers a MIPS global GOT entry: the loader fills it from
    `.dynsym`. For an imported `ext_var` the file holds 0, which `&ext_var`
    folded to."""
    elf = strider.lift.load_elf(_image(image), relocated_got_is_rom=True)
    function = elf.analyze("addr").function
    assert not function.find_all(p.ret().ret_val(0, p.int_const())), function.to_text()
    assert function.find_all(p.ret().ret_val(0, p.load())), function.to_text()


def test_a_mips_lazy_stub_is_not_the_call_target():
    """`ext_call`'s entry holds its lazy-binding stub, which the loader or the
    first call replaces: the call goes through the slot, not to the stub."""
    elf = strider.lift.load_elf(_image("mips32be_got.elf"), relocated_got_is_rom=True)
    stub = elf.symbol("ext_call").address
    text = elf.analyze("call_ext").function.to_text()
    assert f"iconst[{stub:#x}]" not in text, text


@pytest.mark.parametrize(
    "image",
    ["x64_ifunc_pie.elf", "x64_ifunc_lib.elf", "x64_ifunc_static.elf"],
    ids=["irelative", "glob_dat", "static"],
)
def test_a_tail_call_through_an_ifunc_slot_is_unresolved(image):
    """`call_f` is `jmp *slot`, the slot an IFUNC's: an `R_X86_64_IRELATIVE`
    in the PIE and in the unstripped static executable, whose `.rela.plt`
    links to `.symtab`, and a `GLOB_DAT` against the IFUNC symbol in the
    library. The file names the resolver there, or in the static executable
    holds 0; the loader stores what the resolver returns."""
    elf = strider.lift.load_elf(_image(image), relocated_got_is_rom=True)
    resolver = elf.symbol("f").address
    result = elf.analyze("call_f")
    assert result.unresolved, "the jump's target is the resolver's result"
    assert f"iconst[{resolver:#x}]" not in result.function.to_text()


def test_a_tail_call_through_a_plain_slot_still_folds():
    """`call_h`'s `GLOB_DAT` names a plain function, whose slot is served."""
    elf = strider.lift.load_elf(_image("x64_ifunc_lib.elf"), relocated_got_is_rom=True)
    h = elf.symbol("h").address
    result = elf.analyze("call_h")
    assert not result.unresolved
    assert f"iconst[{h:#x}]" in result.function.to_text()
