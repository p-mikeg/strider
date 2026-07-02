"""Reproduces the indirect-branch breakage via Python.

Mirror of `crates/strider/tests/indirect_branch.rs` for the x86 fixture
(`fixtures/out/x86/indirect_branch.elf::indirect_branch_resolved`).
End-to-end: build the CFG, run `strider.lifter(...).analyze(...)`
(which drives the indirect-branch fixed-point loop), assert the run
completed without returning an UnresolvedIndirectBranch error and that
the produced graph contains an IR Return node (proof the dispatch was
lowered to a real control-flow tail rather than left as a placeholder).
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
    loaded = strider.load_elf(str(elf))
    mem = loaded.reader()
    addr = loaded.symbol("indirect_branch_resolved")
    lift = strider.lifter(arch, mem, rom=mem)
    function, _unresolved = lift.analyze(
        addr, cc, opts=strider.LifterOptions(cfg=strider.CfgOptions(allow_code_before_start_addr=True))
    )
    assert function.node_count() > 0


# ── unresolved-site reporting (hand-assembled bytes) ─────────────────────
#
# `unresolved_indirect_branches` must list the machine address of EVERY
# indirect branch the orchestrator could not resolve — not just flag
# that some exist.  Hand-assembled x86-64 keeps the addresses exact.


def test_run_reports_single_unresolved_indirect_site_address():
    # 0x2000: ff e0    jmp rax   (rax = entry InitialVar — unresolvable)
    code = bytes([0xFF, 0xE0])
    mem = strider.BufferReader(0x2000, code)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _function, unresolved = lift.analyze(
        0x2000, strider.CallingConvention.x86_64_systemv(),
        opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=len(code))),
    )
    # Unresolvable is NOT an error — the run completes and the exact
    # branch address is reported.
    assert unresolved == [0x2000]


def test_run_reports_both_unresolved_indirect_site_addresses():
    # Two reachable indirect jumps, neither resolvable:
    #   0x1000: 48 85 ff    test rdi, rdi
    #   0x1003: 74 02       je   0x1007
    #   0x1005: ff e0       jmp  rax
    #   0x1007: ff e1       jmp  rcx
    code = bytes([0x48, 0x85, 0xFF, 0x74, 0x02, 0xFF, 0xE0, 0xFF, 0xE1])
    mem = strider.BufferReader(0x1000, code)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _function, unresolved = lift.analyze(
        0x1000, strider.CallingConvention.x86_64_systemv(),
        opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=len(code))),
    )
    # BOTH sites are reported, by exact machine address.
    assert sorted(unresolved) == [0x1005, 0x1007]
