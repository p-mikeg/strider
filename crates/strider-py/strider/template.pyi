"""Type stubs for strider.template: the build side of a rewrite.

`strider.pattern.Pat` is the match side (the `find`).
`strider.template.Template` is the build side (the `replace`): the node, op
and constant constructors plus `var(capture)`, with no `.when()`, no
commutativity toggle and no `.ordered()`, since those are match-only ideas
with no build-side meaning. `Function.rewrite(find, replace)` and
`rewrite_all` type `replace` as `Template`.
"""

from __future__ import annotations

from typing import Union

from .pattern import Capture, ExtendOpName, IntCmpOpName

class Template:
    """A type-checked expression describing what a rewrite builds.

    Construct one with the free functions below and pass it as `replace` to
    `Function.rewrite(find, replace)` or `Function.rewrite_all(pairs)`.
    """
    ...

#: A build-side operand accepts a `Template`, a bare `Capture` (sugar for
#: `var(c)`), or a string (interned to a `Capture`).
TemplateLike = Union[Template, Capture, str]

def var(c: Capture) -> Template:
    """Substitute the node bound to `c` on the matched left-hand side. The
    only build-side wildcard; there is no build-side `anything()`."""
def int_const(value: int) -> Template:
    """Build an integer constant whose stored value, masked to the output
    width, is `value`."""
def signed_int_const(value: int) -> Template:
    """Build a signed integer constant from a signed 64-bit `value`."""
def bool_const(value: bool) -> Template:
    """Build a 1-bit boolean constant equal to `value`."""
def float_const(bits: int) -> Template:
    """Build a float constant with raw bit pattern `bits`."""

def add(l: TemplateLike, r: TemplateLike) -> Template:
    """Build integer addition."""
def sub(l: TemplateLike, r: TemplateLike) -> Template:
    """Build integer subtraction, which lowers to `add(a, neg(b))`."""
def mul(l: TemplateLike, r: TemplateLike) -> Template:
    """Build integer multiplication."""
def div(l: TemplateLike, r: TemplateLike) -> Template:
    """Build unsigned integer division."""
def sdiv(l: TemplateLike, r: TemplateLike) -> Template:
    """Build signed integer division."""
def rem(l: TemplateLike, r: TemplateLike) -> Template:
    """Build unsigned integer remainder."""
def srem(l: TemplateLike, r: TemplateLike) -> Template:
    """Build signed integer remainder."""
def shl(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a left shift."""
def shr(l: TemplateLike, r: TemplateLike) -> Template:
    """Build a logical (zero-filling) right shift."""
def sshr(l: TemplateLike, r: TemplateLike) -> Template:
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

def neg(operand: TemplateLike) -> Template:
    """Build arithmetic negation (`-x`)."""
def int_not(operand: TemplateLike) -> Template:
    """Build bitwise complement (`~x`), which lowers to an XOR with all
    ones."""
def popcount(operand: TemplateLike) -> Template:
    """Build a set-bit count."""
def lzcount(operand: TemplateLike) -> Template:
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
    """Build float subtraction, which lowers to `float_add(a,
    float_neg(b))`."""
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

def truncate(operand: TemplateLike) -> Template:
    """Build a narrowing to the output width, keeping the low bits."""
def zero_extend(operand: TemplateLike) -> Template:
    """Build a widening that fills the new high bits with zero."""
def sign_extend(operand: TemplateLike) -> Template:
    """Build a widening that replicates the sign bit."""
def extend(op: ExtendOpName, operand: TemplateLike) -> Template:
    """Build a widening of the kind named by `op`."""
