import strider

EXPECTED = {
    "ir": ["Function", "Node"],
    "lift": ["Lifter", "ElfLifter", "LifterOptions", "AnalyzeResult", "load_elf"],
    "cfg": ["Cfg", "CfgOptions"],
    "sleigh": ["SleighArch", "CallingConvention", "Sleigh", "Vn", "VnSpace"],
    "reader": ["BufferReader", "MemReader", "ReadOnlyMemory"],
    "opt": ["OptimizerPipeline"],
    "pattern": ["Pat", "Capture", "Match"],
    "template": ["Template"],
}


def test_each_symbol_has_exactly_one_home():
    for mod, names in EXPECTED.items():
        m = getattr(strider, mod)
        for n in names:
            assert hasattr(m, n), f"strider.{mod}.{n} missing"


def test_top_level_is_just_error_and_submodules():
    assert hasattr(strider, "StriderError")
    for gone in ("Sleigh", "Cfg", "Function", "Node", "Lifter", "load_elf",
                 "OptimizerPipeline", "Vn", "VnSpace"):
        assert not hasattr(strider, gone), f"strider.{gone} should have moved"


def test_no_import_leaks_at_top_level():
    import strider
    public = {n for n in dir(strider) if not n.startswith("__")}
    # `explore` is a real submodule, not a leak: `Lifter.visualize` imports
    # it (binding it on the package), and `explore.shutdown(port)` is the
    # supported way to stop an explorer running on another thread. A thread
    # left parked in `visualize` aborts the interpreter at finalization.
    allowed = {"StriderError", "ir", "lift", "cfg", "sleigh", "reader",
               "opt", "pattern", "template", "explore"}
    assert public <= allowed, f"unexpected top-level names: {public - allowed}"


def test_viz_standalone_js_is_private():
    # Internal helper for the explorer's local server (serves vendored
    # viz.js offline); must stay off the public `strider` surface and stay
    # underscore-prefixed on the private extension module.
    assert not hasattr(strider, "viz_standalone_js")
    import strider._strider as _ext
    assert hasattr(_ext, "_viz_standalone_js")
    assert not hasattr(_ext, "viz_standalone_js")
