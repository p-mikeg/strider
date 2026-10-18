"""A render reads the arch's name table, never the `Lifter`, so it runs off
the thread that built the handle and while that handle is mid-`analyze`."""

import pytest

import strider

BASE = 0x1000
CODE = b"\x48\x83\xc0\x01\xc3"  # add rax, 1 ; ret
IGNORE = strider.lift.LifterOptions(on_incomplete=strider.lift.OnIncomplete.IGNORE)


def test_a_read_callback_renders_a_function_of_its_own_lifter():
    """The second `analyze` holds the handle while its reader runs; a render of
    the first result from inside that read must not need the handle."""
    inner = strider.reader.BufferReader(BASE, CODE)
    holder = {}
    seen = []

    class Rendering(strider.reader.MemReader):
        def read(self, addr, size):
            if "first" in holder and not seen:
                try:
                    seen.append(holder["first"].function.to_dot(pretty=True))
                    seen.append(holder["first"].cfg.to_dot())
                except Exception as e:  # kept out of the decoder's frame
                    seen.append(repr(e))
            return inner.read(addr, size)

    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), Rendering())
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    holder["first"] = lift.analyze(BASE, cc, IGNORE)
    lift.analyze(BASE, cc, IGNORE)
    assert len(seen) == 2 and all(s.startswith("digraph") for s in seen), seen


@pytest.mark.parametrize(
    "render",
    [
        lambda r: r.function.to_dot(pretty=True, lifter=None),
        lambda r: r.function.neighborhood_dot(r.function.entry_node(), lifter=None),
        lambda r: r.cfg.to_dot(lifter=None),
        lambda r: r.cfg.neighborhood_dot(r.cfg.entry(), lifter=None),
    ],
    ids=["fn_to_dot", "fn_neighborhood", "cfg_to_dot", "cfg_neighborhood"],
)
def test_the_renderers_take_no_lifter(render):
    lift = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), strider.reader.BufferReader(BASE, CODE)
    )
    result = lift.analyze(BASE, strider.sleigh.CallingConvention.x86_64_systemv(), IGNORE)
    with pytest.raises(TypeError, match="lifter"):
        render(result)
