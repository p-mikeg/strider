import strider

from .conftest import symbol_addr


def test_empty_pipeline():
    pipe = strider.OptimizerPipeline.empty()
    assert pipe.pass_count() == 0
    assert pipe.post_pass_count() == 0


def test_default_pipeline_nonempty():
    pipe = strider.OptimizerPipeline.default()
    assert pipe.pass_count() > 0


def test_stable_default_pipeline():
    pipe = strider.OptimizerPipeline.stable_default()
    assert pipe.pass_count() > 0


def test_destructive_default_pipeline():
    pipe = strider.OptimizerPipeline.destructive_default()
    assert pipe.pass_count() > 0


def test_add_pure_pass():
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    pipe.add(strider.opt.KnownBits())
    pipe.add(strider.opt.RedundantPhis())
    pipe.add(strider.opt.DeadBranchElim())
    assert pipe.pass_count() == 4


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

    Today's Rust default: `ConstantFold`, `KnownBits`,
    `FlagCmpCanonicalize`, `IfCondInversion`, `RedundantPhis`,
    `DeadBranchElimination` — six passes.  An out-of-sync Python
    wrapper silently produces a graph that doesn't canonicalise
    flag-cmp shapes, so pattern queries that work under the orchestrator
    path (which uses the Rust default) fail under the custom-pipeline
    path.
    """
    assert strider.OptimizerPipeline.default().pass_count() == 6


def test_stable_default_pipeline_mirrors_rust():
    """`OptimizerPipeline.stable_default()` must mirror Rust.

    Rust's `stable_default_pipeline()`: `ConstantFold`, `KnownBits`,
    `FlagCmpCanonicalize`, `IfCondInversion` — four passes.
    """
    assert strider.OptimizerPipeline.stable_default().pass_count() == 4


def test_cc_aware_passes_construct(x86_memory_elf):
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)

    # Construct each CC/arch-aware pass to confirm their constructors
    # accept the (sleigh, cc[, arch]) triples.
    a = strider.opt.StackStoreDetect(sleigh, cc)
    b = strider.opt.StackLoadForward(sleigh, cc, arch)
    c = strider.opt.FunctionArgDetect(sleigh, cc)
    d = strider.opt.CallStackArgCollect(sleigh, cc)
    e = strider.opt.LoadReadOnly(mem)
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(a)
    pipe.add(b)
    pipe.add(e)
    pipe.add_post(c)
    pipe.add_post(d)
    assert pipe.pass_count() == 3
    assert pipe.post_pass_count() == 2


def test_strider_build_optimizer_pipeline(x86_memory_elf):
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    pipe = s.build_optimizer_pipeline()
    assert pipe.pass_count() > 0
    assert pipe.post_pass_count() > 0


def test_strider_build_stable_optimizer_pipeline(x86_memory_elf):
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    pipe = s.build_stable_optimizer_pipeline()
    assert pipe.pass_count() > 0


def test_strider_build_destructive_optimizer_pipeline(x86_memory_elf):
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    pipe = s.build_destructive_optimizer_pipeline()
    assert pipe.pass_count() > 0


def test_graph_reoptimize(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    g = s.analyze_cfg(cfg).graph
    g.reoptimize()
    g.reoptimize(destructive=True)
    assert g.node_count() > 0


def test_run_constant_fold_pipeline_on_real_graph(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    g = s.analyze_cfg(cfg).graph
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
