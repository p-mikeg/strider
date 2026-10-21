"""`assume_incoming_args_survive_calls` covers a callee that was not handed a
pointer into the incoming-argument area. Where the function hands one over,
`h(&a); return a;`, the reload after the call is not the argument any more;
a pointer to a local leaves the claim in force.

Each body sits at 0x1000 and calls 0x2000.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import function_arg
from strider.reader import BufferReader
from strider.sleigh import CallingConvention, SleighArch

CASES = {
    # lea eax,[esp+4]; push eax; call h; add esp,4; mov eax,[esp+4]; ret
    "i386_arg_address": ("x86", "8d44240450e8f60f000083c4048b442404c3", 0, 0),
    # The address only in the pushed argument:
    # lea eax,[esp+4]; push eax; mov eax,0; call h; add esp,4;
    # mov eax,[esp+4]; ret
    "i386_arg_address_pushed": (
        "x86",
        "8d44240450b800000000e8f10f000083c4048b442404c3",
        0,
        0,
    ),
    # The address only in a register the call clobbers (a regparm callee):
    # lea eax,[esp+4]; call h; mov eax,[esp+4]; ret
    "i386_arg_address_in_register": ("x86", "8d442404e8f70f00008b442404c3", 0, 0),
    # g(a1..a6, a7) { h(&a7); return a7; }:
    # sub rsp,8; lea rdi,[rsp+16]; call h; mov eax,[rsp+16]; add rsp,8; ret
    "x64_arg_address": (
        "x86_64",
        "4883ec08488d7c2410e8f20f00008b4424104883c408c3",
        6,
        0,
    ),
    # sub esp,8; lea eax,[esp]; push eax; call h; add esp,4;
    # mov eax,[esp+12]; add esp,8; ret
    "i386_local_address": (
        "x86",
        "83ec088d042450e8f40f000083c4048b44240c83c408c3",
        0,
        1,
    ),
    # The same in a loop, so the SP at the call is a phi:
    # push ebx; sub esp,8; mov ebx,4; 1: lea eax,[esp]; push eax; call h;
    # add esp,4; dec ebx; jnz 1b; mov eax,[esp+16]; add esp,8; pop ebx; ret
    "i386_local_address_in_loop": (
        "x86",
        "5383ec08bb040000008d042450e8ee0f000083c4044b75f18b44241083c4085bc3",
        0,
        1,
    ),
}

_ARCH = {
    "x86": (SleighArch.x86, CallingConvention.x86_cdecl),
    "x86_64": (SleighArch.x86_64, CallingConvention.x86_64_systemv),
}


@pytest.mark.parametrize("name", sorted(CASES))
def test_reload_after_handing_out_an_argument_address(name):
    arch, code, index, carriers = CASES[name]
    sleigh_arch, cc = _ARCH[arch]
    result = strider.lift.lifter(
        sleigh_arch(), BufferReader(0x1000, bytes.fromhex(code))
    ).analyze(0x1000, cc())
    assert not result.cfg.incomplete
    assert len(result.function.find_all(function_arg(index))) == carriers
