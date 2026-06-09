// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use crate::graph::PageStore;
use crate::parser::slugify_page_name;
use comrak::{
    arena_tree::Node,
    nodes::{Ast, AstNode, NodeHtmlBlock, NodeValue},
    plugins::syntect::SyntectAdapterBuilder,
    Arena, Options, Plugins,
};
use regex::Regex;

use std::cell::RefCell;

fn setup_comrak_options() -> Options<'static> {
    let mut options = Options::default();
    // Enable WikiLink parsing
    options.extension.wikilinks_title_after_pipe = true;
    // GFM extensions
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.description_lists = true;
    // Parse options
    options.parse.relaxed_autolinks = true;
    // Render options — we control input, allow raw HTML
    options.render.unsafe_ = true;
    options
}

use super::toc::{self, TocEntry};

/// Result of rendering markdown, including TOC data.
pub struct RenderResult {
    pub html: String,
    pub toc: Vec<TocEntry>,
}

/// Escape `|` inside `[[...]]` so comrak's table parser doesn't split wikilinks.
/// Uses U+FFFF as placeholder — comrak's wikilinks_title_after_pipe looks for `|`,
/// so this preserves the separator for comrak while hiding it from the table parser.
/// The placeholder is restored to `|` just before comrak processes wikilinks.
const PIPE_PLACEHOLDER: char = '\u{FFFF}';

fn escape_pipes_in_wikilinks(markdown: &str) -> String {
    let mut result = String::with_capacity(markdown.len());
    let mut inside = false;
    let chars: Vec<char> = markdown.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        if i + 1 < len && chars[i] == '[' && chars[i + 1] == '[' {
            inside = true;
            result.push('[');
            result.push('[');
            i += 2;
            continue;
        }
        if inside && i + 1 < len && chars[i] == ']' && chars[i + 1] == ']' {
            inside = false;
            result.push(']');
            result.push(']');
            i += 2;
            continue;
        }
        if inside && chars[i] == '|' {
            result.push(PIPE_PLACEHOLDER);
        } else {
            result.push(chars[i]);
        }
        i += 1;
    }
    result
}

/// Restore pipe placeholders in a string back to `|`.
fn restore_pipes(s: &str) -> String {
    s.replace(PIPE_PLACEHOLDER, "|")
}

