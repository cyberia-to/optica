// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use comrak::nodes::{AstNode, NodeValue};
use serde::Serialize;

/// A table of contents entry extracted from headings.
#[derive(Debug, Clone, Serialize)]
pub struct TocEntry {
    pub level: u8,
    pub text: String,
    pub id: String,
}

/// Extract headings from a comrak AST to build a TOC.
pub fn extract_toc<'a>(root: &'a AstNode<'a>) -> Vec<TocEntry> {
    let mut entries = Vec::new();

    for node in root.descendants() {
        let data = node.data.borrow();
        if let NodeValue::Heading(ref heading) = data.value {
            let text = get_text_content(node);
            if !text.is_empty() {
                let id = slug::slugify(&text);
                entries.push(TocEntry {
                    level: heading.level,
                    text,
                    id,
                });
            }
        }
    }

    entries
}

fn get_text_content<'a>(node: &'a AstNode<'a>) -> String {
    let mut text = String::new();
    for child in node.children() {
        let data = child.data.borrow();
        match &data.value {
            NodeValue::Text(t) => text.push_str(t),
            NodeValue::Code(c) => text.push_str(&c.literal),
            _ => text.push_str(&get_text_content(child)),
        }
    }
    text
}

/// Render TOC entries as a flat list keyed by `data-depth`.
///
/// The page title (from frontmatter) is always prepended at
/// depth=1 and links to `#top`. This guarantees the TOC has a
/// stable root regardless of how the body markdown is structured —
/// some pages start with h3 then drop to h2 (animal-fat-oil),
/// others mix h1 and h2 in arbitrary order (cyb has h2 "from
/// subgraph cyb" followed by h1 "features"). Without a synthetic
/// root, the TOC's first item visually competes with the section
/// list and the indent gets weird.
///
/// Body headings are placed at depth = (level - body_min) + 2,
/// so they always sit at least one indent below the page title.
///
/// Flat list + depth class also sidesteps the nested-<ul>
/// open/close counts that produced orphan <li> elements when the
/// heading sequence didn't strictly nest.
pub fn render_toc_html(entries: &[TocEntry], page_title: Option<&str>) -> String {
    let has_title = page_title.is_some();
    if entries.is_empty() && !has_title {
        return String::new();
    }

    let mut html = String::from(
        "<nav class=\"toc\" aria-label=\"Table of Contents\">\n<h3>Contents</h3>\n<ul>\n",
    );

    if let Some(title) = page_title {
        html.push_str(&format!(
            "<li data-depth=\"1\" class=\"toc-title\"><a href=\"#top\">{}</a></li>\n",
            html_escape(title)
        ));
    }

    if !entries.is_empty() {
        let body_min = entries.iter().map(|e| e.level).min().unwrap_or(1);
        let offset: u32 = if has_title { 2 } else { 1 };
        for entry in entries {
            let depth = entry.level.saturating_sub(body_min) as u32 + offset;
            html.push_str(&format!(
                "<li data-depth=\"{}\"><a href=\"#{}\">{}</a></li>\n",
                depth, entry.id, entry.text
            ));
        }
    }

    html.push_str("</ul>\n</nav>");
    html
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

