"""strider.idioms — the query-side convenience wrappers.

Each wrapper enumerates a fixed set of encodings; the tests below pin both what
they DO cover and (for DirectScale) what they deliberately do not.
"""

import strider
from strider import pattern as p
from strider.idioms import DirectScale, OptionalOffset


def _fn(code: bytes):
    mem = strider.BufferReader(0x1000, code)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _cfg, fn, _u = lift.analyze(0x1000, strider.CallingConvention.x86_64_systemv())
    return fn


# ── OptionalOffset ────────────────────────────────────────────────────────────


def test_optional_offset_load_covers_bare_and_offset_forms():
    # mov rax,[rdi] ; mov rdx,[rsi+8] ; add rax,rdx ; ret
    fn = _fn(bytes([0x48, 0x8B, 0x07, 0x48, 0x8B, 0x56, 0x08, 0x48, 0x01, 0xD0, 0xC3]))
    oo = OptionalOffset()
    offsets = sorted(oo.offset(h) for h in fn.find_all(p.load(addr=oo.addr())))
    # The bare load reports 0; the +8 load reports 8 — NOT 0 (the ordering trap:
    # a bare wildcard also matches the Add, so the bare arm must be tried last).
    assert offsets == [0, 8]


def test_optional_offset_works_for_store_too():
    """The wrapper is on the ADDRESS, so it needs no per-node-kind variant."""
    # mov [rdi],rax ; mov [rsi+8],rax ; ret
    fn = _fn(bytes([0x48, 0x89, 0x07, 0x48, 0x89, 0x46, 0x08, 0xC3]))
    oo = OptionalOffset()
    hits = fn.find_all(p.store(addr=oo.addr(), data=p.anything()))
    assert sorted(oo.offset(h) for h in hits) == [0, 8]


def test_optional_offset_binds_the_base():
    fn = _fn(bytes([0x48, 0x8B, 0x56, 0x08, 0xC3]))  # mov rdx,[rsi+8] ; ret
    oo = OptionalOffset()
    hits = fn.find_all(p.load(addr=oo.addr()))
    assert len(hits) == 1
    assert oo.offset(hits[0]) == 8
    assert oo.base(hits[0]) is not None, "the base sub-expression binds out"


def test_optional_offset_accepts_a_caller_supplied_base():
    """A constrained base still gets the optional-offset treatment."""
    fn = _fn(bytes([0x48, 0x8B, 0x56, 0x08, 0xC3]))
    oo = OptionalOffset()
    hits = fn.find_all(p.load(addr=oo.addr(base=p.anything())))
    assert len(hits) == 1
    assert oo.offset(hits[0]) == 8


# ── DirectScale ───────────────────────────────────────────────────────────────


def test_direct_scale_normalises_mul_and_shl_to_a_multiplier():
    # mov rax,rdi ; shl rax,3 ; imul rdx,rsi,12 ; add rax,rdx ; ret
    fn = _fn(
        bytes(
            [0x48, 0x89, 0xF8, 0x48, 0xC1, 0xE0, 0x03,
             0x48, 0x6B, 0xD6, 0x0C, 0x48, 0x01, 0xD0, 0xC3]
        )
    )
    ds = DirectScale()
    scales = sorted(ds.scale(h) for h in fn.find_all(ds.of()))
    # shl 3 normalises to 8 (not 3); imul 12 stays 12.
    assert scales == [8, 12]


def test_direct_scale_can_constrain_the_scaled_operand():
    fn = _fn(bytes([0x48, 0x6B, 0xC7, 0x0C, 0xC3]))  # imul rax,rdi,12 ; ret
    ds = DirectScale()
    assert [ds.scale(h) for h in fn.find_all(ds.of(p.anything()))] == [12]


def test_direct_scale_does_not_see_lea_composed_multiplies():
    """PINNED LIMITATION, not a bug.

    `lea rax,[rdi+rdi*2]` is rdi*3, but lifts to add(x, mul(x, 2)) — the ×3 is
    distributed across the Add, and no node holds 3.  DirectScale reports the
    SIB scale (2).  Documented on the class; pinned here so a future change to
    this behaviour is a deliberate decision rather than a surprise.
    """
    fn = _fn(bytes([0x48, 0x8D, 0x04, 0x7F, 0xC3]))  # lea rax,[rdi+rdi*2] ; ret
    ds = DirectScale()
    scales = [ds.scale(h) for h in fn.find_all(ds.of())]
    assert scales == [2], "reports the SIB scale, NOT the true ×3 multiplier"


def test_direct_scale_scale_is_none_when_unbound():
    """A Match from an unrelated pattern yields None rather than raising."""
    fn = _fn(bytes([0x48, 0x8B, 0x07, 0xC3]))  # mov rax,[rdi] ; ret
    ds = DirectScale()
    assert fn.find_all(ds.of()) == []
