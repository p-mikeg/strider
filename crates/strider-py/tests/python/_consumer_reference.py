"""Phase 4 Task 4.0 — minimal Python consumer of the V2 reference type.

mypy --strict is run against this file by `test_reference_pyi.py`.
The test passes iff every method call below type-checks against the
hand-written `strider/pattern.pyi` (which is shape-identical to the
auto-generated `strider/_generated/strider/pattern.pyi` produced by
`cargo run --example stub_gen --features stub_gen --no-default-features`).
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture,
    PartialMatch,
    Pat,
    StackStorePatV2,
)


def _predicate(m: PartialMatch) -> bool:
    """Sample `.when(...)` predicate — exercises the PartialMatch proxy
    type that the V2 reference stores under `Mutex<Option<PyObject>>`
    and re-invokes at match time.  Mypy --strict checks both the
    callback's parameter type and the bool return type."""
    return "x" in m


def build_with_v2() -> None:
    """Smoke-build every method on the V2 reference and verify that
    each method returns a value usable by the next call in the chain.
    Failures here surface as mypy --strict errors, not Python runtime
    errors — we never call `.find_all(...)` since the V2 type isn't
    yet integrated into the `Graph.find_all` PatLike dispatch."""
    cap = Capture()
    p: StackStorePatV2 = strider.pattern.StackStorePatV2()
    p = p.offset(8)
    p = p.offset_any({0, 8, 16})
    p = p.data("x")  # PatLike accepts str
    p = p.data(cap)  # PatLike accepts Capture
    p = p.capture(cap)
    p = p.cap("base")
    p = p.when(_predicate)
    final: Pat = p.into_pat()
    # Use `final` so mypy keeps the annotation live.
    assert final is not None


def build_via_free_function() -> None:
    """The free-function constructor mirrors v1's `stack_store(...)`
    helper.  All args are keyword-optional; type-check each form."""
    a: StackStorePatV2 = strider.pattern.stack_store_v2()
    b: StackStorePatV2 = strider.pattern.stack_store_v2(offset=16)
    c: StackStorePatV2 = strider.pattern.stack_store_v2(data="addr")
    d: StackStorePatV2 = strider.pattern.stack_store_v2(offset=0, data="addr")
    assert a is not None and b is not None and c is not None and d is not None


if __name__ == "__main__":
    build_with_v2()
    build_via_free_function()
