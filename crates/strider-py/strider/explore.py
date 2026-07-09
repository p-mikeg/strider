"""Interactive IR explorer: a tiny local server + graphviz (viz.js) frontend.

`strider.serve(lifter, function)` opens a browser tab showing the depth-N
neighborhood (inputs + outputs) around the entry node, rendered by graphviz.
Click any node to re-center on it; a search bar runs a strider pattern and
highlights the matching nodes as pivots.

The graph is never rendered whole — only a small neighborhood — so graphviz is
always fast and pretty. Node ids in the rendered DOT are IR node ids, so
pattern matches (`find_all(...).root`) line up 1:1 with what's drawn.
"""

from __future__ import annotations

import http.server
import json
import socketserver
import urllib.parse
import webbrowser


def _run_pattern(function, expr: str):
    """Evaluate a `strider.pattern` expression against `function`, returning the
    IR node ids of the (deduplicated) matches. `expr` is the pattern DSL text,
    e.g. `load(addr=add(initial_var(), any_int_const()))`."""
    from strider import pattern as _p

    names = {k: getattr(_p, k) for k in dir(_p) if not k.startswith("_")}
    pat = eval(expr, {"__builtins__": {}}, names)  # local dev tool; trusted input
    return sorted({m.root for m in function.find_all(pat)})


_FRONTEND = r"""<!doctype html>
<html><head><meta charset="utf-8"><title>strider explorer</title>
<style>
  :root{--bg:#141417;--panel:#1e1e22;--border:#2e2e34;--text:#d4d4d8;--accent:#5b9cf6}
  *{box-sizing:border-box} html,body{margin:0;height:100%;background:var(--bg);color:var(--text);font-family:system-ui,sans-serif}
  #bar{position:fixed;top:0;left:0;right:0;height:44px;display:flex;align-items:center;gap:10px;padding:0 12px;background:var(--panel);border-bottom:1px solid var(--border);z-index:10}
  #bar input[type=text]{flex:1;background:#141417;color:var(--text);border:1px solid var(--border);border-radius:5px;padding:6px 10px;font-family:monospace;font-size:13px}
  #bar label{font-size:12px;color:#8c8c96}
  #wrap{position:fixed;top:44px;left:0;bottom:0;right:260px;overflow:auto}
  #side{position:fixed;top:44px;right:0;bottom:0;width:260px;background:var(--panel);border-left:1px solid var(--border);overflow:auto;padding:8px;font-size:12px}
  #side .hit{padding:4px 6px;border-radius:4px;cursor:pointer;font-family:monospace}
  #side .hit:hover{background:#2a2a30}
  svg{max-width:none}
  .node.match polygon,.node.match ellipse,.node.match path,.node.match rect{stroke:#ff5555 !important;stroke-width:3px !important}
  #msg{color:#ff6666;font-size:12px;padding:4px 8px}
</style></head><body>
<div id="bar">
  <b>strider</b>
  <label>depth <input id="depth" type="number" min="1" max="12" value="5" style="width:46px"></label>
  <input id="q" type="text" placeholder="pattern, e.g.  load(addr=add(initial_var(), any_int_const()))  — Enter to search">
  <span id="msg"></span>
</div>
<div id="wrap"><div id="graph"></div></div>
<div id="side"><div id="hits"></div></div>
<script src="viz.js"></script>
<script>
let viz, center, matches = new Set();
const graph = document.getElementById("graph"), hits = document.getElementById("hits"), msg = document.getElementById("msg");
const depthEl = document.getElementById("depth");

async function render() {
  const d = depthEl.value;
  const dot = await (await fetch(`/dot?center=${center}&depth=${d}`)).text();
  const svg = viz.renderSVGElement(dot);
  graph.replaceChildren(svg);
  for (const g of svg.querySelectorAll("g.node")) {
    const id = g.querySelector("title")?.textContent;
    if (matches.has(id)) g.classList.add("match");
    g.style.cursor = "pointer";
    g.addEventListener("click", () => { center = id; render(); });
  }
}
async function search() {
  const q = document.getElementById("q").value.trim();
  msg.textContent = ""; hits.innerHTML = "";
  if (!q) return;
  const r = await fetch(`/pattern?q=${encodeURIComponent(q)}`);
  if (!r.ok) { msg.textContent = await r.text(); return; }
  const ids = await r.json();
  matches = new Set(ids.map(String));
  msg.textContent = `${ids.length} match${ids.length===1?"":"es"}`;
  for (const id of ids) {
    const el = document.createElement("div");
    el.className = "hit"; el.textContent = "node " + id;
    el.onclick = () => { center = String(id); render(); };
    hits.appendChild(el);
  }
  render();
}
document.getElementById("q").addEventListener("keydown", e => { if (e.key === "Enter") search(); });
depthEl.addEventListener("change", render);
Viz.instance().then(async v => {
  viz = v;
  center = String(await (await fetch("/entry")).json());
  render();
});
</script></body></html>"""


def serve(lifter, function, host="127.0.0.1", port=0, depth=5):
    """Start the explorer server for `function` (lifted via `lifter`) and open a
    browser tab. Blocks serving requests until interrupted."""
    entry = function.entry_node()

    class Handler(http.server.BaseHTTPRequestHandler):
        def _send(self, body, ctype="text/html", code=200):
            b = body.encode() if isinstance(body, str) else body
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)

        def do_GET(self):
            u = urllib.parse.urlparse(self.path)
            q = urllib.parse.parse_qs(u.query)
            try:
                if u.path == "/":
                    self._send(_FRONTEND)
                elif u.path == "/viz.js":
                    import strider

                    self._send(strider.viz_standalone_js(), "application/javascript")
                elif u.path == "/entry":
                    self._send(json.dumps(entry), "application/json")
                elif u.path == "/dot":
                    c = int(q.get("center", [entry])[0])
                    d = int(q.get("depth", [depth])[0])
                    self._send(lifter.neighborhood_dot(function, c, depth=d), "text/plain")
                elif u.path == "/pattern":
                    ids = _run_pattern(function, q.get("q", [""])[0])
                    self._send(json.dumps(ids), "application/json")
                else:
                    self.send_error(404)
            except Exception as e:  # noqa: BLE001 — surface the error to the UI
                self._send(f"{type(e).__name__}: {e}", "text/plain", code=400)

        def log_message(self, format, *args):  # noqa: A002 — matches base signature
            pass  # quiet

    # Single-threaded on purpose: `Function` is a PyO3 `unsendable` object, so
    # every request must be handled on the same thread that created it — the
    # caller's thread, which blocks here in `serve_forever`. A local single-user
    # explorer serialises its handful of requests just fine.
    class Server(socketserver.TCPServer):
        allow_reuse_address = True

    srv = Server((host, port), Handler)
    url = f"http://{host}:{srv.server_address[1]}/"
    print(f"strider explorer → {url}  (Ctrl-C to stop)")
    webbrowser.open(url)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        srv.shutdown()