/// Extract math blocks ($..$ and $$..$$) from markdown, replacing them with placeholders.
/// Returns the processed markdown and a list of extracted math strings.
fn extract_math_blocks(markdown: &str) -> (String, Vec<String>) {
    let mut math_blocks: Vec<String> = Vec::new();
    let mut result = String::with_capacity(markdown.len());
    let chars: Vec<char> = markdown.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut inside_wikilink = false;

    while i < len {
        // Track wiki-link boundaries: [[ opens, ]] closes
        if i + 1 < len && chars[i] == '[' && chars[i + 1] == '[' {
            inside_wikilink = true;
            result.push('[');
            result.push('[');
            i += 2;
            continue;
        }
        if inside_wikilink && i + 1 < len && chars[i] == ']' && chars[i + 1] == ']' {
            inside_wikilink = false;
            result.push(']');
            result.push(']');
            i += 2;
            continue;
        }
        // Skip $ inside wiki-links — not math
        if inside_wikilink && chars[i] == '$' {
            result.push('$');
            i += 1;
            continue;
        }

        // Check for $$ (display math) first
        if i + 1 < len && chars[i] == '$' && chars[i + 1] == '$' {
            // Find closing $$
            let start = i;
            i += 2;
            let mut found = false;
            while i + 1 < len {
                if chars[i] == '$' && chars[i + 1] == '$' {
                    let math_str: String = chars[start..i + 2].iter().collect();
                    let idx = math_blocks.len();
                    math_blocks.push(math_str);
                    result.push_str(&format!("\n\nMATH_PLACEHOLDER_{}\n\n", idx));
                    i += 2;
                    found = true;
                    break;
                }
                i += 1;
            }
            if !found {
                // No closing $$, output as-is
                let remainder: String = chars[start..].iter().collect();
                result.push_str(&remainder);
                break;
            }
        }
        // Check for $ (inline math) — skip if preceded by \
        else if chars[i] == '$' && (i == 0 || chars[i - 1] != '\\') {
            let start = i;
            i += 1;
            // Skip if immediately followed by space, another $, or a digit
            // (currency: $7, $15, $25 etc. are not math).
            if i < len && chars[i] != ' ' && chars[i] != '$' && !chars[i].is_ascii_digit() {
                let mut found = false;
                while i < len {
                    if chars[i] == '$' && (i == 0 || chars[i - 1] != '\\') {
                        let math_str: String = chars[start..i + 1].iter().collect();
                        let idx = math_blocks.len();
                        math_blocks.push(math_str);
                        result.push_str(&format!("MATH_PLACEHOLDER_{}", idx));
                        i += 1;
                        found = true;
                        break;
                    }
                    // Don't cross newlines for inline math
                    if chars[i] == '\n' {
                        break;
                    }
                    i += 1;
                }
                if !found {
                    let remainder: String = chars[start..i].iter().collect();
                    result.push_str(&remainder);
                }
            } else {
                result.push('$');
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    (result, math_blocks)
}

/// Ensure underscores inside \text{} blocks are escaped as \_ for KaTeX.
/// KaTeX requires `\_` for literal underscores in \text{} (bare `_` triggers subscript).
/// Handles both already-escaped `\_` and bare `_` by normalizing first.
fn fix_text_underscores(math: &str) -> String {
    let re = Regex::new(r"\\text\{([^}]*)\}").unwrap();
    re.replace_all(math, |caps: &regex::Captures| {
        let inner = &caps[1];
        // Normalize: strip \_ to _, then re-escape all _ to \_
        let normalized = inner.replace("\\_", "_");
        let fixed = normalized.replace('_', "\\_");
        format!("\\text{{{}}}", fixed)
    })
    .to_string()
}

/// Restore math blocks from placeholders in the rendered HTML.
fn restore_math_blocks(html: &str, math_blocks: &[String]) -> String {
    let re = Regex::new(r"MATH_PLACEHOLDER_(\d+)").unwrap();
    re.replace_all(html, |caps: &regex::Captures| {
        let idx: usize = caps[1].parse().unwrap_or(0);
        if idx < math_blocks.len() {
            fix_text_underscores(&math_blocks[idx])
        } else {
            caps[0].to_string()
        }
    })
    .to_string()
}

/// Render markdown to HTML with wikilink resolution, embed expansion, block refs, and queries.
pub fn render_markdown(markdown: &str, store: &PageStore, _code_theme: &str) -> RenderResult {
    render_markdown_with_source(markdown, store, _code_theme, None, None)
}

/// Render markdown with explicit source-page context — required for the
/// wikilink resolver's namespace + basename fallbacks to match a target like
/// `[[visit]]` from `cyber valley/cyb.land.md` to `cyber valley/cyb.land/visit`.
pub fn render_markdown_with_source(
    markdown: &str,
    store: &PageStore,
    _code_theme: &str,
    source_namespace: Option<&str>,
    source_subgraph: Option<&str>,
) -> RenderResult {
    // Pre-process: resolve embeds and block references in the markdown source
    let processed = resolve_embeds_and_refs(markdown, store, source_namespace, source_subgraph, 0);

    // Pre-process: resolve query blocks
    let processed = crate::query::resolve_queries(&processed, store);

    // Protect pipes inside wikilinks from being parsed as table column separators.
    // Replace | with PIPE_PLACEHOLDER inside [[...]], then restore in the parsed AST.
    let processed = escape_pipes_in_wikilinks(&processed);

    // Protect math blocks from comrak processing
    let (processed, math_blocks) = extract_math_blocks(&processed);

    // Strip `^block-id` anchors so they never render as literal text. Runs after
    // math extraction so `\sum ^N` and similar superscripts are already protected.
    let processed = strip_block_markers(&processed);

    let arena = Arena::new();
    let options = setup_comrak_options();

    let root = comrak::parse_document(&arena, &processed, &options);

    // Extract TOC from headings before transforming
    let toc_entries = toc::extract_toc(root);

    // Transform wikilinks to proper HTML links
    transform_wikilinks(root, store, &arena, source_namespace, source_subgraph);

    // Transform external links to open in new tab
    transform_external_links(root, &arena);

    // Add heading IDs for TOC anchors
    inject_heading_ids(root, &arena);

    // Render svgbob fences to inline SVG before syntect sees them
    render_svgbob_blocks(root);

    // Render to HTML with syntax highlighting (CSS class mode — no inline styles)
    let adapter = SyntectAdapterBuilder::new()
        .css()
        .build();
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);

    let mut html = Vec::new();
    comrak::format_html_with_plugins(root, &options, &mut html, &plugins).unwrap();

    let mut html = String::from_utf8(html).unwrap_or_default();

    // Restore math blocks after HTML rendering
    if !math_blocks.is_empty() {
        html = restore_math_blocks(&html, &math_blocks);
    }

    // Post-process: resolve [[wikilinks]] inside <code> and <pre> blocks.
    // Comrak does not parse wikilinks in code blocks, so we handle them here.
    html = resolve_wikilinks_in_code(&html, store);

    RenderResult {
        html,
        toc: toc_entries,
    }
}

/// Inject id attributes into heading nodes by wrapping with anchor spans.
fn inject_heading_ids<'a>(root: &'a AstNode<'a>, arena: &'a Arena<AstNode<'a>>) {
    let mut headings: Vec<(&'a AstNode<'a>, String)> = Vec::new();

    for node in root.descendants() {
        let data = node.data.borrow();
        if let NodeValue::Heading(_) = data.value {
            let text = get_node_text_content(node);
            if !text.is_empty() {
                let id = slug::slugify(&text);
                headings.push((node, id));
            }
        }
    }

    for (node, id) in headings {
        // Prepend an anchor before the heading content
        let anchor_html = format!(r#"<a class="heading-anchor" id="{}"></a>"#, id);
        let anchor_node = arena.alloc(Node::new(RefCell::new(Ast::new(
            NodeValue::HtmlInline(anchor_html),
            node.data.borrow().sourcepos.start,
        ))));
        node.prepend(anchor_node);
    }
}

lazy_static::lazy_static! {
    /// Matches `{{embed [[Page Name]]}}` page embeds
    static ref EMBED_PAGE_RE: Regex = Regex::new(
        r"\{\{embed\s+\[\[([^\]]+)\]\]\s*\}\}"
    ).unwrap();

    /// Matches a trailing Obsidian-style block marker `^block-id` at end of a
    /// line (preceded by whitespace or starting the line). These are anchors for
    /// `{{embed [[Page#^id]]}}`, not content, so they are stripped before render.
    static ref BLOCK_MARKER_RE: Regex = Regex::new(
        r"(?m)(?:[ \t]+|^)\^[A-Za-z0-9][A-Za-z0-9_-]*[ \t]*$"
    ).unwrap();
}

/// Remove trailing `^block-id` markers from rendered source so they don't appear
/// as literal text on the page that defines them.
fn strip_block_markers(markdown: &str) -> String {
    if !markdown.contains('^') {
        return markdown.to_string();
    }
    BLOCK_MARKER_RE.replace_all(markdown, "").to_string()
}

/// Resolve {{embed [[Page]]}} in markdown.
/// `depth` prevents infinite recursion for circular embeds.
/// Resolve `[[wikilinks]]` inside rendered HTML code blocks.
/// Comrak does not parse wikilinks in `<code>` / `<pre>` elements,
/// so this post-processes the final HTML to linkify known page names.
fn resolve_wikilinks_in_code(html: &str, store: &PageStore) -> String {
    lazy_static::lazy_static! {
        // Match [[page]] or [[page￿display]] (PIPE_PLACEHOLDER) or [[page|display]]
        static ref CODE_WIKILINK: Regex = Regex::new(
            &format!(r"\[\[([^\]\|{pp}]+?)(?:[|{pp}]([^\]]+?))?\]\]", pp = PIPE_PLACEHOLDER)
        ).unwrap();
    }

    CODE_WIKILINK.replace_all(html, |caps: &regex::Captures| {
        let page_name = &caps[1];
        let display = caps.get(2).map(|m| m.as_str()).unwrap_or_else(|| {
            // Show last path segment by default
            page_name.rsplit('/').next().unwrap_or(page_name)
        });
        let slug = slugify_page_name(page_name);
        let resolved = if store.pages.contains_key(&slug) {
            slug.clone()
        } else if let Some(canonical) = store.alias_map.get(&slug) {
            canonical.clone()
        } else {
            slug.clone()
        };
        let class = if store.pages.contains_key(&resolved) || store.alias_map.contains_key(&slug) {
            "internal-link"
        } else {
            "internal-link stub-link"
        };
        format!(
            r#"<a href="/{}" class="{}" data-page="{}">{}</a>"#,
            resolved, class, resolved, display
        )
    }).to_string()
}

fn resolve_embeds_and_refs(
    markdown: &str,
    store: &PageStore,
    source_namespace: Option<&str>,
    source_subgraph: Option<&str>,
    depth: usize,
) -> String {
    if depth > 3 {
        return markdown.to_string();
    }

    // Fast path: if no embed patterns exist, skip regex processing
    if !markdown.contains("{{embed") {
        return markdown.to_string();
    }

    // Resolve {{embed [[Page Name]]}} → inline the page's content.
    // The target may carry a `#fragment`:
    //   {{embed [[Page]]}}            → whole page
    //   {{embed [[Page#Heading]]}}    → section under that heading
    //   {{embed [[Page#^block-id]]}}  → single block tagged `^block-id`
    EMBED_PAGE_RE
        .replace_all(markdown, |caps: &regex::Captures| {
            // Split the wikilink target on the first `#` into page + fragment.
            let raw = caps[1].trim();
            let (page_name, fragment) = match raw.split_once('#') {
                Some((p, f)) => (p.trim(), Some(f.trim())),
                None => (raw, None),
            };
            // Resolve the page id the same way wikilinks do — exact match, alias,
            // then namespace/basename fallback — so `[[languages]]` from
            // `soft3/docs` finds `soft3/specs/languages`.
            let slug = crate::graph::links::resolve_link(
                page_name,
                source_namespace,
                source_subgraph,
                store,
            );

            let Some(page) = store.pages.get(&slug) else {
                return format!(
                    "<div class=\"embed embed-page embed-missing\"><em>Embed: page \"{}\" not found</em></div>",
                    page_name
                );
            };

            // Select the source markdown to inline: whole page, a section, or a block.
            let (source, anchor, label) = match fragment {
                None => (page.content_md.clone(), String::new(), page.meta.title.clone()),
                Some(frag) => {
                    // Block refs keep the page title in the header; section refs
                    // add a `title › section` breadcrumb and anchor the link.
                    let extracted = if let Some(block_id) = frag.strip_prefix('^') {
                        extract_block(&page.content_md, block_id.trim())
                            .map(|s| (s, String::new(), page.meta.title.clone()))
                    } else {
                        let heading_slug = slug::slugify(frag);
                        extract_section(&page.content_md, &heading_slug).map(|s| {
                            (
                                s,
                                format!("#{}", heading_slug),
                                format!("{} › {}", page.meta.title, frag),
                            )
                        })
                    };
                    match extracted {
                        Some((s, anchor, label)) => (s, anchor, label),
                        None => {
                            return format!(
                                "<div class=\"embed embed-page embed-missing\"><em>Embed: \"{}#{}\" not found</em></div>",
                                page_name, frag
                            );
                        }
                    }
                }
            };

            // Nested embeds inside the inlined page resolve relative to that
            // page's own namespace/subgraph, not the embedding page's.
            let content = resolve_embeds_and_refs(
                &source,
                store,
                page.namespace.as_deref(),
                page.subgraph.as_deref(),
                depth + 1,
            );
            format!(
                "\n<div class=\"embed embed-page\" data-page=\"{}\">\n<div class=\"embed-header\"><a href=\"/{}{}\" class=\"internal-link\">{}</a></div>\n\n{}\n\n</div>\n",
                slug, slug, anchor, label, content
            )
        })
        .to_string()
}

/// Length of the leading ATX heading marker (`#`..`######` followed by a space),
/// or `None` if the line is not a heading. The returned value is the heading
/// level (1–6); slice `line[level + 1..]` to get the heading text.
fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some(hashes)
    } else {
        None
    }
}

