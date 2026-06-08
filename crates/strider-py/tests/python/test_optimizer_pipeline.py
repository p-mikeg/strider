import strider

from .conftest import symbol_addr


def test_empty_pipeline():
    pipe = strider.OptimizerPipeline.empty()
    assert pipe.pass_count() == 0
    assert pipe.post_pass_count() == 0


def test_python_default_pipeline_matches_rust_pinned_count():
    """Pin Python's manually-listed default
    pipeline pass count against the Rust-side factory function.
    Adding a Rust pass without updating PipelineState::from_default in
    crates/strider-py/src/opt.rs would make the Python pipeline a
    behaviourally-different subset of the Rust one — silent drift.
    """
    assert strider.OptimizerPipeline.default().pass_count() == 10
    assert strider.OptimizerPipeline.default().post_pass_count() == 3


def test_default_pipeline_nonempty():
    pipe = strider.OptimizerPipeline.default()
    assert pipe.pass_count() > 0


def test_add_pure_pass():
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    pipe.add(strider.opt.KnownBits())
    pipe.add(strider.opt.PhiCollapse())
    pipe.add(strider.opt.RegionCollapse())
    pipe.add(strider.opt.DeadBranchElimination())
    assert pipe.pass_count() == 5


def test_flag_cmp_canonicalize_pass_exposed():
    """`FlagCmpCanonicalize` must be addable from Python.

    Without this pass, `Equal(Add(a, Neg(b)), 0)` flag-cmp shapes left
    by the lifter never collapse to `Equal(a, b)`, which breaks pattern
    queries that match on the canonical compare shape (e.g.
    `int_eq(load(<base>+K), add(<base>, K))` for `list_empty`-style
    loops).  The pass is in the Rust `opt::default_pipeline()`; the
    Python wrapper must mirror it.
    """
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.FlagCmpCanonicalize())
    assert pipe.pass_count() == 1


def test_if_cond_inversion_pass_exposed():
    """`IfCondInversion` must be addable from Python.

    Required for `IfPat` to match — that pattern depends on every `If`
    being in canonical (non-`BoolNeg`) form.
    """
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.IfCondInversion())
    assert pipe.pass_count() == 1


def test_default_pipeline_mirrors_rust_default():
    """`OptimizerPipeline.default()` must include every pass that the
    Rust `opt::default_pipeline()` does.

    Today's Rust default: ten in-loop passes (`ConstantFold`,
    `LoadReadOnly`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`,
    `PhiCollapse`, `RegionCollapse`, `DeadBranchElimination`, `CfgDetach`,
    `LoadForward`) plus three post-passes (`StackOffsetDetect`,
    `CallStackArgCollect`, `FunctionArgDetect`).  An out-of-sync Python
    wrapper silently produces a graph that doesn't canonicalise
    flag-cmp shapes, so pattern queries that work under the orchestrator
    path (which uses the Rust default) fail under the custom-pipeline
    path.
    """
    assert strider.OptimizerPipeline.default().pass_count() == 10
    assert strider.OptimizerPipeline.default().post_pass_count() == 3


def test_cc_aware_passes_construct(x86_memory_elf):
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    sleigh = strider.Sleigh(arch, mem)

    # Construct each CC/arch-aware pass to confirm their constructors
    # accept the (sleigh, cc[, arch]) triples.  `LoadReadOnly()` is a
    # marker now — its rom flows via `strider.run(..., rom=mem)`.
    b = strider.opt.LoadForward(sleigh, cc, arch)
    c = strider.opt.FunctionArgDetect(sleigh, cc)
    d = strider.opt.CallStackArgCollect(sleigh, cc)
    e = strider.opt.LoadReadOnly()
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(b)
    pipe.add(e)
    pipe.add_post(c)
    pipe.add_post(d)
    assert pipe.pass_count() == 2
    assert pipe.post_pass_count() == 2


def test_strider_build_optimizer_pipeline(x86_memory_elf):
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    pipe = s.build_optimizer_pipeline()
    assert pipe.pass_count() > 0
    assert pipe.post_pass_count() > 0


def test_graph_reoptimize(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)
    g = s.analyze_cfg(cfg).function
    g.reoptimize()
    assert g.node_count() > 0


def test_run_constant_fold_pipeline_on_real_graph(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)
    g = s.analyze_cfg(cfg).function
    pre = g.node_count()

    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    pipe.add(strider.opt.KnownBits())
    g.optimize(pipe)
    # Optimization may or may not reduce node count; at minimum it must
    # leave a valid graph (no exception).
    assert g.node_count() >= 1
    # Also: pre/post should be sensible integers.
    assert pre >= 1


def test_optimize_twice_on_same_pipeline_raises(x86_memory_elf):
    """Regression: a wrapper that has
    already been drained by a prior `Function.optimize` (or
    `strider.run`) call must surface a typed error on a second call,
    not silently no-op with an empty pipeline.
    """
    import pytest

    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(addr, allow_code_before_start_addr=True)
    g = s.analyze_cfg(cfg).function

    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    g.optimize(pipe)  # drains pipe
    # Second call: must raise StriderError, not silently succeed.
    with pytest.raises(strider.errors.StriderError):
        g.optimize(pipe)
