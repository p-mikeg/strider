"""`phi().any_input(p)` composes with the query-level cast-transparency
controls (`ignore_casts` / `ignore_casts_mask`).

Cast-transparency is a QUERY-level flag, not a per-operand one: it applies to
the whole pattern, and existential `any_input` candidates go through the same
operand path as fixed slots. These tests pin that an extend-wrapped phi arm
(the mips64/thumb "widen before merging" shape) is reachable without
spelling the extend out.
"""

import strider
from strider import pattern as p


def _phi_of_extended_loads(second_arm):
    #   test edi, edi
    #   jne  else
    #   movsx eax, byte [rdi]      -> SignExtend(Load(rdi))
    #   jmp  end
    # else:
    #   <second_arm>               -> SignExtend / ZeroExtend of Load(rsi)
    # end:
    #   ret                        -> Phi(SignExtend(Load), <second_arm>)
    code = bytes([
        0x85, 0xff,              # test edi, edi
        0x75, 0x05,              # jne +5
        0x0f, 0xbe, 0x07,        # movsx eax, byte [rdi]
        0xeb, 0x03,              # jmp +3
    ]) + second_arm + bytes([
        0xc3,                    # ret
    ])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    return fn


_MOVSX_RSI = bytes([0x0f, 0xbe, 0x06])  # movsx eax, byte [rsi]
_MOVZX_RSI = bytes([0x0f, 0xb6, 0x06])  # movzx eax, byte [rsi]


def test_plain_any_input_misses_the_extend_wrapped_arm():
    """Baseline: without cast-transparency the extend sits between the phi
    and the load, so the bare `any_input(load())` correctly misses."""
    fn = _phi_of_extended_loads(_MOVSX_RSI)
    assert len(fn.find_all(p.phi())) >= 1
    assert fn.find_all(p.phi().any_input(p.load())) == []


def test_any_input_finds_the_extend_wrapped_arm_under_ignore_casts():
    fn = _phi_of_extended_loads(_MOVSX_RSI)
    assert len(fn.find_all(p.phi().any_input(p.load()), ignore_casts=True)) >= 1


def test_any_input_ignore_casts_spans_mixed_sign_and_zero_extends():
    """The two arms need not agree on the extend flavour (mips64/thumb gcc
    widens before merging, not always with the same sign)."""
    fn = _phi_of_extended_loads(_MOVZX_RSI)
    assert len(fn.find_all(p.phi().any_input(p.load()), ignore_casts=True)) >= 1


def test_any_input_honours_the_granular_cast_mask():
    fn = _phi_of_extended_loads(_MOVSX_RSI)
    mask = p.CastMask.extend()
    assert len(fn.find_all(p.phi().any_input(p.load()), ignore_casts_mask=mask)) >= 1
    # A mask that does not include `extend` must not peel the extend.
    assert fn.find_all(p.phi().any_input(p.load()), ignore_casts_mask=p.CastMask.truncate()) == []


def test_any_input_captures_bind_through_the_peeled_cast():
    fn = _phi_of_extended_loads(_MOVSX_RSI)
    c = p.Capture()
    hits = fn.find_all(p.phi().any_input(p.load().capture(c)), ignore_casts=True)
    assert len(hits) >= 1
    assert hits[0].node(c) is not None
