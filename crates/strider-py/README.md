# strider-py

Python bindings for the Strider binary analysis pipeline.

## Build (development)

From this directory:

    maturin develop

Then:

    pytest tests/python/

`pyelftools` is required for the integration tests
(`pip install pyelftools`).

See `docs/superpowers/specs/2026-05-01-strider-py-design.md` for the full
design.
