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
/// The previous implementation tried to emit properly-nested
/// `<ul><li>…</li></ul>` structures by opening/closing `<ul>` tags
/// on heading-level transitions. That works only when the document
/// strictly nests (h1 → h2 → h3, never decreasing past the root).
/// Real pages mix levels in any order — e.g. animal-fat-oil starts
/// with two h3s then drops to h2 — and the open/close counts don't
/// balance. The result was orphan `<li>` elements outside any
/// `<ul>`, which browsers fix up inconsistently (some li's got
/// bullets, some didn't, and the indent pattern inverted).
///
/// Flat list + depth class sidesteps the whole problem: every
/// entry is a sibling `<li>` in a single `<ul>`, depth comes from
/// CSS `padding-left` keyed off `data-depth`. The DOM is always
/// valid and the indent always matches the heading hierarchy.
pub fn render_toc_html(entries: &[TocEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let min_level = entries.iter().map(|e| e.level).min().unwrap_or(1);
    let mut html = String::from(
        "<nav class=\"toc\" aria-label=\"Table of Contents\">\n<h3>Contents</h3>\n<ul>\n",
    );

    for entry in entries {
        let depth = entry.level.saturating_sub(min_level) + 1;
        html.push_str(&format!(
            "<li data-depth=\"{}\"><a href=\"#{}\">{}</a></li>\n",
            depth, entry.id, entry.text
        ));
    }

    html.push_str("</ul>\n</nav>");
    html
}

