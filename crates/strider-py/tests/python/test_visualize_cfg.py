import threading, time, urllib.request, json
import strider


def _serve_bg(target_kind, port):
    def run():
        lift = strider.lifter(strider.SleighArch.x86_64(),
                              strider.BufferReader(0x1000, b"\x75\x01\x90\xc3"))
        cfg = lift.build_cfg(0x1000)
        target = cfg if target_kind == "cfg" else lift.analyze(0x1000, strider.CallingConvention.x86_64_systemv())[1]
        lift.visualize(target, port=port)
    threading.Thread(target=run, daemon=True).start()


def _get(port, path):
    for _ in range(40):
        try:
            return urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=8).read().decode()
        except Exception:
            time.sleep(0.3)
    raise RuntimeError("server never came up")


def test_visualize_cfg_serves_neighborhood_and_search():
    _serve_bg("cfg", 8931)
    entry = int(_get(8931, "/entry"))
    pretty = _get(8931, f"/dot?center={entry}&depth=1&raw=0")
    raw = _get(8931, f"/dot?center={entry}&depth=1&raw=1")
    assert "#ffcc00" in pretty              # center highlighted
    assert f"n{entry}" in raw               # raw uses region-index ids
    # address search centers the containing block
    res = json.loads(_get(8931, "/pattern?q=0x1000"))
    assert res.get("center") is not None or res.get("highlight") is not None
    # frontend loads
    assert "viz" in _get(8931, "/").lower()


def test_visualize_ir_still_works():
    _serve_bg("ir", 8932)
    entry = int(_get(8932, "/entry"))
    dot = _get(8932, f"/dot?center={entry}&depth=2&raw=0")
    assert "#ffcc00" in dot   # center highlighted => a real neighborhood rendered