/// Slice the markdown section whose heading slug matches `target_slug`: from the
/// matching heading through to (but not including) the next heading of the same
/// or higher level. The heading line itself is included. Fenced code blocks are
/// skipped so a `#` comment inside ``` is never mistaken for a heading.
fn extract_section(markdown: &str, target_slug: &str) -> Option<String> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut in_fence = false;
    let mut start: Option<(usize, usize)> = None; // (line index, heading level)

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let Some(level) = heading_level(trimmed) else {
            continue;
        };
        let text = trimmed[level + 1..].trim();
        match start {
            None => {
                if slug::slugify(text) == target_slug {
                    start = Some((i, level));
                }
            }
            Some((s, start_level)) => {
                if level <= start_level {
                    return Some(lines[s..i].join("\n").trim_end().to_string());
                }
            }
        }
    }

    start.map(|(s, _)| lines[s..].join("\n").trim_end().to_string())
}

/// Extract a single block tagged with an Obsidian-style `^block-id` marker at the
/// end of a line. Returns the whole containing paragraph (contiguous non-blank
/// lines) with the marker stripped.
fn extract_block(markdown: &str, block_id: &str) -> Option<String> {
    let marker = format!("^{}", block_id);
    let lines: Vec<&str> = markdown.lines().collect();

    // Find the line whose trimmed end is exactly the marker or ends with ` ^id`.
    let idx = lines.iter().position(|line| {
        let t = line.trim_end();
        t == marker || t.strip_suffix(&marker).is_some_and(|p| p.ends_with(char::is_whitespace))
    })?;

    // Walk up to the paragraph start (preceding blank line or top of file).
    let mut start = idx;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }

    let mut block: Vec<String> = lines[start..=idx].iter().map(|s| s.to_string()).collect();
    if let Some(last) = block.last_mut() {
        let t = last.trim_end();
        *last = t[..t.len() - marker.len()].trim_end().to_string();
    }
    // A bare-marker line collapses to empty; drop trailing empties.
    while block.last().is_some_and(|l| l.trim().is_empty()) {
        block.pop();
    }
    if block.is_empty() {
        return None;
    }
    Some(block.join("\n"))
}

