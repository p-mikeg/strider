"""04 — Custom optimizer pipeline: build it pass by pass.

`OptimizerPipeline.default()` builds the full pipeline (every built-in
pass).  When you need finer control — e.g. profiling a single pass, comparing
graphs with and without a specific rewrite, or building a CC-specific
combination — construct the pipeline yourself.

This example lifts `array_sum` twice: once raw (no optimization) and
once with a hand-built pipeline that includes only ConstantFold + KnownBits
+ LoadForward. We compare graph node counts to
show that the optimization actually shrank the IR.

Run from the workspace root:
    python crates/strider-py/examples/python/04_custom_optimizer.py
"""

from __future__ import annotations

import pathlib

import strider
from strider.pattern import anything, load, store

WORKSPACE = pathlib.Path(__file__).resolve().parents[4]
FIXTURE = WORKSPACE / "fixtures" / "out" / "x86" / "memory.elf"

arch = strider.SleighArch.x86()
cc = strider.CallingConvention.x86_cdecl()

elf = strider.load_elf(str(FIXTURE))
mem = elf.reader()
addr = elf.symbol("array_sum")


def lift(pipeline: strider.OptimizerPipeline | None) -> strider.Function:
    """Lift array_sum, optionally apply `pipeline`, return the Function."""
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)
    g = s.analyze_cfg(cfg).function
    if pipeline is not None:
        g.optimize(pipeline)
    return g


def shape(g: strider.Function) -> dict[str, int]:
    """A small grab-bag summary so we can compare two lifts cheaply."""
    return {
        "loads": len(g.find_all(load())),
        "stores": len(g.find_all(store())),
        "any": len(g.find_all(anything())),
    }


# Lift 1: no optimization at all.
raw = lift(pipeline=None)
print(f"raw         : {shape(raw)}")

# Lift 2: a deliberately partial pipeline.
sleigh = strider.Sleigh(arch, mem)
pipe = strider.OptimizerPipeline.empty()
pipe.add(strider.opt.ConstantFold())
pipe.add(strider.opt.KnownBits())
pipe.add(strider.opt.LoadForward(sleigh, cc, arch))

partial = lift(pipeline=pipe)
print(f"partial opt : {shape(partial)}")

# Lift 3: the full default pipeline.
full = lift(pipeline=strider.OptimizerPipeline.default())
print(f"default opt : {shape(full)}")

# A single number that conveys "did optimization help": the total
# node count typically shrinks as redundant phis collapse and dead
# branches get eliminated.
print(
    f"\ntotal nodes — raw: {shape(raw)['any']}, "
    f"partial: {shape(partial)['any']}, default: {shape(full)['any']}"
)
