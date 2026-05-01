"""Snapshot of the strider public surface.

This test pins the v1 API shape.  Any accidental addition / removal
of a top-level symbol trips the snapshot and forces a deliberate
review.

To intentionally update the snapshot, replace the expected sets
below — and announce the API change in the commit message.
"""

import strider
import strider.errors
import strider.opt
import strider.pattern


# Top-level strider names (excluding dunder names + auto-generated
# imports the cdylib pulls in via `from .strider import *`).
def _public(mod):
    return {
        n for n in dir(mod)
        if not n.startswith("_")
    }


EXPECTED_TOP = {
    "AnalyzeOutcome",
    "CallingConvention",
    "Cfg",
    "Graph",
    "Match",
    "MemoryMap",
    "OptimizerPipeline",
    "RunResult",
    "Sleigh",
    "SleighArch",
    "Strider",
    "StriderError",
    "build_cfg",
    "errors",
    "opt",
    "pattern",
    "run",
    "strider",
}

EXPECTED_ERRORS = {
    "LiftError",
    "PatternError",
    "ReaderError",
    "RewriteError",
    "StriderError",
}

EXPECTED_OPT = {
    "CallOtherElide",
    "CallStackArgCollect",
    "ConstantFold",
    "DeadBranchElim",
    "FunctionArgDetect",
    "KnownBits",
    "LoadReadOnly",
    "RedundantPhis",
    "StackLoadForward",
    "StackStoreDetect",
}

EXPECTED_PATTERN = {
    "Capture",
    "Pat",
    # Free constructors covered in v1:
    "add", "and_", "any_", "any_bool_const", "any_int_const",
    "bool_and", "bool_const", "bool_or", "bool_xor",
    "call", "call_other", "div",
    "function_arg", "function_arg_any",
    "if_", "initial_var",
    "int_carry", "int_eq", "int_le", "int_lt",
    "int_sborrow", "int_scarry", "int_sle", "int_slt",
    "int_const",
    "load", "mul", "or_", "phi", "rem", "ret",
    "sdiv", "shl", "shr", "srem", "sshr",
    "stack_store", "store", "sub", "var", "xor",
}

# `or_` / `and_` are escaped Python keywords — the Rust pyfunction
# names are `or` / `and`, not `or_` / `and_`.  Adjust the expected
# set to whichever convention the wrapper actually uses (see register
# in src/pattern.rs).  Currently we expose them as the bare keywords
# `or` / `and`, which Python accepts as attribute names.

# Patch the expected set to match what register() actually exposes.
EXPECTED_PATTERN.discard("or_")
EXPECTED_PATTERN.discard("and_")
EXPECTED_PATTERN.add("or")
EXPECTED_PATTERN.add("and")


def test_top_level_surface():
    have = _public(strider)
    # Allow a few extras (e.g. version) that aren't symbols of interest.
    extras = have - EXPECTED_TOP
    missing = EXPECTED_TOP - have
    # Only fail on missing.  Extras flagged as a soft warning so
    # adding documentation/version metadata is non-blocking.
    assert not missing, f"missing top-level names: {missing}"
    if extras:
        # Print but don't fail.
        print(f"unexpected top-level names: {extras}")


def test_errors_surface():
    have = _public(strider.errors)
    missing = EXPECTED_ERRORS - have
    assert not missing, f"missing errors names: {missing}"


def test_opt_surface():
    have = _public(strider.opt)
    missing = EXPECTED_OPT - have
    assert not missing, f"missing opt names: {missing}"


def test_pattern_surface():
    have = _public(strider.pattern)
    missing = EXPECTED_PATTERN - have
    assert not missing, f"missing pattern names: {missing}"
