# strider-py

The Python bindings, and Strider's primary interface. Load a binary, lift and
optimize a function, and query the result with patterns, all from Python.

## Getting started

Build the extension from the workspace root (not this directory):

```bash
uv sync --group dev
uv run maturin develop
uv run pytest
```

Then:

```python
import strider
from strider.pattern import load, add

prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")
cfg, function, unresolved = prog.analyze("array_sum")
for hit in function.find_all(load(addr=add("base", "off")), ignore_casts=True):
    print("offset =", hit.const_uint("off"))
```

## Learn more

- The guides in [`../../docs/`](../../docs/): getting started, vocabulary, the
  full Python walkthrough, and the optimizer passes.
- [`examples/python/`](examples/python/): nine runnable scripts, from a
  quickstart to custom memory readers.
- The `.pyi` stubs in [`strider/`](strider/) are the typed reference for every
  module.
