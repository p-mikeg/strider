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

    # These passes read the calling convention off the function at run time
    # and carry no per-instance state, so every constructor is zero-arg.
    # `LoadReadOnly` is a marker too; its rom arrives via `lifter(rom=mem)`.
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
    # `OptimizerPipeline.default()` is what `Lifter.analyze` drives internally.
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
    assert g.node_count() >= 1
    lift.optimize(g)
    # `node_count` counts every arena slot, reachable or not, and need not
    # shrink before compaction.
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
    # Optimization may or may not reduce node count.
    assert g.node_count() >= 1
    assert pre >= 1


def test_optimize_twice_on_the_same_pipeline_reuses_it(x86_memory_elf):
    """Applying a pipeline copies its passes, so the object survives the call
    and keeps its pass list."""
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
    s.optimize(g, pipe)
    assert pipe.passes == ["ConstantFold"]
    s.optimize(g, pipe)
    assert pipe.passes == ["ConstantFold"]


def test_one_lifter_options_drives_many_analyses(x86_memory_elf):
    """A `LifterOptions` carrying a custom pipeline is an ordinary options
    object: reusing it across `analyze` calls gives the same answer each time.
    """
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    mem = strider.lift.load_elf(str(x86_memory_elf)).reader()
    s = strider.lift.lifter(arch, mem)

    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    pipe.add(strider.opt.KnownBits())
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True),
        pipeline=pipe,
    )

    first = s.analyze(addr, cc, opts=opts).function
    second = s.analyze(addr, cc, opts=opts).function
    assert first.to_dot() == second.to_dot()
    assert pipe.passes == ["ConstantFold", "KnownBits"]


def test_lifter_optimize_folds_against_the_handles_rom():
    """`Lifter.optimize` builds the same `OptCtx` `analyze` does, so
    `LoadReadOnly` folds there too."""
    from strider import reader, sleigh
    from strider.pattern import int_const, load

    code_base, table_base = 0x1000, 0x2000
    # mov eax, dword [0x2000] ; ret : an absolute address, so it can fold.
    code = bytes([0x8B, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00, 0xC3]) + bytes(16)
    table = (0x11111111).to_bytes(4, "little") * 4
    mem = reader.BufferReader(code_base, code)
    rom = reader.BufferReader(table_base, table)
    cfg_opts = strider.cfg.CfgOptions(allow_code_before_start_addr=True)

    def loads_after_manual_optimize(with_rom):
        lft = strider.lift.lifter(
            sleigh.SleighArch.x86_64(), mem, rom if with_rom else None
        )
        result = lft.analyze(
            code_base,
            sleigh.CallingConvention.x86_64_systemv(),
            opts=strider.lift.LifterOptions(
                cfg=cfg_opts,
                pipeline=strider.opt.OptimizerPipeline.empty(),
                resolve_indirect_branches=False,
            ),
        )
        pipe = strider.opt.OptimizerPipeline.empty()
        pipe.add(strider.opt.LoadReadOnly())
        pipe.add(strider.opt.ConstantFold())
        lft.optimize(result.function, pipe)
        return result.function

    with_rom = loads_after_manual_optimize(True)
    assert not with_rom.find_all(load()), "the rom must reach LoadReadOnly"
    assert len(with_rom.find_all(int_const(0x11111111))) == 1

    without = loads_after_manual_optimize(False)
    assert len(without.find_all(load())) == 1, "no rom, nothing to fold against"
