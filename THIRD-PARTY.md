# Third-party components

Strider itself is MIT (see [LICENSE](LICENSE)). The components below are
redistributed with it: the GHIDRA Sleigh decoder and its Rust bindings are
compiled into the Python extension module, sixty third-party Rust crates are
linked into it statically, and two JavaScript bundles are inlined into every
`.html` graph as well as compiled in.

Full licence texts that are too long to inline live under
[`licenses/`](licenses); the wheel and the sdist carry this file and that
directory.

- [GHIDRA Sleigh](#ghidra-sleigh) -- Apache-2.0
- [rsleigh](#rsleigh-410) -- MIT
- [Rust crates](#rust-crates) -- MIT / Apache-2.0 / Unicode-3.0 / Zlib / 0BSD /
  Unlicense
- [svg-pan-zoom](#svg-pan-zoom-361) -- BSD-2-Clause
- [@viz-js/viz](#viz-jsviz-350-standalone) -- MIT, embedding Graphviz (EPL-1.0)
  and Expat (MIT)

The extension module also links `libstdc++` and `libz` from the system rather
than vendoring either, so neither is redistributed here.

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
  plus the 14 compiled `.sla` images `SleighArch`'s constructors name, embedded
  in the module with `include_bytes!`. The sdist additionally carries the C++
  and `.slaspec` sources.

Modified, as Apache-2.0 section 4(b) requires stating: rsleigh patches both
`src/` and `processors/`. Every change is a numbered patch under
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

Sixty third-party crates are in `strider-py`'s normal dependency closure and
are linked statically into the extension module. Their names, versions, SPDX
expressions and licence texts are in
[`licenses/rust-crates.md`](licenses/rust-crates.md), which also carries the
command that regenerates it. Nothing in that set is copyleft; the licences are
MIT, Apache-2.0 (three crates with the LLVM exception), Unicode-3.0, Zlib, 0BSD
and Unlicense.

Strider's own crates are `publish = false`, so only the wheel and the sdist
redistribute any of this.

## JavaScript bundles

Both are vendored under `crates/dot/assets/vendored/` and compiled into the
`dot` crate, and so into the extension module. Every `.html` graph
`dot::GraphDot::as_html_from_dot` emits inlines both verbatim, and the Python
explorer serves the viz.js bundle as `/viz.js`.

### svg-pan-zoom 3.6.1

- File: `crates/dot/assets/vendored/svg-pan-zoom.min.js`
- Upstream: <https://github.com/ariutta/svg-pan-zoom>
- Licence: BSD-2-Clause. Copyright 2009-2010 Andrea Leofreddi
  <a.leofreddi@vleo.net>. Updates and changes to the original SVGPan are under
  the same licence, each change's copyright held by its author.
- Used for: pan / zoom over the rendered SVG in the graph viewer.

The upstream minified distribution ships no licence banner; the one at the head
of the vendored copy reproduces `LICENSE` from the upstream repository.

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
