"""`any_input` matches SOME input of a node regardless of kind: a memory
producer (`store` / `mem_phi`) now matches a memory input, not only value
producers. Regression: `mem_phi().any_input(store())` used to raise
"expected a value pattern"."""

import strider
from strider import pattern as p
from .conftest import lift_bytes as _lift


def test_construction_accepts_memory_producer():
    assert p.mem_phi().any_input(p.store(addr=p.anything(), data=p.anything())).into_pat() is not None
    assert p.call().any_input(p.store(addr=p.anything(), data=p.anything())).into_pat() is not None


def test_any_input_matches_a_mem_phi_store_input():
    # test edi,edi ; je +2 ; mov [rsi],eax (store on one arm) ; mov eax,[rdx] ; ret
    fn = _lift(bytes([0x85, 0xFF, 0x74, 0x02, 0x89, 0x06, 0x8B, 0x02, 0xC3]))
    assert len(fn.find_all(p.mem_phi())) == 1
    hits = fn.find_all(
        p.mem_phi().any_input(p.store(addr=p.anything(), data=p.anything()))
    )
    assert len(hits) == 1
