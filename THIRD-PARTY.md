# Third-party components

Strider itself is MIT (see [LICENSE](LICENSE)). Two JavaScript bundles are
vendored under `crates/dot/assets/vendored/` and compiled into the `dot` crate,
and so into the Python extension module. Every `.html` graph
`dot::GraphDot::as_html_from_dot` emits inlines both verbatim, and the Python
explorer serves the viz.js bundle as `/viz.js`, so the HTML files and the wheel
redistribute the components below.

## svg-pan-zoom 3.6.1

- File: `crates/dot/assets/vendored/svg-pan-zoom.min.js`
- Upstream: <https://github.com/ariutta/svg-pan-zoom>
- Licence: BSD-2-Clause. Copyright 2009-2010 Andrea Leofreddi
  <a.leofreddi@vleo.net>. Updates and changes to the original SVGPan are under
  the same licence, each change's copyright held by its author.
- Used for: pan / zoom over the rendered SVG in the graph viewer.

The upstream minified distribution ships no licence banner; the one at the head
of the vendored copy reproduces `LICENSE` from the upstream repository.

## @viz-js/viz 3.5.0 (standalone)

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