/// Replace ```svgbob fenced code blocks with inline SVG before HTML rendering.
/// Runs before the syntect adapter so svgbob ASCII art never hits syntax highlighting.
fn render_svgbob_blocks<'a>(root: &'a AstNode<'a>) {
    let mut to_replace: Vec<(&'a AstNode<'a>, String)> = Vec::new();

    for node in root.descendants() {
        let data = node.data.borrow();
        if let NodeValue::CodeBlock(ref block) = data.value {
            if block.info.trim() == "svgbob" {
                to_replace.push((node, block.literal.clone()));
            }
        }
    }

    // Render with `currentColor` strokes/fills and a transparent backdrop so the
    // diagram inherits the page's text color instead of svgbob's hardcoded
    // black-on-white. This makes the SVG theme-adaptive (dark or light).
    let settings = svgbob::Settings {
        background: "transparent".into(),
        stroke_color: "currentColor".into(),
        fill_color: "currentColor".into(),
        include_backdrop: false,
        ..Default::default()
    };

    for (node, literal) in to_replace {
        let svg = svgbob::to_svg_with_settings(&literal, &settings);
        let html = format!("<figure class=\"svgbob\">\n{}\n</figure>\n", svg);
        node.data.borrow_mut().value = NodeValue::HtmlBlock(NodeHtmlBlock {
            block_type: 6,
            literal: html,
        });
    }
}

