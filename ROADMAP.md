# roadmap

optica is a static-site publisher for markdown knowledge graphs. the rendering pipeline (markdown → HTML → templates → CSS + minimal JS) is constrained by the web platform — DOM, Canvas, CSS. that ceiling is acceptable for documents up to ~10⁴ nodes.

high-performance interactive graph rendering at scale (10⁵+ nodes, GPU compute, realtime tri-kernel) is out of scope. it belongs to a standalone renderer in cyb that targets wgpu/Vulkan/Metal directly, sharing buffers with the layout and ranking compute. optica's graph view stays a read-only static visualization.

## rendering — what this project owns

scope: improvements to the static rendering pipeline that fit within DOM/Canvas constraints.

### tier 1 — small wins, high value

- dark/light mode toggle (CSS variables already in place, runtime switcher missing)
- responsive sweep — current `style.css` has 2 media queries; sidebar, peers, and minimap overflow on small viewports
- code blocks: copy-to-clipboard button, optional line numbers
- images: `loading="lazy"`, `srcset`, captions from alt text, lightbox on click
- accessibility pass: skip-to-content link, ARIA landmarks (`nav`, `main`, `complementary`), visible focus styles, heading hierarchy validation
- minify `style.css` (1770 LOC) and `graph.js` (709 LOC) in release builds

### tier 2 — feature gaps vs peers

- mermaid diagram support (server-side render preferred)
- KaTeX SSR or pre-rendered math (eliminate per-page DOM walk and flash)
- code block extensions: tabbed groups, diff highlighting, language label
- admonitions: allow nested markdown inside body, expand type set
- search UI: typeahead with result snippets and focus-weighted scoring
- inline minimap data (current per-page minimap re-fetches `graph-data.json`)

### tier 3 — query and content

- expand the static query language: graph traversal (`neighbors`, `path`), date filters, aggregates, count
- footnote styling and back-references
- redirect map for renamed pages (alias resolution covers in-graph, not external URLs)

## rendering — out of scope

these belong to a standalone GPU renderer in cyb, not optica:

- realtime force layout on 10⁵+ nodes
- compute-shader tri-kernel running in the viewport
- level-of-detail rendering keyed on focus / cyberank
- frustum and focus culling for navigable mole-scale graphs
- shared GPU buffers between layout, ranking, and rendering
- WebGPU/native renderer with instanced node draws

optica's responsibility ends at producing static graph data (nodes, edges, focus, gravity) consumable by any renderer — including the future GPU renderer.

## non-rendering tracks

other tracks (full-text search via tantivy, plugin hooks, themes-as-packages, exports, i18n) live outside this document until prioritized.
