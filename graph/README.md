---
tags: optica
icon: "\U0001F52D"
---
knowledge graph publisher — transforms markdown with [[wiki-links]] into a fast static site

any project with markdown files and a GRAPH.md can publish with optica:

```
optica serve .
optica build .
```

## features

- [[wiki-links]] resolution with alias support
- [[tri-kernel]] ranking ([[cyberank]], [[focus]], [[prob]])
- namespace hierarchy with dimensional navigation
- live reload with sub-second content-only rebuilds
- [[LaTeX]] math rendering
- search index generation
- graph visualization

## architecture

scanner → parser → graph builder → tri-kernel → renderer → output

the scanner walks the filesystem. the parser extracts frontmatter and [[wiki-links]]. the graph builder resolves links and computes [[PageRank]], gravity, and the [[tri-kernel]]. the renderer transforms markdown to HTML with template support. the output writes static files

see [[optica/serve]] for the dev server. see [[optica/build]] for static generation
