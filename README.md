# 🔭 optica

knowledge graph publisher — transforms a directory of files into a fast, navigable static site. markdown pages with wiki-links are the primary content. all other files (source code, configs, data, binaries) become graph nodes with syntax-highlighted previews

```
optica serve .        # dev server with live reload
optica build .        # generate static site
optica check .        # validate links, find orphans
```

## what it does

point optica at any directory. markdown files with `[[wiki-links]]` become navigable pages. all other files — `.rs`, `.toml`, `.nu`, `.json`, images, binaries — become graph nodes with syntax highlighting, metadata, and backlinks. the entire repo is the knowledge graph.

```
your-project/
├── root/              # pages (or any directory name)
│   ├── cyber/         # namespaces = directories
│   │   ├── truth.md
│   │   └── truth/     # sub-namespaces
│   │       ├── serum.md
│   │       └── coupling.md
│   ├── focus.md       # root-level pages
│   └── particle.md
├── blog/              # journal entries (optional)
├── publish.toml       # configuration
└── build/             # output (generated)
```

## features

- wiki-link resolution with aliases — `[[page]]`, `[[page|display text]]`, `[[namespace/page]]`
- tri-kernel ranking — PageRank + screened Laplacian + heat kernel compute per-page probability
- namespace hierarchy — directories become navigable namespaces with breadcrumbs
- dimensional navigation — pages with the same name across namespaces are shown as "dimensions"
- sub-second live reload — content-only edits skip the full scan and rebuild in <10ms
- subgraph support — import pages from other repos via `subgraph: true` in frontmatter
- YAML frontmatter — tags, aliases, icons, custom properties
- LaTeX math — inline `$...$` and block `$$...$$`
- query expressions — `{{query (and (page-tags [[tag]]))}}` for dynamic content
- embed transclusion — `{{embed [[page]]}}` to include other pages inline
- search index — JSON search index for client-side full-text search
- graph visualization — interactive force-directed minimap per page
- RSS feed, sitemap, SEO metadata

## quick start

```bash
# build optica
git clone <repo-url> ~/git/optica
cd ~/git/optica
cargo build --release

# serve any markdown directory
~/git/optica/target/release/optica serve ~/git/your-project --open
```

the scanner looks for pages in `root/` (fallback: `graph/`, `pages/`). configuration via `publish.toml` in the project root.

## page format

```markdown
---
tags: topic, subtopic
alias: alternative name, another name
icon: "🔭"
---
content with [[wiki-links]] and $\LaTeX$ math

## headings become namespace children

link to [[other pages]] freely. aliases resolve automatically.
```

## architecture

```
scanner → parser → graph builder → tri-kernel → renderer → output
```

| Stage | What it does |
|-------|-------------|
| scanner | walks filesystem, classifies files, discovers subgraphs |
| parser | extracts YAML frontmatter, normalizes outliner format, collects wiki-links |
| graph | resolves links, builds alias map, computes PageRank + tri-kernel |
| renderer | markdown → HTML with template support, wiki-link resolution, math |
| output | writes HTML, search index, graph data, sitemap, RSS |

the dev server watches for changes. content-only edits (no new links, no tag changes) take the fast path: skip the scan, re-parse only the changed file, re-render only the dirty page. structural changes trigger a full incremental rebuild.

## configuration

`publish.toml` in the project root:

```toml
[site]
title = "My Knowledge Base"
base_url = "http://localhost:8080"

[content]
public_only = false
default_public = true
exclude_patterns = [".git/*", "target/*", "build/*"]

[graph]
show_minimap = true
minimap_depth = 2

[search]
enabled = true
```

## performance

on a 12K page graph (cyber knowledge base):

| Operation | Time |
|-----------|------|
| full build | ~28s |
| incremental rebuild (structural) | ~12s |
| fast path (content-only edit) | <10ms |
| live reload latency | ~110ms (100ms debounce + render) |

## license

cyber license: don't trust. don't fear. don't beg.
