"""Every DOT, HTML and p-code text render is a function of the analysed bytes.

Two processes hold the Sleigh engine at different host addresses, so anything
leaking one into the output (a LOAD / STORE space id is the address of the
engine's `AddrSpace`) differs between them.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys

import pytest

import strider

from .conftest import fixture_path

_RENDER_ALL = r"""
import hashlib, json, pathlib, sys
import strider

elf, fn_name, out_dir = sys.argv[1:]
out = pathlib.Path(out_dir)
cfg, fn, _ = strider.lift.load_elf(elf).analyze(fn_name)
node = fn.entry_node()
renders = {
    "Cfg.to_dot": cfg.to_dot(),
    "Cfg.to_dot(style=dark)": cfg.to_dot(style="dark"),
    "Cfg.to_html": cfg.to_html(),
    "Cfg.neighborhood_dot": cfg.neighborhood_dot(cfg.entry()),
    "Cfg._region_texts": repr(sorted(cfg._region_texts().items())),
    "Function.to_dot": fn.to_dot(),
    "Function.to_dot(pretty=True)": fn.to_dot(pretty=True),
    "Function.to_html": fn.to_html(),
    "Function.to_html(pretty=True)": fn.to_html(pretty=True),
    "Function.neighborhood_dot": fn.neighborhood_dot(node),
    "Function.neighborhood_dot(pretty=True)": fn.neighborhood_dot(node, pretty=True),
}
cfg.to_dot(str(out / "cfg.dot"))
cfg.to_html(str(out / "cfg.html"))
fn.to_dot(str(out / "fn.dot"), pretty=True)
fn.to_html(str(out / "fn.html"), pretty=True)
for name in ("cfg.dot", "cfg.html", "fn.dot", "fn.html"):
    renders[name] = (out / name).read_text()
entry = cfg.lifter.symbol(fn_name).address
addrs = sorted({a for n in fn.node_ids() for a in fn.node(n).asm_fingerprint()})
renders["Cfg.pcode_at"] = repr([cfg.pcode_at(a) for a in addrs])
renders["Lifter.pcode_at"] = repr([cfg.lifter.pcode_at(entry, a) for a in addrs])
json.dump({k: hashlib.sha1(v.encode()).hexdigest() for k, v in renders.items()}, sys.stdout)
"""


def _render_all(tmp_path, run: int, elf: str, fn_name: str) -> dict[str, str]:
    out_dir = tmp_path / str(run)
    out_dir.mkdir()
    proc = subprocess.run(
        [sys.executable, "-c", _RENDER_ALL, elf, fn_name, str(out_dir)],
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


@pytest.mark.parametrize(
    ("arch", "case", "fn_name"),
    [("x64", "memory", "array_copy"), ("aarch64", "switch", "dispatch_value")],
)
def test_every_render_is_identical_across_processes(tmp_path, arch, case, fn_name):
    elf = str(fixture_path(arch, case))
    first = _render_all(tmp_path, 1, elf, fn_name)
    second = _render_all(tmp_path, 2, elf, fn_name)
    assert first.keys() == second.keys()
    differing = [k for k in first if first[k] != second[k]]
    assert not differing, f"renders differ between two processes: {differing}"


def test_cfg_dot_names_the_load_store_space():
    cfg, _fn, _ = strider.lift.load_elf(str(fixture_path("x64", "memory"))).analyze(
        "array_copy"
    )
    dot = cfg.to_dot()
    assert dot is not None
    assert "Store ram, " in dot and ", ram, " in dot
    assert not re.search(r"(Store|Load [^,\\]+,) 0x[0-9a-f]+:", dot), dot
