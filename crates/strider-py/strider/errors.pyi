"""Type stubs for strider.errors."""

from __future__ import annotations

class StriderError(Exception):
    """The single exception type raised by strider.  Every Rust error
    lands here carrying an informative message; the hierarchy is flat
    (no typed subclasses)."""
    ...
