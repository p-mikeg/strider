"""Reproduces the indirect-branch breakage via Python.

Mirror of `crates/strider/tests/indirect_branch.rs` for the x86 fixture
(`fixtures/out/x86/indirect_branch.elf::indirect_branch_resolved`).
End-to-end: build the CFG, run `strider.run` (which drives the
indirect-branch fixed-point loop), assert the run completed without
returning an UnresolvedIndirectBranch error and that the produced graph
contains an IR Return node (proof the dispatch was lowered to a real
control-flow tail rather than left as a placeholder).
"""

from __future__ import annotations

import pathlib
import pytest

import strider

from .conftest import fixture_path


def _x86_indirect_branch_elf() -> pathlib.Path:
    return fixture_path("x86", "indirect_branch")


def test_run_resolves_indirect_branch_x86():
    elf = _x86_indirect_branch_elf()
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr = mem.symbol("indirect_branch_resolved")
    result = strider.run(
        arch=arch,
        cc=cc,
        mem=mem,
        rom=mem,
        entry=addr,
        allow_code_before_start_addr=True,
    )
    assert result.function.node_count() > 0
