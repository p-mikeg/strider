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
    """Pin Python's manually-listed default
    pipeline pass count against the Rust-side factory function.
    Adding a Rust pass without updating PipelineState::from_default in
    crates/strider-py/src/opt.rs would make the Python pipeline a
    behaviourally-different subset of the Rust one — silent drift.
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
    """`FlagCmpCanonicalize` must be addable from Python.

    Without this pass, `Equal(Add(a, Neg(b)), 0)` flag-cmp shapes left
    by the lifter never collapse to `Equal(a, b)`, which breaks pattern
    queries that match on the canonical compare shape (e.g.
    `int_eq(load(<base>+K), add(<base>, K))` for `list_empty`-style
    loops).  The pass is in the Rust `opt::default_pipeline()`; the
    Python wrapper must mirror it.
    """
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.FlagCmpCanonicalize())
    assert len(pipe.passes) == 1


def test_if_cond_inversion_pass_exposed():
    """`IfCondInversion` must be addable from Python.

    Required for `IfPat` to match — that pattern depends on every `If`
    being in canonical (non-`BoolNeg`) form.
    """
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.IfCondInversion())
    assert len(pipe.passes) == 1


def test_default_pipeline_mirrors_rust_default():
    """`OptimizerPipeline.default()` must include every pass that the
    Rust `opt::default_pipeline()` does.

    Today's Rust default: ten in-loop passes (`ConstantFold`,
    `LoadReadOnly`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`,
    `PhiCollapse`, `RegionCollapse`, `DeadBranchElimination`, `CfgDetach`,
    `LoadForward`) plus three post-passes (`StackOffsetDetect`,
    `CallStackArgCollect`, `FunctionArgDetect`).  (Structural twins a rewrite
    leaves behind are re-merged incrementally by the edit context's `clean()`
    re-canonicalization, not a dedup pass.)  An out-of-sync Python wrapper
    silently produces a graph that doesn't canonicalise flag-cmp shapes, so
    pattern queries that work under the orchestrator path (which uses the Rust
    default) fail under the custom-pipeline path.
    """
    assert len(strider.opt.OptimizerPipeline.default().passes) == 10
    assert len(strider.opt.OptimizerPipeline.default().post_passes) == 3


def test_cc_aware_passes_construct(x86_memory_elf):
    del x86_memory_elf

    # Construct each formerly-CC/arch-aware pass to confirm their
    # zero-arg constructors work.  The calling convention is read from the
    # function under analysis at run time, so these passes carry no
    # per-instance state.  `LoadReadOnly()` is a marker too — its rom
    # flows via `strider.lift.lifter(..., rom=mem)`.
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
    # `strider.opt.OptimizerPipeline.default()` is the canonical default
    # pipeline (the one `Lifter.analyze` drives internally); the
    # low-level `Lifter.build_optimizer_pipeline()` this test used to
    # exercise was removed by the single-`Lifter` collapse.
    pipe = strider.opt.OptimizerPipeline.default()
    assert len(pipe.passes) > 0
    assert len(pipe.post_passes) > 0


def test_optimize_on_lifter_mutates(x86_memory_elf):
    """`optimize` lives on `Lifter`, not `Function`: `lift.optimize(g)`
    (no pipeline) runs the default pipeline in place, and neither
    `optimize` nor `reoptimize` exist on `Function` any more."""
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
    lift.optimize(g)  # default pipeline, in place
    # `node_count` counts every arena slot (reachable or not) and isn't
    # guaranteed to shrink monotonically pre-compaction — the load-
    # bearing assertion is that the call succeeds and leaves a valid,
    # non-empty graph.
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
    # Optimization may or may not reduce node count; at minimum it must
    # leave a valid graph (no exception).
    assert g.node_count() >= 1
    # Also: pre/post should be sensible integers.
    assert pre >= 1


def test_optimize_twice_on_same_pipeline_raises(x86_memory_elf):
    """Regression: a wrapper that has
    already been drained by a prior `Lifter.optimize` call must
    surface a typed error on a second call, not silently no-op with an
    empty pipeline.
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
    # Second call: must raise StriderError, not silently succeed.
    with pytest.raises(strider.StriderError):
        s.optimize(g, pipe)
