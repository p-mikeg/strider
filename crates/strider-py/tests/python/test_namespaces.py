import strider

EXPECTED = {
    "ir": ["Function", "Node"],
    "lift": ["Lifter", "ElfLifter", "LifterOptions", "load_elf"],
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
    # nothing but StriderError + submodules leaks at top level
    assert hasattr(strider, "StriderError")
    for gone in ("Sleigh", "Cfg", "Function", "Node", "Lifter", "load_elf",
                 "OptimizerPipeline", "Vn", "VnSpace"):
        assert not hasattr(strider, gone), f"strider.{gone} should have moved"


def test_no_import_leaks_at_top_level():
    import strider
    public = {n for n in dir(strider) if not n.startswith("__")}
    allowed = {"StriderError", "ir", "lift", "cfg", "sleigh", "reader",
               "opt", "pattern", "template"}
    assert public <= allowed, f"unexpected top-level names: {public - allowed}"
