import strider
from strider.pattern import load

from .conftest import symbol_addr


def test_empty_pipeline():
    pipe = strider.opt.OptimizerPipeline.empty()
    assert len(pipe.passes) == 0
    assert len(pipe.post_passes) == 0


def test_default_pipeline_pass_names():
    """`default()` snapshots the Rust `default_pipeline()`."""
    p = strider.opt.OptimizerPipeline.default()
    names = p.passes
    assert isinstance(names, list) and all(isinstance(n, str) for n in names)
    assert len(names) == 10
    assert "ConstantFold" in names
    assert p.post_passes == ["CallStackArgCollect", "FunctionArgDetect"]


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


def test_optimize_on_function_mutates(x86_memory_elf):
    """`optimize` lives on `Function`, not `Lifter`: `g.optimize()` runs
    the default pipeline in place, and `Lifter.optimize` and `.reoptimize`
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
    g.optimize()
    # `node_count` counts every arena slot, reachable or not, and need not
    # shrink before compaction.
    assert g.node_count() >= 1
    assert not hasattr(lift, "optimize")
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
    g.optimize()
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
    g.optimize(pipeline=pipe)
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
    g.optimize(pipeline=pipe)
    assert pipe.passes == ["ConstantFold"]
    g.optimize(pipeline=pipe)
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


def test_function_optimize_folds_against_the_handles_rom():
    """`Function.optimize` builds the same `OptCtx` `analyze` does, so
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
        result.function.optimize(pipeline=pipe)
        return result.function

    with_rom = loads_after_manual_optimize(True)
    assert not with_rom.find_all(load()), "the rom must reach LoadReadOnly"
    assert len(with_rom.find_all(int_const(0x11111111))) == 1

    without = loads_after_manual_optimize(False)
    assert len(without.find_all(load())) == 1, "no rom, nothing to fold against"


def test_a_rewriting_post_pass_restamps_stack_offsets():
    """A fixed-point pass added with `add_post` that rewrites an address
    leaves no stale stack offset: `LoadForward` turns the `[rax+4]` load's
    address into `rsp + 12`, and the run's closing stamp reads that."""
    # lea rax,[rsp+8]; mov [rsp],rax; mov rax,[rsp]; mov eax,[rax+4]; ret
    code = bytes.fromhex("488d442408" "48890424" "488b0424" "8b4004" "c3")
    base = 0x1000
    mem = strider.reader.BufferReader(base, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        base,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(
            pipeline=strider.opt.OptimizerPipeline.empty(),
        ),
    )
    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add_post(strider.opt.LoadForward())
    fn.optimize(pipeline=pipe)
    assert len(fn.find_all(load().stack_offset(12))) == 1


def test_a_python_default_pipeline_runs_as_the_rust_default(x86_memory_elf):
    """Analysing with `OptimizerPipeline.default()` matches no pipeline."""
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.sleigh.SleighArch.x86()
    cc = strider.sleigh.CallingConvention.x86_cdecl()
    s = strider.lift.lifter(arch, strider.lift.load_elf(str(x86_memory_elf)).reader())
    cfg = strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    builtin = s.analyze(addr, cc, opts=strider.lift.LifterOptions(cfg=cfg)).function
    pipe = strider.opt.OptimizerPipeline.default()
    opts = strider.lift.LifterOptions(cfg=cfg, pipeline=pipe)
    first = s.analyze(addr, cc, opts=opts).function
    second = s.analyze(addr, cc, opts=opts).function
    assert first.to_text() == builtin.to_text() == second.to_text()


def test_the_pass_name_tuples_cover_the_default_pipeline():
    """The registry's names are the default pipeline's passes, and stay out
    of `__all__`."""
    p = strider.opt.OptimizerPipeline.default()
    assert sorted(p.passes) == sorted(strider.opt._MAIN_PASS_NAMES)
    assert set(p.post_passes) <= set(strider.opt._POST_PASS_NAMES)
    assert not {"_MAIN_PASS_NAMES", "_POST_PASS_NAMES"} & set(strider.opt.__all__)
