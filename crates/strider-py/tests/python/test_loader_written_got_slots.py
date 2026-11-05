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


def test_a_copy_relocated_array_element_is_not_folded():
    """`get3` reads `tbl[3]` from the PIE's `R_X86_64_COPY` object, which
    holds zeros in the file and the library's 4 at run time."""
    elf = strider.lift.load_elf(_image("x64_copy_pie.elf"), relocated_got_is_rom=True)
    function = elf.analyze("get3").function
    assert not function.find_all(p.ret().ret_val(0, p.int_const())), function.to_text()
    assert function.find_all(p.load()), function.to_text()


def test_a_call_through_an_android_packed_table_reaches_its_target():
    """`call1` tail-calls `ptrs[1]`, whose `R_AARCH64_RELATIVE` lld packed
    into an `SHT_ANDROID_RELA` table, decoded as bionic decodes it: the call
    folds to `g2`, where the file holds 0."""
    elf = strider.lift.load_elf(_image("aarch64_android_rela.elf"), relocated_got_is_rom=True)
    g2 = elf.symbol("g2").address
    text = elf.analyze("call1").function.to_text()
    assert "iconst[0x0]" not in text, text
    assert f"iconst[{g2:#x}]" in text, text


def test_a_textrel_image_without_section_headers_is_relocated_from_its_tags():
    """`llvm-objcopy --strip-sections` leaves only `PT_DYNAMIC` to place the
    `.rel.dyn` that relocates `getl`'s operand. It is walked, and with no
    `.dynsym` header to resolve `loc` the operand is a hole: `getl` no longer
    loads from address 0 and folds to the ELF magic."""
    twin = strider.lift.load_elf(_image("x86_textrel.elf"))
    getl = twin.symbol("getl").address
    elf = strider.lift.load_elf(_image("x86_textrel_noshdr.elf"))
    result = elf.analyze(getl)
    assert "iconst[0x464c457f]" not in result.function.to_text()
    assert result.cfg.incomplete, result.function.to_text()


@pytest.mark.parametrize(
    "image",
    ["x64_ifunc_nopic.o", "ppc64be_ifunc.o", "ppc64le_ifunc.o"],
)
def test_an_object_files_call_to_a_defined_ifunc_names_its_symbol(image):
    """An unlinked object calls its defined IFUNC `f` at an unmapped extern
    address, as it calls an undefined symbol; `f`'s symbol sits there, not at
    the resolver."""
    elf = strider.lift.load_elf(_image(image))
    f = elf.symbol("f")
    assert f.address != elf.symbol("resolve_f").address
    sym = elf.symbol_at(f.address)
    assert sym is not None and sym.name == "f"
    function = elf.analyze("call_f").function
    assert function.find_all(p.call().target(f.address)), function.to_text()


def test_an_object_files_defined_ifunc_covers_only_its_own_extern_address():
    """`f`'s extern address holds no bytes, so its symbol has no size and an
    address inside `tab`, the next extern, does not name it."""
    elf = strider.lift.load_elf(_image("x64_ifunc_extern.o"))
    f = elf.symbol("f")
    tab = elf.symbol("tab")
    assert f.size is None
    assert f.address < tab.address
    sym = elf.symbol_at(f.address)
    assert sym is not None and sym.name == "f"
    assert elf.symbol_at(tab.address + 8) is None
    function = elf.analyze("read_tab").function
    assert function.find_all(p.call().target(f.address)), function.to_text()