fn transform_wikilinks<'a>(
    root: &'a AstNode<'a>,
    store: &PageStore,
    arena: &'a Arena<AstNode<'a>>,
    source_namespace: Option<&str>,
    source_subgraph: Option<&str>,
) {
    // Collect nodes that need transformation first to avoid borrow issues
    let mut nodes_to_transform: Vec<(&'a AstNode<'a>, String)> = Vec::new();

    for node in root.descendants() {
        let data = node.data.borrow();
        if let NodeValue::WikiLink(ref wl) = data.value {
            let url = wl.url.clone();
            nodes_to_transform.push((node, url));
        }
    }

    for (node, raw_url) in nodes_to_transform {
        // Restore pipe placeholders that were escaped to protect from table parser.
        // If the URL contains PIPE_PLACEHOLDER, comrak couldn't split url|title,
        // so the whole string is in the URL. Split it manually.
        let restored = restore_pipes(&raw_url);
        let (url, forced_display) = if restored.contains('|') {
            let mut parts = restored.splitn(2, '|');
            let u = parts.next().unwrap().to_string();
            let d = parts.next().map(|s| s.to_string());
            (u, d)
        } else {
            (restored, None)
        };
        let _slug = slugify_page_name(&url);

        // Resolve via the same logic the graph builder uses (links.rs) — exact
        // match → alias → namespace-qualified → source-id walk → basename
        // fallback. Without this, `[[visit]]` from `cyber valley/cyb.land.md`
        // wouldn't find `cyber valley/cyb.land/visit` and would render as a
        // stub-link to the top-level basename URL.
        let resolved_slug = crate::graph::links::resolve_link(
            &url,
            source_namespace,
            source_subgraph,
            store,
        );

        let class = if store.stub_pages.contains(&resolved_slug) {
            "internal-link stub-link"
        } else if store.pages.contains_key(&resolved_slug) {
            "internal-link"
        } else {
            "internal-link stub-link"
        };

        // Get display text: forced from pipe split, or from children, or URL fallback
        let display = if let Some(ref d) = forced_display {
            d.clone()
        } else {
            let child_text = restore_pipes(&get_node_text_content(node));
            if child_text.trim().is_empty() {
                url.rsplit('/').next().unwrap_or(&url).to_string()
            } else {
                child_text
            }
        };

        let html = format!(
            r#"<a href="/{resolved_slug}" class="{class}" data-page="{resolved_slug}">{display}</a>"#,
        );

        // Replace WikiLink node with inline HTML
        let new_node = arena.alloc(Node::new(RefCell::new(Ast::new(
            NodeValue::HtmlInline(html),
            node.data.borrow().sourcepos.start,
        ))));

        node.insert_before(new_node);
        node.detach();
    }
}

