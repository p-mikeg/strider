"""Phase 4 Task 4.1 — verify the `#[strider_pattern]` proc-macro
emits a `.pyi` stub byte-identical (modulo the class-name token) to
the V2 reference's `.pyi` stub.

This is the smoking-gun test for the macro: any divergence between
the V2 hand-written reference (`pattern_reference.rs`) and the V3
macro-emitted form (`pattern_macro_v2.rs`) is a macro bug.  Run
`cargo run -p strider-py --example stub_gen --features stub_gen
--no-default-features` to regenerate the stubs before this test;
the regenerated `pattern.pyi` lives at
`strider/_generated/strider/pattern.pyi`.

If this test fails, do NOT relax the assertion — fix the macro in
`crates/strider-pattern-macros/src/lib.rs` to bring V3's emission
back in line with V2's reference shape.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

# `strider/_generated/` is gitignored; the test only runs when the
# stub-gen example has been executed.  If the file is missing we
# skip rather than fail so a clean checkout can still pytest cleanly.
GENERATED_PATTERN_PYI = (
    Path(__file__).resolve().parents[2]
    / "strider"
    / "_generated"
    / "strider"
    / "pattern.pyi"
)


def _extract_class_block(text: str, class_name: str) -> str:
    """Pull `class <name>: ...\\n` block out of a .pyi file.  Reads
    every line until the next `class ` line or end-of-file.  Returns
    the block with trailing blank lines stripped."""
    pattern = rf"class {class_name}:.*?(?=\nclass |\Z)"
    m = re.search(pattern, text, re.DOTALL)
    if m is None:
        raise AssertionError(
            f"class {class_name!r} not found in generated stub at "
            f"{GENERATED_PATTERN_PYI}"
        )
    return m.group().rstrip()


def _normalise_class_name(block: str, name: str) -> str:
    """Replace the class-name token on the class def line AND in
    return-type arrows (`-> Name`), but NOT in docstring prose.
    The V2 / V3 forms are byte-identical EXCEPT for these two
    structural tokens; everything else (including the prose
    `strider.pattern.StackStorePatV2` mention inside the docstring,
    since V3 re-uses V2's verbatim docstring during the migration)
    must match exactly."""
    out_lines: list[str] = []
    for line in block.splitlines():
        line = re.sub(rf"^(class ){name}(:)", r"\1XXX\2", line)
        line = re.sub(rf"(-> ){name}(:)", r"\1XXX\2", line)
        out_lines.append(line)
    return "\n".join(out_lines)


@pytest.mark.skipif(
    not GENERATED_PATTERN_PYI.exists(),
    reason=(
        "generated stubs missing; run `cargo run -p strider-py "
        "--example stub_gen --features stub_gen --no-default-features` "
        "to populate strider/_generated/"
    ),
)
def test_v3_macro_emission_matches_v2_reference_byte_for_byte() -> None:
    """The V3 macro-emitted stub must be byte-identical to the V2
    hand-written reference modulo the class-name token.  If this
    fails, fix `#[strider_pattern]` — don't relax the test."""
    text = GENERATED_PATTERN_PYI.read_text()
    v2 = _extract_class_block(text, "StackStorePatV2")
    v3 = _extract_class_block(text, "StackStorePatV3")

    v2_norm = _normalise_class_name(v2, "StackStorePatV2")
    v3_norm = _normalise_class_name(v3, "StackStorePatV3")

    if v2_norm != v3_norm:
        # Generate a helpful unified diff so the failure message
        # points the developer at the exact shape regression.
        import difflib

        diff_lines = list(
            difflib.unified_diff(
                v2_norm.splitlines(),
                v3_norm.splitlines(),
                fromfile="V2 (hand-written reference)",
                tofile="V3 (macro-emitted)",
                lineterm="",
            )
        )
        msg = "\n".join(diff_lines)
        raise AssertionError(
            "V3 macro emission diverged from V2 reference; fix the "
            "proc-macro in crates/strider-pattern-macros/src/lib.rs:\n"
            f"{msg}"
        )
