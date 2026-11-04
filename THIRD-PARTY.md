# Third-party components

Strider is MIT (see [LICENSE](LICENSE)). Redistributed with it: the GHIDRA
Sleigh decoder and its Rust bindings, compiled into the Python extension
module; sixty-four third-party Rust crates, linked statically; and two
JavaScript bundles, compiled in and inlined into every `.html` graph.

Long licence texts are under [`licenses/`](licenses); the wheel and sdist carry
this file and that directory.

- [GHIDRA Sleigh](#ghidra-sleigh) -- Apache-2.0
- [rsleigh](#rsleigh-410) -- MIT
- [Rust crates](#rust-crates) -- MIT / Apache-2.0 / Unicode-3.0 / Zlib / 0BSD /
  Unlicense
- [svg-pan-zoom](#svg-pan-zoom-361) -- BSD-2-Clause
- [@viz-js/viz](#viz-jsviz-350-standalone) -- MIT, embedding Graphviz (EPL-1.0)
  and Expat (MIT)

`libstdc++` and `libz` are linked from the system, not redistributed.

## GHIDRA Sleigh

- Files: `externals/rsleigh/sleigh/src/`, `externals/rsleigh/sleigh/src_generated/`
  and `externals/rsleigh/sleigh/processors/`, a git submodule.
- Upstream: <https://github.com/NationalSecurityAgency/ghidra>
- Licence: Apache-2.0, full text in
  [`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt). The required notice is
  in [`licenses/NOTICE-ghidra.txt`](licenses/NOTICE-ghidra.txt).
- Used for: decoding machine instructions to p-code. 221 of the 223 C++ sources
  carry the `IP: GHIDRA` Apache header (the other two are Bison-generated); the
  146 processor `.slaspec` specifications are GHIDRA's too.
- Shipped as: `libsla.a`, statically linked into `strider/_strider.abi3.so`,
  plus the 15 compiled `.sla` images `SleighArch`'s constructors name, embedded
  in the module with `include_bytes!`. The sdist additionally carries the C++
  and `.slaspec` sources.

Modified (stated per Apache-2.0 section 4(b)): rsleigh patches `src/` and
`processors/`, each change a numbered patch under
`externals/rsleigh/sleigh/patches/`, described in
`externals/rsleigh/sleigh/FORK.md`. Neither ships in the wheel; both are in the
sdist and the repository.

## rsleigh 4.1.0

- Files: `externals/rsleigh/src/` and `externals/rsleigh/build.rs`, a git
  submodule.
- Upstream: <https://github.com/roeeshoshani/rsleigh>
- Licence: MIT, declared by `Cargo.toml` (`license = "MIT"`). The repository
  ships no licence file, so there is no upstream copyright line to reproduce;
  the holders are Roee Shoshani and the fork's contributors, per its git
  history. The MIT text is the one quoted under
  [@viz-js/viz](#viz-jsviz-350-standalone) below.
- Used for: the Rust bindings over `libsla`, the `.sla` compilation driven from
  `build.rs`, and the `Sleigh` lifting API every Strider crate that touches
  machine state calls.
- Shipped as: object code in `strider/_strider.abi3.so`; sources in the sdist.

## Rust crates

Sixty-four third-party crates in `strider-py`'s normal dependency closure are
linked statically. Names, versions, SPDX expressions, licence texts and the
regenerating command are in
[`licenses/rust-crates.md`](licenses/rust-crates.md). None is copyleft; the
licences are
MIT, Apache-2.0 (three crates with the LLVM exception), Unicode-3.0, Zlib, 0BSD
and Unlicense.

Strider's crates are `publish = false`; only the wheel and sdist
redistribute these.

## JavaScript bundles

Vendored under `crates/dot/assets/vendored/` and compiled into the `dot`
crate, hence the extension module. `dot::GraphDot::as_html_from_dot` inlines
both verbatim in every `.html` graph; the Python explorer serves viz.js as
`/viz.js`.

### svg-pan-zoom 3.6.1

- File: `crates/dot/assets/vendored/svg-pan-zoom.min.js`
- Upstream: <https://github.com/ariutta/svg-pan-zoom>
- Licence: BSD-2-Clause. Copyright 2009-2010 Andrea Leofreddi
  <a.leofreddi@vleo.net>. Updates and changes to the original SVGPan are under
  the same licence, each change's copyright held by its author.
- Used for: pan / zoom over the rendered SVG in the graph viewer.

The upstream minified file has no licence banner; the vendored copy's banner
reproduces upstream's `LICENSE`.

### @viz-js/viz 3.5.0 (standalone)

- File: `crates/dot/assets/vendored/viz-standalone.js`
- Upstream: <https://github.com/mdaines/viz-js>
- Licence: MIT. Copyright (c) 2023 Michael Daines.
- Used for: rendering DOT to SVG in the browser, so the emitted page needs no
  Graphviz install and no network.

The bundle's banner carries the copyright line but not the licence text, which
is:

> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.

Per its own banner, the bundle embeds two further components as object code
(a base64 Wasm payload), with no source form in this repository:

- Graphviz 11.x, <https://www.graphviz.org>: Eclipse Public License 1.0, the
  `LICENSE` file of the Graphviz 11.x source tree.
- Expat, <https://libexpat.github.io>: MIT.