/// Transform external links (http/https) to open in new tab.
fn transform_external_links<'a>(root: &'a AstNode<'a>, arena: &'a Arena<AstNode<'a>>) {
    let mut nodes_to_transform: Vec<(&'a AstNode<'a>, String, String)> = Vec::new();

    for node in root.descendants() {
        let data = node.data.borrow();
        if let NodeValue::Link(ref link) = data.value {
            if link.url.starts_with("http://") || link.url.starts_with("https://") {
                let url = link.url.clone();
                let title = link.title.clone();
                nodes_to_transform.push((node, url, title));
            }
        }
    }

    for (node, url, title) in nodes_to_transform {
        let display = get_node_text_content(node);
        let title_attr = if title.is_empty() {
            String::new()
        } else {
            format!(r#" title="{}""#, html_escape(&title))
        };
        let html = format!(
            r#"<a href="{}" class="external-link" target="_blank" rel="noopener noreferrer"{}>{}</a>"#,
            html_escape(&url),
            title_attr,
            html_escape(&display),
        );

        let new_node = arena.alloc(Node::new(RefCell::new(Ast::new(
            NodeValue::HtmlInline(html),
            node.data.borrow().sourcepos.start,
        ))));
        node.insert_before(new_node);
        node.detach();
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn get_node_text_content<'a>(node: &'a AstNode<'a>) -> String {
    let mut text = String::new();
    for child in node.children() {
        let data = child.data.borrow();
        match &data.value {
            NodeValue::Text(t) => text.push_str(t),
            _ => {
                text.push_str(&get_node_text_content(child));
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::build_graph;
    use crate::parser::{PageKind, PageMeta, ParsedPage};
    use std::collections::HashMap;
    use std::path::PathBuf;

    const TEST_THEME: &str = "base16-ocean.dark";

    fn empty_store() -> PageStore {
        build_graph(vec![]).unwrap()
    }

    fn store_with_page(name: &str) -> PageStore {
        store_with_content(name, "")
    }

    fn store_with_content(name: &str, content: &str) -> PageStore {
        let page = ParsedPage {
            id: slugify_page_name(name),
            meta: PageMeta {
                title: name.to_string(),
                properties: HashMap::new(),
                tags: vec![],
                public: Some(true),
                aliases: vec![],
                date: None,
                icon: None,
                menu_order: None,
                stake: None,
            },
            kind: PageKind::Page,
            source_path: PathBuf::new(),
            namespace: None,
            subgraph: None,
            content_md: content.to_string(),
            outgoing_links: vec![],
        };
        build_graph(vec![page]).unwrap()
    }

    #[test]
    fn test_basic_markdown() {
        let store = empty_store();
        let result = render_markdown("# Hello\n\nWorld", &store, TEST_THEME);
        assert!(result.html.contains("<h1>"));
        assert!(result.html.contains("Hello"));
        assert!(result.html.contains("<p>World</p>"));
    }

    #[test]
    fn test_embed_whole_page() {
        let store = store_with_content("Languages", "intro\n\n## the roster\n\n| a | b |\n\n## other\n\ntail");
        let out = resolve_embeds_and_refs("{{embed [[Languages]]}}", &store, None, None, 0);
        assert!(out.contains("intro"));
        assert!(out.contains("the roster"));
        assert!(out.contains("tail"));
    }

    #[test]
    fn test_embed_section() {
        let store = store_with_content(
            "Languages",
            "intro text\n\n## the roster\n\n| a | b |\n| 1 | 2 |\n\n## other section\n\nignored tail",
        );
        let out = resolve_embeds_and_refs("{{embed [[Languages#the roster]]}}", &store, None, None, 0);
        assert!(out.contains("## the roster"), "section heading included: {out}");
        assert!(out.contains("| 1 | 2 |"), "section body included");
        assert!(!out.contains("intro text"), "earlier content excluded");
        assert!(!out.contains("ignored tail"), "next section excluded");
        assert!(out.contains("/languages#the-roster"), "header anchors the section");
    }

    #[test]
    fn test_embed_section_stops_at_same_level() {
        // A deeper heading inside the section must NOT terminate it.
        let store = store_with_content(
            "Doc",
            "## one\n\nA\n\n### one-sub\n\nB\n\n## two\n\nC",
        );
        let out = resolve_embeds_and_refs("{{embed [[Doc#one]]}}", &store, None, None, 0);
        assert!(out.contains("### one-sub"), "nested subsection kept");
        assert!(out.contains("B"));
        assert!(!out.contains("\nC"), "sibling section excluded");
    }

    #[test]
    fn test_embed_section_ignores_fenced_hash() {
        let store = store_with_content(
            "Doc",
            "## real\n\n```sh\n# not a heading\n```\n\nbody\n\n## next\n\nnope",
        );
        let out = resolve_embeds_and_refs("{{embed [[Doc#real]]}}", &store, None, None, 0);
        assert!(out.contains("# not a heading"), "fenced hash preserved in section");
        assert!(out.contains("body"));
        assert!(!out.contains("nope"), "next section excluded despite fenced hash");
    }

    #[test]
    fn test_embed_block() {
        let store = store_with_content(
            "Doc",
            "first para\n\nthe canonical claim spanning\ntwo lines ^claim1\n\nafter",
        );
        let out = resolve_embeds_and_refs("{{embed [[Doc#^claim1]]}}", &store, None, None, 0);
        assert!(out.contains("the canonical claim spanning"), "block start included");
        assert!(out.contains("two lines"), "full paragraph included");
        assert!(!out.contains("^claim1"), "block marker stripped");
        assert!(!out.contains("first para"), "earlier paragraph excluded");
        assert!(!out.contains("after"), "later paragraph excluded");
    }

    #[test]
    fn test_block_marker_stripped_on_source_page() {
        let store = empty_store();
        let result = render_markdown(
            "a claim worth citing ^claim1\n\nnext para",
            &store,
            TEST_THEME,
        );
        assert!(result.html.contains("a claim worth citing"));
        assert!(!result.html.contains("^claim1"), "marker hidden: {}", result.html);
    }

    #[test]
    fn test_block_marker_strip_leaves_superscript_alone() {
        // A mid-line caret (exponent) must survive; only end-of-line anchors go.
        let store = empty_store();
        let result = render_markdown("the value 2^3 equals eight", &store, TEST_THEME);
        assert!(result.html.contains("2^3"), "inline caret preserved: {}", result.html);
    }

    #[test]
    fn test_embed_missing_fragment() {
        let store = store_with_content("Doc", "## present\n\nx");
        let out = resolve_embeds_and_refs("{{embed [[Doc#absent]]}}", &store, None, None, 0);
        assert!(out.contains("embed-missing"));
        assert!(out.contains("Doc#absent"));
    }

    #[test]
    fn test_wikilink_resolved() {
        let store = store_with_page("My Page");
        let result = render_markdown("Link to [[My Page]]", &store, TEST_THEME);
        assert!(result.html.contains("class=\"internal-link\""));
        assert!(result.html.contains("href=\"/my-page\""));
    }

    #[test]
    fn test_wikilink_stub() {
        let store = empty_store();
        let result = render_markdown("Link to [[Missing Page]]", &store, TEST_THEME);
        assert!(result.html.contains("stub-link"));
    }

    #[test]
    fn test_gfm_features() {
        let store = empty_store();
        let result = render_markdown("| A | B |\n|---|---|\n| 1 | 2 |", &store, TEST_THEME);
        assert!(result.html.contains("<table>"));
    }

    #[test]
    fn test_math_block_protection() {
        let store = empty_store();
        // Inline math with backslash-brace should be preserved
        let result = render_markdown(
            "The set $\\left\\{x \\in \\mathbb{R}\\right\\}$ is open.",
            &store,
            TEST_THEME,
        );
        assert!(
            result.html.contains("\\left\\{"),
            "backslash-brace should be preserved in inline math"
        );
        // Display math
        let result = render_markdown("$$\\left\\{x > 0\\right\\}$$", &store, TEST_THEME);
        assert!(
            result.html.contains("\\left\\{"),
            "backslash-brace should be preserved in display math"
        );
        assert!(
            result.html.contains("x > 0"),
            "greater-than should be preserved in display math"
        );
    }

    #[test]
    fn test_text_underscore_fix() {
        // Already-escaped \_ should be preserved for KaTeX
        let store = empty_store();
        let result = render_markdown(
            "$\\text{type\\_tag}(a)$",
            &store,
            TEST_THEME,
        );
        assert!(
            result.html.contains("\\text{type\\_tag}"),
            "escaped underscore should be preserved in \\text{{}}: {}",
            result.html
        );

        // Bare underscores in \text{} should be escaped to \_
        let result = render_markdown(
            "$\\text{staking_share}$",
            &store,
            TEST_THEME,
        );
        assert!(
            result.html.contains("\\text{staking\\_share}"),
            "bare underscore should be escaped in \\text{{}}: {}",
            result.html
        );

        // Multiple \text{} blocks
        let result = render_markdown(
            "$\\text{BBG\\_root} = H(\\text{by\\_neuron.commit})$",
            &store,
            TEST_THEME,
        );
        assert!(result.html.contains("\\text{BBG\\_root}"));
        assert!(result.html.contains("\\text{by\\_neuron.commit}"));
    }

    #[test]
    fn test_toc_generation() {
        let store = empty_store();
        let result = render_markdown("# First\n\n## Second\n\n### Third\n\nContent", &store, TEST_THEME);
        assert_eq!(result.toc.len(), 3);
        assert_eq!(result.toc[0].text, "First");
        assert_eq!(result.toc[0].level, 1);
        assert_eq!(result.toc[1].text, "Second");
        assert_eq!(result.toc[1].level, 2);
    }
}
