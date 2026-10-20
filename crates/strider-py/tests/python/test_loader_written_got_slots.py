"""`load_elf(relocated_got_is_rom=True)` leaves unmapped a GOT slot whose
load-time value only the dynamic loader computes, so a load through it stays
opaque instead of folding to what the file holds.

The images are `fixtures/loader/out/*`, built by `make -C fixtures/loader`.
"""

from __future__ import annotations

import pytest
import strider

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
    text = elf.analyze("addr").function.to_text()
    returned = [line for line in text.splitlines() if line.startswith("return ")]
    assert returned, text
    value = returned[0].split(", ")[2]
    defined = next(line for line in text.splitlines() if line.startswith(f"{value}:"))
    assert " = load[r] " in defined, f"{image}: &ext_var is {defined}"


def test_a_mips_lazy_stub_is_not_the_call_target():
    """`ext_call`'s entry holds its lazy-binding stub, which the loader or the
    first call replaces: the call goes through the slot, not to the stub."""
    elf = strider.lift.load_elf(_image("mips32be_got.elf"), relocated_got_is_rom=True)
    stub = elf.symbol("ext_call").address
    text = elf.analyze("call_ext").function.to_text()
    assert f"iconst[{stub:#x}]" not in text, text
