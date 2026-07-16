"""Query-side convenience wrappers over ``strider.pattern``.

Pure Python composed from the stable pattern API — nothing here is a new IR or
matcher concept, and anything these do you can write by hand.

**Each wrapper enumerates a FIXED set of encodings and is deliberately
partial.** They exist to absorb the encodings that are easy to get silently
wrong by hand, not to be exhaustive. Where a wrapper's coverage has a known
edge, it is named on the class and pinned by a test — read those before
trusting a result. When a query needs the *semantic* answer over an arbitrary
expression rather than a fixed shape, a pattern is the wrong tool.
"""

from .pattern import Capture, add, any_int_const, anything, mul, one_of, shl, var

__all__ = ["OptionalOffset", "DirectScale"]


class OptionalOffset:
    """An address that is ``base`` or ``base + K``, with ``K`` defaulting to 0.

    The wrapper is on the *address expression*, not the node kind, so one
    instance serves every address slot::

        oo = OptionalOffset()
        for h in fn.find_all(p.load(addr=oo.addr())):
            base, off = oo.base(h), oo.offset(h)      # off is 0 when absent

        fn.find_all(p.store(addr=oo.addr(), data=p.anything()))

    Coverage is exact: ``base + K`` either has an ``Add`` of a constant or it
    does not, so there is no third encoding to miss. ``load(base)`` is the
    canonical IR form for a zero offset — ``x + 0`` folds to ``x`` — so the 0
    default is supplied here rather than by the IR.

    Reuse one instance per query. Two concurrent queries need two instances,
    since the captures live on the object.
    """

    def __init__(self) -> None:
        self._base = Capture()
        self._off = Capture()

    def addr(self, base=None):
        """The address pattern. ``base`` defaults to a wildcard bound to
        :meth:`base`; pass a pattern to constrain it (then :meth:`base` stays
        unbound, since the capture is yours to place)."""
        b = var(self._base) if base is None else base
        # The add-form MUST be tried first. A bare wildcard also matches the
        # Add node itself, so a bare-first alternation silently reports offset
        # 0 for every offset load rather than failing loudly.
        return one_of([add(b, any_int_const(self._off)), b])

    def offset(self, h) -> int:
        """The constant offset, or 0 when the bare-base arm matched."""
        off = h.uint(self._off)
        return 0 if off is None else off

    def base(self, h):
        """The base sub-expression as a ``Node``; ``None`` if ``addr()`` was
        given an explicit ``base``."""
        return h.node(self._base)


class DirectScale:
    """A ``x * K`` or ``x << K`` scaling, reported as the multiplier ``K``.

    Normalises the shift form to its multiplier, which is the part that is easy
    to get wrong by hand (``x << 3`` is a ×8 scaling, not ×3)::

        ds = DirectScale()
        for h in fn.find_all(ds.of()):
            size = ds.scale(h)        # 12 for imul 12; 8 for shl 3

    **Known limitation — direct forms only.** A multiplier is not always held
    by a constant. ``x * 3`` compiles to ``lea [rdi+rdi*2]``, which lifts to
    ``add(x, mul(x, 2))``: the ×3 is distributed across the ``Add`` and no node
    holds 3, so this reports **2**. Compose that with a shift (``x * 24``) and
    it reports 8. Non-power-of-two sizes are exactly where this happens, so
    treat a result as "the direct scaling I found", never as "the struct size".

    ``mul`` and ``shl`` are not unified in the IR by design: the two forms carry
    the multiplier differently, and the analyses that consume them want opposite
    canonical forms.
    """

    def __init__(self) -> None:
        self._mul = Capture()
        self._shl = Capture()

    def of(self, x=None):
        """The scaling pattern over operand ``x`` (default: any operand)."""
        v = anything() if x is None else x
        return one_of(
            [mul(v, any_int_const(self._mul)), shl(v, any_int_const(self._shl))]
        )

    def scale(self, h):
        """The multiplier: ``K`` for ``x * K``, ``1 << K`` for ``x << K``.
        ``None`` if neither arm is bound in ``h``."""
        m = h.uint(self._mul)
        if m is not None:
            return m
        s = h.uint(self._shl)
        return None if s is None else 1 << s
