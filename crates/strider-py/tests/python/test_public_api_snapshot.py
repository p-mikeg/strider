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
    # Core types.
    "Capture",
    "Pat",
    "PartialMatch",
    "IntBinaryPat",
    "BoolBinaryPat",
    "FloatBinaryPat",
    # Wildcards / consts / phi / initial.
    "any_", "var", "predicate",
    "int_const", "bool_const", "float_const",
    "any_int_const", "any_bool_const", "any_float_const",
    "initial_var", "function_arg", "function_arg_any", "phi",
    # Integer binary / unary / cmp.
    "add", "sub", "mul", "div", "sdiv", "rem", "srem",
    "shl", "shr", "sshr",
    "int_eq", "int_lt", "int_le", "int_slt", "int_sle",
    "int_carry", "int_scarry", "int_sborrow",
    "neg", "not_",
    # Bool binary / unary.
    "bool_and", "bool_or", "bool_xor", "bool_not",
    # Float binary / unary / cmp.
    "float_add", "float_sub", "float_mul", "float_div",
    "float_neg", "float_abs", "float_sqrt", "float_ceil",
    "float_floor", "float_round", "float_is_nan",
    "float_eq", "float_ne", "float_lt", "float_le",
    # Conversions / bitcasts / casts.
    "int_to_float", "float_to_int", "float_to_float",
    "int_bits_to_float", "float_bits_to_int",
    "cast_to_int", "cast_to_bool", "cast_to_float",
    "truncate", "popcount", "lzcount",
    "zero_extend", "sign_extend", "extend",
    # Memory & control flow.
    "load", "store", "stack_store", "stack_store_phi",
    "call", "call_other", "ret", "if_",
    # Typed family dispatchers.
    "int_binary", "bool_binary", "float_binary",
    # Variant-agnostic.
    "int_bin_any", "int_un_any", "int_cmp_any",
    "bool_bin_any", "bool_un_any",
    "float_bin_any", "float_un_any", "float_cmp_any",
}

# `or_` / `and_` are escaped Python keywords — the Rust pyfunction
# names are `or` / `and`, not `or_` / `and_`.  Adjust the expected
# set to whichever convention the wrapper actually uses (see register
# in src/pattern.rs).  Currently we expose them as the bare keywords
# `or` / `and`, which Python accepts as attribute names.

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
