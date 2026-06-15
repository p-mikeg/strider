"""The FunctionArgDetect alias tuning knobs must be reachable from the
high-level `analyze()` surface.

`FunctionArgsOptions` carries two real optimizer knobs
(`args_assume_distinct_sp_bases_disjoint` and
`calls_clobber_stack_arguments`) that were fully plumbed through the
Rust alias core but hardcoded to the `OptOptions` default at the Python
boundary, so no Python caller could ever set them.  These tests pin them
as settable keyword arguments.
"""

from __future__ import annotations

import strider

from .conftest import fixture_path


def test_analyze_accepts_function_args_alias_knobs():
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze(
        "add",
        args_assume_distinct_sp_bases_disjoint=True,
        calls_clobber_stack_arguments=True,
    )
    assert isinstance(a, strider.Analysis)
    assert a.function.node_count() > 0


def test_function_args_knobs_default_off_still_analyzes():
    """Omitting the knobs keeps the previous default behaviour."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    assert a.function.node_count() > 0
