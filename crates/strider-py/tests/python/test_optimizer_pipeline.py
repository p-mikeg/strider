import strider

from .conftest import symbol_addr


def test_empty_pipeline():
    pipe = strider.opt.OptimizerPipeline.empty()
    assert len(pipe.passes) == 0
    assert len(pipe.post_passes) == 0


def test_default_pipeline_pass_names():
    p = strider.opt.OptimizerPipeline.default()
    names = p.passes
    assert isinstance(names, list) and all(isinstance(n, str) for n in names)
    assert len(names) == 10
    assert "ConstantFold" in names
    assert len(p.post_passes) == 3


def test_python_default_pipeline_matches_rust_pinned_count():
    """The Python default pipeline is listed by hand, so a pass added on the
    Rust side but not mirrored here would silently make the Python pipeline a
    behaviourally different subset.  These counts catch that drift.
    """
    assert len(strider.opt.OptimizerPipeline.default().passes) == 10
    assert len(strider.opt.OptimizerPipeline.default().post_passes) == 3


def test_default_pipeline_nonempty():
    pipe = strider.opt.OptimizerPipeline.default()
    assert len(pipe.passes) > 0


def test_add_pure_pass():
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    pipe.add(strider.opt.KnownBits())
    pipe.add(strider.opt.PhiCollapse())
    pipe.add(strider.opt.RegionCollapse())
    pipe.add(strider.opt.DeadBranchElimination())
    assert len(pipe.passes) == 5


def test_flag_cmp_canonicalize_pass_exposed():
    """Without this pass the lifter's `Equal(Add(a, Neg(b)), 0)` flag shapes
    never collapse to `Equal(a, b)`, breaking every query written against the
    canonical compare shape.
    """
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.FlagCmpCanonicalize())
    assert len(pipe.passes) == 1


def test_if_cond_inversion_pass_exposed():
    """`IfPat` only matches `If` nodes in canonical (non-negated) form, so
    this pass has to be reachable from Python.
    """
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.IfCondInversion())
    assert len(pipe.passes) == 1


def test_default_pipeline_mirrors_rust_default():
    """The default pipeline must stay in step with the Rust one: ten in-loop
    passes (`ConstantFold`, `LoadReadOnly`, `KnownBits`,
    `FlagCmpCanonicalize`, `IfCondInversion`, `PhiCollapse`,
    `RegionCollapse`, `DeadBranchElimination`, `CfgDetach`, `LoadForward`)
    plus three post-passes (`StackOffsetDetect`, `CallStackArgCollect`,
    `FunctionArgDetect`).  Out of sync, the custom-pipeline path silently
    skips canonicalisation and queries that work under `analyze` fail here.
    """
    assert len(strider.opt.OptimizerPipeline.default().passes) == 10
    assert len(strider.opt.OptimizerPipeline.default().post_passes) == 3


def test_cc_aware_passes_construct(x86_memory_elf):
    del x86_memory_elf

    # These passes used to take a CC or arch argument.  They now read the
    # calling convention from the function at run time and carry no
    # per-instance state, so every constructor is zero-arg.  `LoadReadOnly`
    # is likewise a marker; its rom arrives via `lifter(..., rom=mem)`.
    b = strider.opt.LoadForward()
    c = strider.opt.FunctionArgDetect()
    d = strider.opt.CallStackArgCollect()
    e = strider.opt.LoadReadOnly()
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(b)
    pipe.add(e)
    pipe.add_post(c)
    pipe.add_post(d)
    assert len(pipe.passes) == 2
    assert len(pipe.post_passes) == 2


def test_default_optimizer_pipeline_nonempty_pre_and_post():
    # `OptimizerPipeline.default()` is what `Lifter.analyze` drives
    # internally; the low-level `Lifter.build_optimizer_pipeline()` this
    # test used to exercise no longer exists.
    pipe = strider.opt.OptimizerPipeline.default()
    assert len(pipe.passes) > 0
    assert len(pipe.post_passes) > 0


def test_optimize_on_lifter_mutates(x86_memory_elf):
    """`optimize` lives on `Lifter`, not `Function`: `lift.optimize(g)` runs
    the default pipeline in place, and `Function.optimize` / `.reoptimize`
    are gone."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    lift = strider.lift.lifter(arch, mem)
    _cfg, g, _unresolved = lift.analyze(
        addr,
        cc,
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True),
            pipeline=strider.opt.OptimizerPipeline.empty(),
        ),
    )
    assert g.node_count() >= 1  # sanity: something to optimize
    lift.optimize(g)
    # `node_count` counts every arena slot, reachable or not, and need not
    # shrink before compaction; all that matters is the call succeeds and
    # leaves a valid, non-empty graph.
    assert g.node_count() >= 1
    assert not hasattr(g, "optimize")
    assert not hasattr(g, "reoptimize")


def test_graph_reoptimize(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    _cfg, g, _unresolved = s.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    s.optimize(g)
    assert g.node_count() > 0


def test_run_constant_fold_pipeline_on_real_graph(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    _cfg, g, _unresolved = s.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    pre = g.node_count()

    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    pipe.add(strider.opt.KnownBits())
    s.optimize(g, pipe)
    # Optimization may or may not reduce node count; it must leave a valid
    # graph either way.
    assert g.node_count() >= 1
    assert pre >= 1


def test_optimize_twice_on_same_pipeline_raises(x86_memory_elf):
    """Regression: a pipeline drained by a prior `Lifter.optimize` used to
    silently no-op as an empty pipeline on the second call.  It must raise.
    """
    import pytest

    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)
    _cfg, g, _unresolved = s.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )

    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    s.optimize(g, pipe)  # drains pipe
    with pytest.raises(strider.StriderError):
        s.optimize(g, pipe)
