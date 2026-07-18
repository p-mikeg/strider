"""Interactive IR explorer: a tiny local server + graphviz (viz.js) frontend.

`Lifter.visualize(target)` opens a browser tab showing the depth-N
neighborhood (inputs + outputs) around the entry node, rendered by graphviz.
Click a node to re-center on it (the clicked node stays put); click an edge to
walk along it, shift-click to mark it. A search bar runs a strider pattern
(with autocomplete) and highlights the matching nodes as pivots.

The graph is never rendered whole — only a small neighborhood — so graphviz is
always fast and pretty. Real DOT node ids are IR node ids, so pattern matches
(`find_all(...).root`) line up 1:1 with what's drawn. Per-use constant boxes
(`c*`) and virtual nodes (`v*`: if.true / if.false / Post Call) aren't
navigation targets.
"""

from __future__ import annotations

import http.server
import json
import socketserver
import urllib.parse


def _pattern_names():
    """The `strider.pattern` builder names, for the search-bar autocomplete."""
    from strider import pattern as _p

    return sorted(k for k in dir(_p) if not k.startswith("_"))


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
  :root{--bg:#141417;--bg2:#0f0f12;--panel:#1b1b1f;--panel2:#232329;--border:#2e2e34;
        --text:#d4d4d8;--text2:#8c8c96;--accent:#5b9cf6;--match:#ff5555;--mark:#ffd24a}
  *{box-sizing:border-box}
  html,body{margin:0;height:100%;background:var(--bg);color:var(--text);
            font-family:system-ui,-apple-system,sans-serif;font-size:13px}
  #bar{position:fixed;top:0;left:0;right:0;height:46px;display:flex;align-items:center;gap:10px;
       padding:0 14px;background:linear-gradient(#1f1f24,#191920);border-bottom:1px solid var(--border);z-index:20}
  #bar b{font-weight:600;letter-spacing:.3px}
  .stepper{display:flex;align-items:center;gap:6px;color:var(--text2)}
  .stepper button{width:24px;height:24px;border-radius:5px;border:1px solid var(--border);
       background:var(--panel2);color:var(--text);cursor:pointer;font-size:15px;line-height:1}
  .stepper button:hover{border-color:var(--accent)}
  .navbtn{width:28px;height:26px;border-radius:5px;border:1px solid var(--border);background:var(--panel2);
          color:var(--text);cursor:pointer;font-size:15px;line-height:1}
  .navbtn:hover:not(:disabled){border-color:var(--accent)}
  .navbtn:disabled{opacity:.35;cursor:default}
  .navbtn.cur{background:var(--accent);border-color:var(--accent);color:#000}
  .stepper #dval{min-width:16px;text-align:center;color:var(--text);font-variant-numeric:tabular-nums}
  #qwrap{position:relative;flex:1}
  #q{width:100%;background:var(--bg2);color:var(--text);border:1px solid var(--border);border-radius:6px;
     padding:7px 11px;font-family:ui-monospace,monospace;font-size:13px}
  #q:focus{outline:none;border-color:var(--accent)}
  #ac{position:absolute;top:36px;left:0;background:var(--panel2);border:1px solid var(--border);border-radius:6px;
      min-width:220px;max-height:260px;overflow:auto;box-shadow:0 8px 24px #000a;display:none;z-index:30}
  #ac div{padding:6px 11px;font-family:ui-monospace,monospace;cursor:pointer}
  #ac div.sel,#ac div:hover{background:var(--accent);color:#fff}
  #msg{color:var(--text2);white-space:nowrap;font-variant-numeric:tabular-nums}
  #msg.err{color:var(--match)}
  #wrap{position:fixed;top:46px;left:0;bottom:0;right:340px;overflow:auto;background:
        radial-gradient(circle at 20px 20px,#1a1a1f 1px,transparent 0) 0 0/22px 22px,var(--bg)}
  #graph{transform-origin:0 0}
  #side{position:fixed;top:46px;right:0;bottom:0;width:340px;background:var(--panel);
        border-left:1px solid var(--border);overflow:auto;padding:10px}
  #side h3{margin:2px 0 8px;font-size:11px;text-transform:uppercase;letter-spacing:.5px;color:var(--text2)}
  #nb,#histlist,#hits{max-height:200px;overflow-y:auto}
  .hit{padding:5px 8px;border-radius:5px;cursor:pointer;font-family:ui-monospace,monospace;font-size:12px;
       white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .hit:hover{background:var(--panel2)}
  .hit.cur{background:var(--accent);color:#fff}
  .hit .dir{color:var(--text2);margin-right:5px}
  .hit .role{margin-right:6px}
  .cnt{color:var(--text2);font-weight:400;font-size:10px}
  .legend div{display:flex;align-items:center;gap:7px;padding:2px 0;color:var(--text2);font-size:11px}
  .legend i{width:16px;height:3px;border-radius:2px;display:inline-block}
  /* graph node/edge states */
  g.node.clickable{cursor:pointer}
  g.node.clickable:hover>*:not(text){filter:brightness(1.45)}
  g.node.match>polygon,g.node.match>ellipse,g.node.match>path,g.node.match>rect{
     stroke:var(--match)!important;stroke-width:3px!important}
  g.edge{cursor:pointer}
  g.edge:hover>path{stroke-width:2.4px}
  g.edge.marked>path{stroke:var(--mark)!important;stroke-width:3px!important}
  g.edge.marked>polygon{stroke:var(--mark)!important;fill:var(--mark)!important}
  #hint{position:fixed;bottom:8px;left:12px;color:var(--text2);font-size:11px;pointer-events:none}
</style></head><body>
<div id="bar">
  <b>strider</b>
  <button id="back" class="navbtn" title="Back (Alt+←)">←</button>
  <button id="fwd" class="navbtn" title="Forward (Alt+→)">→</button>
  <span class="stepper">depth <button id="dm">−</button><span id="dval">5</span><button id="dp">+</button></span>
  <button id="raw" class="navbtn" title="Toggle raw (structure-faithful) view" style="width:auto;padding:0 8px">raw</button>
  <div id="qwrap">
    <input id="q" type="text" spellcheck="false"
      placeholder="strider pattern — e.g.  load(addr=add(initial_var(), any_int_const()))">
    <div id="ac"></div>
  </div>
  <span id="msg"></span>
</div>
<div id="wrap"><div id="graph"></div></div>
<div id="side">
  <h3>Neighbors <span id="nbc" class="cnt"></span></h3><div id="nb"></div>
  <h3 style="margin-top:14px">History</h3><div id="histlist"></div>
  <h3 style="margin-top:14px">Matches <span id="hitc" class="cnt"></span></h3>
  <div id="hits"><div style="color:var(--text2);font-size:11px">— none —</div></div>
  <h3 style="margin-top:14px">Edge roles</h3>
  <div class="legend" id="legend"></div>
</div>
<div id="hint">click node = re-center · click edge near an end = walk there · shift-click edge = mark · alt+←/→ = history · ctrl+wheel = zoom</div>

<script src="viz.js"></script>
<script>
const $=id=>document.getElementById(id);
const wrap=$("wrap"), graph=$("graph"), hits=$("hits"), msg=$("msg"), qEl=$("q"), acEl=$("ac"), dval=$("dval");
let viz, center, curSvg, depth=5, scale=1, baseW=0, baseH=0, rawMode=false;
let matches=new Set(), marked=new Set(), names=[];

const ROLES=[["control","#00cccc"],["memory","#cc88aa"],["lhs","#4488ff"],["rhs","#ff4444"],
  ["addr","#cc88ff"],["data / arg","#ff8800"],["value / ret","#88cc88"],["target / sp","#ffdd44"],["cond","#ff44ff"]];
$("legend").innerHTML=ROLES.map(([n,c])=>`<div><i style="background:${c}"></i>${n}</div>`).join("");

const title=g=>g.querySelector("title")?.textContent||"";
const isReal=id=>/^\d+$/.test(id);
const findNode=id=>[...curSvg.querySelectorAll("g.node")].find(g=>title(g)===id);

function applyScale(){ if(!curSvg)return; curSvg.style.width=(baseW*scale)+"px"; curSvg.style.height=(baseH*scale)+"px"; }

let dotCtrl=null;
async function render(anchor){
  if(dotCtrl) dotCtrl.abort();               // cancel any in-flight render (single-threaded server)
  dotCtrl=new AbortController();
  let dot;
  try{ dot=await (await fetch(`/dot?center=${center}&depth=${depth}&raw=${rawMode?1:0}`,{signal:dotCtrl.signal})).text(); }
  catch(e){ if(e.name==="AbortError") return; throw e; }
  curSvg=viz.renderSVGElement(dot);
  curSvg.removeAttribute("width"); curSvg.removeAttribute("height");
  const vb=(curSvg.getAttribute("viewBox")||"0 0 800 600").split(/\s+/).map(Number);
  baseW=vb[2]; baseH=vb[3];
  graph.replaceChildren(curSvg); applyScale(); wire(); updateNeighbors();
  if(anchor){ const g=findNode(anchor.id); if(g){ const r=g.getBoundingClientRect();
    wrap.scrollLeft+=(r.left+r.width/2)-anchor.x; wrap.scrollTop+=(r.top+r.height/2)-anchor.y; } }
}
function centerNode(id,smooth=true){ const g=findNode(id); if(!g)return; const r=g.getBoundingClientRect(),w=wrap.getBoundingClientRect();
  wrap.scrollTo({left:wrap.scrollLeft+(r.left+r.width/2)-(w.left+w.width/2),
                 top:wrap.scrollTop+(r.top+r.height/2)-(w.top+w.height/2), behavior:smooth?"smooth":"auto"}); }
/* Re-render around `id`, keeping it where it was during the swap, then glide it to the viewport center. */
function recenter(id,gEl){ let a=null; if(gEl){const r=gEl.getBoundingClientRect(); a={id,x:r.left+r.width/2,y:r.top+r.height/2};} center=id; pushHist(); render(a).then(()=>{ if(String(center)===String(id)) centerNode(id); }); }

/* ── history: back/forward across re-centers AND searches ── */
let hist=[], hi=-1;
const HIST_MAX=50;
function pushHist(){ hist=hist.slice(0,hi+1); hist.push({center,query:qEl.value,matches:new Set(matches)});
  if(hist.length>HIST_MAX) hist=hist.slice(hist.length-HIST_MAX); hi=hist.length-1; updateNav(); }
function updateNav(){ $("back").disabled=hi<=0; $("fwd").disabled=hi>=hist.length-1; updateHistUI(); }
function go(delta){ const n=hi+delta; if(n<0||n>=hist.length)return; hi=n; const s=hist[hi];
  center=s.center; qEl.value=s.query; matches=new Set(s.matches); render().then(()=>centerNode(center)); updateNav(); }
$("back").onclick=()=>go(-1); $("fwd").onclick=()=>go(1);
document.addEventListener("keydown",e=>{ if(!e.altKey)return;
  if(e.key==="ArrowLeft"){e.preventDefault();go(-1);} else if(e.key==="ArrowRight"){e.preventDefault();go(1);} });
/* Apply match highlighting to the current SVG without re-rendering (no scroll jump). */
function highlight(){ if(!curSvg)return; for(const g of curSvg.querySelectorAll("g.node")) g.classList.toggle("match",matches.has(title(g))); }

const esc=s=>s.replace(/[&<>]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;"}[c]));
const nodeLabel=id=>{ const g=findNode(id); const t=g&&(g.querySelector("text tspan")||g.querySelector("text")); return t?t.textContent:("node "+id); };
/* Follow a virtual (if.true / Post-Call) node to the real node on its far side. */
function realEnd(vid){ for(const g of curSvg.querySelectorAll("g.edge")){ const [s,d]=title(g).split("->");
  if(s===vid && d!==String(center) && isReal(d)) return d; if(d===vid && s!==String(center) && isReal(s)) return s; } return null; }
/* Side panel: the center node's in/out edges, clickable to walk. */
function updateNeighbors(){
  const nb=$("nb"), c=String(center); nb.innerHTML=""; let n=0; const seen=new Set();
  for(const g of curSvg.querySelectorAll("g.edge")){
    const [s,d]=title(g).split("->"); let dir,o;
    if(s===c){dir="→";o=isReal(d)?d:realEnd(d);} else if(d===c){dir="←";o=isReal(s)?s:realEnd(s);} else continue;
    if(!o||seen.has(dir+o))continue; seen.add(dir+o);
    const t=g.querySelector("text"); const role=t?t.textContent:""; const col=(t&&t.getAttribute("fill"))||"#8c8c96";
    const el=document.createElement("div"); el.className="hit";
    el.innerHTML=`<span class="dir">${dir}</span><span class="role" style="color:${col}">${esc(role)||"·"}</span>${esc(nodeLabel(o))}`;
    el.title=(dir==="→"?"walk forward to ":"walk back to ")+nodeLabel(o);
    el.onclick=()=>recenter(o, findNode(o)); nb.appendChild(el); n++;
  }
  if(!n) nb.innerHTML=NONE; $("nbc").textContent=n||"";
}
/* Side panel: the visited trail, clickable to jump anywhere. */
function updateHistUI(){
  const hl=$("histlist"); hl.innerHTML=""; let curEl=null;
  hist.forEach((s,i)=>{ const el=document.createElement("div"); el.className="hit"+(i===hi?" cur":"");
    el.textContent = s.query ? ("search: "+s.query) : ("node "+s.center);
    el.onclick=()=>{ if(i!==hi){ hi=i; const st=hist[hi]; center=st.center; qEl.value=st.query; matches=new Set(st.matches);
      render().then(()=>centerNode(center)); updateNav(); } };
    hl.appendChild(el); if(i===hi) curEl=el; });
  if(curEl) curEl.scrollIntoView({block:"nearest"});  // keep the current (latest by default) entry visible
}

function wire(){
  for(const g of curSvg.querySelectorAll("g.node")){
    const id=title(g);
    if(matches.has(id)) g.classList.add("match");
    if(isReal(id)){ g.classList.add("clickable"); g.addEventListener("click",e=>{e.stopPropagation(); recenter(id,g);}); }
  }
  for(const g of curSvg.querySelectorAll("g.edge")){
    const [s,d]=title(g).split("->"); const key=s+"->"+d;
    if(marked.has(key)) g.classList.add("marked");
    g.addEventListener("click",e=>{
      e.stopPropagation();
      if(e.shiftKey){ marked.has(key)?marked.delete(key):marked.add(key); g.classList.toggle("marked"); return; }
      // Walk toward the endpoint you clicked nearer to: click the arrow end to
      // go forward (to the consumer), the tail to go back (to the producer).
      const near=nid=>{const n=findNode(nid); if(!n)return Infinity; const r=n.getBoundingClientRect(); return Math.hypot(e.clientX-(r.left+r.width/2),e.clientY-(r.top+r.height/2));};
      let t = near(s)<=near(d)?s:d;
      if(!isReal(t)) t = isReal(s)?s:d;   // never land on a virtual node
      if(isReal(t)) recenter(t, findNode(t));
    });
  }
}

const NONE='<div style="color:var(--text2);font-size:11px">— none —</div>';
async function search(){
  const q=qEl.value.trim(); msg.className=""; msg.textContent="";
  if(!q){ matches=new Set(); highlight(); hits.innerHTML=NONE; pushHist(); return; }
  const r=await fetch("/pattern?q="+encodeURIComponent(q));
  if(!r.ok){ msg.className="err"; msg.textContent=await r.text(); return; }
  const res=await r.json();
  if(res && typeof res==="object" && !Array.isArray(res) && "center" in res){
    recenter(String(res.center)); return;
  }
  const ids=res.highlight; matches=new Set(ids.map(String));
  msg.textContent=`${ids.length} match${ids.length===1?"":"es"}`;
  hits.innerHTML = ids.length ? "" : NONE;
  for(const id of ids){ const el=document.createElement("div"); el.className="hit"; el.textContent="node "+id;
    el.onclick=()=>recenter(String(id), findNode(String(id))); hits.appendChild(el); }
  highlight(); pushHist();     // highlight in the current view; no re-render / no scroll jump
}

/* ── autocomplete ── */
let acItems=[], acSel=-1;
function curToken(){ const p=qEl.selectionStart, s=qEl.value.slice(0,p); const m=s.match(/[A-Za-z_][A-Za-z0-9_]*$/); return m?{t:m[0],start:p-m[0].length,end:p}:null; }
function showAc(){ const tok=curToken();
  acItems = tok&&tok.t ? names.filter(n=>n.startsWith(tok.t)).slice(0,12) : [];
  if(!acItems.length){ acEl.style.display="none"; return; }
  acSel=0; acEl.innerHTML=acItems.map((n,i)=>`<div class="${i===0?'sel':''}">${n}</div>`).join(""); acEl.style.display="block";
  [...acEl.children].forEach((el,i)=>el.onclick=()=>pick(i));
}
function pick(i){ const tok=curToken(); if(!tok)return; const n=acItems[i];
  const v=qEl.value; qEl.value=v.slice(0,tok.start)+n+"()"+v.slice(tok.end);
  const cur=tok.start+n.length+1; qEl.setSelectionRange(cur,cur); acEl.style.display="none"; qEl.focus(); }
qEl.addEventListener("input",showAc);
qEl.addEventListener("keydown",e=>{
  if(acEl.style.display==="block" && acItems.length){
    if(e.key==="ArrowDown"){e.preventDefault(); acSel=(acSel+1)%acItems.length;}
    else if(e.key==="ArrowUp"){e.preventDefault(); acSel=(acSel-1+acItems.length)%acItems.length;}
    else if(e.key==="Tab"){e.preventDefault(); pick(acSel); return;}
    else if(e.key==="Escape"){acEl.style.display="none"; return;}
    else if(e.key==="Enter"){ e.preventDefault(); pick(acSel); return; }
    else return;
    [...acEl.children].forEach((el,i)=>el.classList.toggle("sel",i===acSel)); return;
  }
  if(e.key==="Enter") search();
});
document.addEventListener("click",e=>{ if(!qEl.contains(e.target)&&!acEl.contains(e.target)) acEl.style.display="none"; });

/* ── depth + zoom ── */
function setDepth(d){ depth=Math.max(1,Math.min(12,d)); dval.textContent=depth;
  const c=findNode(String(center)); const r=c&&c.getBoundingClientRect();
  render(r?{id:String(center),x:r.left+r.width/2,y:r.top+r.height/2}:null).then(()=>centerNode(String(center))); }
$("dp").onclick=()=>setDepth(depth+1); $("dm").onclick=()=>setDepth(depth-1);
$("raw").onclick=()=>{ rawMode=!rawMode; $("raw").classList.toggle("cur",rawMode);
  render().then(()=>centerNode(String(center))); };
wrap.addEventListener("wheel",e=>{ if(!e.ctrlKey)return; e.preventDefault();
  scale=Math.max(0.2,Math.min(4,scale*(e.deltaY<0?1.12:0.89))); applyScale(); },{passive:false});

Viz.instance().then(async v=>{
  viz=v; names=await (await fetch("/patterns")).json();
  center=String(await (await fetch("/entry")).json());
  await render(); centerNode(center,false); pushHist();
});
</script></body></html>"""


class _IrVisualizer:
    """Adapts a `(lifter, function)` pair to the `_Visualizer` protocol
    `_serve` expects: `entry()`, `dot(center, depth, raw)`, `search(query)`,
    `completions()`."""

    def __init__(self, lifter, function):
        self._lifter, self._fn = lifter, function

    def entry(self):
        return self._fn.entry_node()

    def dot(self, center, depth, raw):
        if raw:
            # Structure-faithful view for when the pretty output can't be
            # trusted (no Sleigh needed → on the Function).
            return self._fn.neighborhood_dot(center, depth=depth)
        return self._lifter.neighborhood_dot(self._fn, center, depth=depth)

    def search(self, query):
        return {"highlight": _run_pattern(self._fn, query)}

    def completions(self):
        return _pattern_names()


class _CfgVisualizer:
    """Adapts a `Cfg` to the `_Visualizer` protocol `_serve` expects:
    `entry()`, `dot(center, depth, raw)`, `search(query)`,
    `completions()`."""

    def __init__(self, cfg):
        self._cfg = cfg
        # Disassembly text per region, built once (Sleigh-backed — not
        # cheap per call) and reused for every text search.
        self._texts = cfg._region_texts()

    def entry(self):
        return self._cfg.entry()

    def dot(self, center, depth, raw):
        # Cfg no longer exposes a raw (structure-faithful) neighborhood
        # view (that lives on Function, over IR node ids — a different
        # id space than a CFG region index, so it isn't a drop-in
        # replacement here). Fall back to the pretty render for both
        # modes; the "raw" toggle is a no-op for a Cfg-backed visualizer.
        del raw
        return self._cfg.neighborhood_dot(center, depth=depth)

    def search(self, query):
        q = query.strip()
        try:
            addr = int(q, 0)  # bare address -> center the containing region
            blk = self._cfg.region_at(addr)
            return {"center": blk} if blk is not None else {"highlight": []}
        except ValueError:
            ql = q.lower()
            hits = sorted(rid for rid, txt in self._texts.items() if ql in txt.lower())
            return {"highlight": hits}

    def completions(self):
        return []  # (region-start addresses can be added later)


def visualize(lifter, target, *, host="127.0.0.1", port=0, depth=5):
    """Start the explorer for `target` — a `Function` (from `analyze`) or a
    `Cfg` (from `build_cfg`/`analyze`), dispatching on `type(target).__name__`
    to avoid importing the pyclass types here. Blocks serving requests until
    interrupted."""
    tn = type(target).__name__
    if tn == "Function":
        vis = _IrVisualizer(lifter, target)
    elif tn == "Cfg":
        vis = _CfgVisualizer(target)
    else:
        raise TypeError(f"visualize expects a Function or Cfg, got {tn}")
    return _serve(vis, host=host, port=port, depth=depth)


def _serve(visualizer, *, host="127.0.0.1", port=0, depth=5):
    """Start the explorer server over any `_Visualizer`-shaped object. Prints
    the URL to stdout (never opens a browser). Blocks serving requests until
    interrupted."""
    entry = visualizer.entry()

    class Handler(http.server.BaseHTTPRequestHandler):
        def _send(self, body, ctype="text/html", code=200):
            b = body.encode() if isinstance(body, str) else body
            try:
                self.send_response(code)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(b)))
                self.end_headers()
                self.wfile.write(b)
            except (BrokenPipeError, ConnectionError):
                pass  # client cancelled the request (e.g. clicked again); nothing to do

        def do_GET(self):
            # Close after each response. This server is single-threaded (the
            # Function is unsendable), so it must never sit blocked in a
            # keep-alive read waiting for the next request on an idle connection
            # — that would stall every other request (a click) until the idle
            # connection times out. One request per connection keeps it snappy.
            self.close_connection = True
            u = urllib.parse.urlparse(self.path)
            q = urllib.parse.parse_qs(u.query)
            try:
                if u.path == "/":
                    self._send(_FRONTEND)
                elif u.path == "/viz.js":
                    import strider._strider as _ext

                    self._send(_ext._viz_standalone_js(), "application/javascript")
                elif u.path == "/entry":
                    self._send(json.dumps(entry), "application/json")
                elif u.path == "/patterns":
                    self._send(json.dumps(visualizer.completions()), "application/json")
                elif u.path == "/dot":
                    c = int(q.get("center", [entry])[0])
                    d = int(q.get("depth", [depth])[0])
                    raw = q.get("raw", ["0"])[0] == "1"
                    dot = visualizer.dot(c, d, raw)
                    self._send(dot, "text/plain")
                elif u.path == "/pattern":
                    result = visualizer.search(q.get("q", [""])[0])
                    self._send(json.dumps(result), "application/json")
                else:
                    self.send_error(404)
            except (BrokenPipeError, ConnectionError):
                pass  # client went away mid-request; not an error
            except Exception as e:  # noqa: BLE001 — surface the error to the UI
                self._send(f"{type(e).__name__}: {e}", "text/plain", code=400)

        def handle_one_request(self):
            # Swallow the client-disconnect races the single-threaded loop hits
            # when the browser cancels an in-flight fetch, so serve_forever keeps
            # running instead of dumping a BrokenPipe traceback.
            try:
                super().handle_one_request()
            except (BrokenPipeError, ConnectionError):
                self.close_connection = True

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
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        srv.shutdown()
