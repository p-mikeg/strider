"""Python-side mirror of `crates/strider-orchestrator/tests/indirect_branch.rs`
over the x86 `indirect_branch.elf::indirect_branch_resolved` fixture."""

from __future__ import annotations

import pathlib
import pytest

import strider

from .conftest import fixture_path


def _x86_indirect_branch_elf() -> pathlib.Path:
    return fixture_path("x86", "indirect_branch")


def test_run_resolves_indirect_branch_x86():
    elf = _x86_indirect_branch_elf()
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    addr = loaded.symbol("indirect_branch_resolved").address
    lift = strider.lift.lifter(arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    assert function.node_count() > 0


def test_run_reports_single_unresolved_indirect_site_address():
    # Every unresolvable site must be reported by exact address, not just
    # flagged as existing; hand-assembled bytes keep the addresses exact.
    # 0x2000: ff e0    jmp rax   (rax = entry InitialVar, unresolvable)
    code = bytes([0xFF, 0xE0])
    mem = strider.reader.BufferReader(0x2000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, _function, unresolved = lift.analyze(
        0x2000, strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=len(code))),
    )
    # Unresolvable is not an error: the run completes and reports.
    assert unresolved == [0x2000]


def test_run_reports_both_unresolved_indirect_site_addresses():
    # Two reachable indirect jumps, neither resolvable:
    #   0x1000: 48 85 ff    test rdi, rdi
    #   0x1003: 74 02       je   0x1007
    #   0x1005: ff e0       jmp  rax
    #   0x1007: ff e1       jmp  rcx
    code = bytes([0x48, 0x85, 0xFF, 0x74, 0x02, 0xFF, 0xE0, 0xFF, 0xE1])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, _function, unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=len(code))),
    )
    assert sorted(unresolved) == [0x1005, 0x1007]
