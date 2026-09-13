# Third-party components

Strider itself is MIT (see [LICENSE](LICENSE)). Two JavaScript bundles are
vendored under `crates/dot/assets/vendored/` and inlined verbatim into every
`.html` graph `dot::GraphDot::as_html_from_dot` emits, so those files
redistribute the components below. Each bundle carries its own licence notice
at its head.

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

Per its own banner, the bundle embeds two further components as object code
(a base64 Wasm payload), with no source form in this repository:

- Graphviz 11.x, <https://www.graphviz.org>: Eclipse Public License 1.0, the
  `LICENSE` file of the Graphviz 11.x source tree.
- Expat, <https://libexpat.github.io>: MIT.
