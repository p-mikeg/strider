"""Type stubs for strider.template: the build side of a rewrite.

`strider.pattern.Pat` is the match side (the `find`); `Template` is the build
side (the `replace`): the node, op and constant constructors plus
`var(capture)`. `Function.rewrite(find, replace)` and `rewrite_all` type
`replace` as `Template`.
"""

from __future__ import annotations

from typing import Union

from .pattern import Capture, ExtendOpName, IntCmpOpName, Pat

__all__: list[str]

class Template:
    """A type-checked expression describing what a rewrite builds.

    Construct one with the free functions below and pass it as `replace` to
    `Function.rewrite(find, replace)` or `Function.rewrite_all(pairs)`.
    """
    ...

#: A build-side operand accepts a `Template`, a build-valid `Pat`, a raw `int`
#: (an `int_const`), or a bare `Capture` (sugar for `var(c)`).
TemplateLike = Union[Template, Pat, Capture, int]

def var(c: Capture) -> Template:
    """Substitute the node bound to `c` on the matched left-hand side. The
    only build-side wildcard."""
def int_const(value: int) -> Template:
    """Build an integer constant whose stored value, masked to the output
    width, is `value`. Negatives take the sign-extended form."""
def bool_const(value: bool) -> Template:
    """Build a 1-bit boolean constant equal to `value`."""
def float_const(bits: int) -> Template:
    """Build a float constant with raw bit pattern `bits`."""

def int_add(l: TemplateLike, r: TemplateLike) -> Template:
    """Build integer addition."""
def int_sub(l: TemplateLike, r: TemplateLike) -> Template:
    """Build integer subtraction."""
def int_mul(l: TemplateLike, r: TemplateLike) -> Template:
    """Build integer multiplication."""
def int_div(l: TemplateLike, r: TemplateLike) -> Template:
    """Build unsigned integer division."""
def int_sdiv(l: TemplateLike, r: TemplateLike) -> Template:
    """Build signed integer division."""
def int_rem(l: TemplateLike, r: TemplateLike) -> Template:
    """Build unsigned integer remainder."""
def int_srem(l: TemplateLike, r: TemplateLike) -> Template:
    """Build signed integer remainder."""
def int_shl(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a left shift."""
def int_shr(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a logical (zero-filling) right shift."""
def int_sshr(l: TemplateLike, r: TemplateLike) -> Template:
    """Build an arithmetic (sign-filling) right shift."""
def int_and(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a bitwise AND."""
def int_or(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a bitwise OR."""
def int_xor(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a bitwise XOR."""

def int_cmp(op: IntCmpOpName, l: TemplateLike, r: TemplateLike) -> Template:
    """Build a named integer comparison, e.g. `"Equal"`, `"Less"`,
    `"Sless"`, `"Carry"`, `"Scarry"`, `"Sborrow"`."""
def int_eq(l: TemplateLike, r: TemplateLike) -> Template:
    """Build an integer equality test."""
def int_lt(l: TemplateLike, r: TemplateLike) -> Template:
    """Build an unsigned less-than test."""
def int_slt(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a signed less-than test."""
def int_carry(l: TemplateLike, r: TemplateLike) -> Template:
    """Build an unsigned-addition carry test."""
def int_scarry(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a signed-addition overflow test."""
def int_sborrow(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a signed-subtraction overflow test."""

def int_neg(operand: TemplateLike) -> Template:
    """Build arithmetic negation (`-x`)."""
def int_not(operand: TemplateLike) -> Template:
    """Build bitwise complement (`~x`)."""
def int_popcount(operand: TemplateLike) -> Template:
    """Build a set-bit count."""
def int_lzcount(operand: TemplateLike) -> Template:
    """Build a leading-zero count."""

def bool_and(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a logical AND on 1-bit values."""
def bool_or(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a logical OR on 1-bit values."""
def bool_xor(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a logical XOR on 1-bit values."""
def bool_not(operand: TemplateLike) -> Template:
    """Build a logical NOT on a 1-bit value."""

def float_add(l: TemplateLike, r: TemplateLike) -> Template:
    """Build float addition."""
def float_sub(l: TemplateLike, r: TemplateLike) -> Template:
    """Build float subtraction."""
def float_mul(l: TemplateLike, r: TemplateLike) -> Template:
    """Build float multiplication."""
def float_div(l: TemplateLike, r: TemplateLike) -> Template:
    """Build float division."""
def float_neg(operand: TemplateLike) -> Template:
    """Build float negation."""
def float_abs(operand: TemplateLike) -> Template:
    """Build float absolute value."""
def float_sqrt(operand: TemplateLike) -> Template:
    """Build a float square root."""
def float_ceil(operand: TemplateLike) -> Template:
    """Build a round-toward-positive-infinity."""
def float_floor(operand: TemplateLike) -> Template:
    """Build a round-toward-negative-infinity."""
def float_round(operand: TemplateLike) -> Template:
    """Build a round-to-nearest."""
def float_eq(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a float equality test."""
def float_lt(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a float less-than test."""

def int_to_float(operand: TemplateLike) -> Template:
    """Build an integer-to-float numeric conversion."""
def float_to_int(operand: TemplateLike) -> Template:
    """Build a float-to-integer numeric conversion."""
def float_to_float(operand: TemplateLike) -> Template:
    """Build a float-to-float reprecision."""
def int_bits_to_float(operand: TemplateLike) -> Template:
    """Build a same-width reinterpretation of integer bits as a float."""
def float_bits_to_int(operand: TemplateLike) -> Template:
    """Build a same-width reinterpretation of float bits as an integer."""

def int_truncate(operand: TemplateLike) -> Template:
    """Build a narrowing to the output width, keeping the low bits."""
def int_zero_extend(operand: TemplateLike) -> Template:
    """Build a widening that fills the new high bits with zero."""
def int_sign_extend(operand: TemplateLike) -> Template:
    """Build a widening that replicates the sign bit."""
def int_extend(op: ExtendOpName, operand: TemplateLike) -> Template:
    """Build a widening of the kind named by `op`."""
