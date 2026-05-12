// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
pub mod admonitions;
pub mod outliner;
pub mod properties;
pub mod wikilinks;

use crate::scanner::{DiscoveredFile, DiscoveredFiles, FileKind};
use anyhow::Result;
use chrono::NaiveDate;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Unique identifier for a page (normalized/slugified from page name)
pub type PageId = String;

#[derive(Debug, Clone, Serialize)]
pub struct PageMeta {
    pub title: String,
    pub properties: HashMap<String, String>,
    pub tags: Vec<String>,
    pub public: Option<bool>,
    pub aliases: Vec<String>,
    pub date: Option<NaiveDate>,
    pub icon: Option<String>,
    pub menu_order: Option<i32>,
    pub stake: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum PageKind {
    Page,
    Journal,
    File,
}

#[derive(Debug, Clone)]
pub struct ParsedPage {
    pub id: PageId,
    pub meta: PageMeta,
    pub kind: PageKind,
    pub source_path: PathBuf,
    pub namespace: Option<String>,
    /// Which subgraph this page belongs to (None = root graph)
    pub subgraph: Option<String>,
    /// Normalized markdown content (after outliner transform, properties stripped)
    pub content_md: String,
    /// Wikilinks found during parsing (raw page names, not yet slugified)
    pub outgoing_links: Vec<String>,
}

pub fn slugify_page_name(name: &str) -> PageId {
    use unicode_normalization::UnicodeNormalization;
    let lower = name.nfc().collect::<String>().to_lowercase();
    let mut result = String::with_capacity(lower.len());
    let mut prev_hyphen = true; // prevents leading hyphen

    for ch in lower.chars() {
        if ch.is_alphanumeric() || ch == '$' || ch == '.' {
            result.push(ch);
            prev_hyphen = false;
        } else if ch == '/' {
            // Preserve path separators for namespace hierarchy
            // Trim trailing hyphen before slash
            if result.ends_with('-') {
                result.pop();
            }
            result.push('/');
            prev_hyphen = true; // prevents hyphen after slash
        } else if !prev_hyphen {
            result.push('-');
            prev_hyphen = true;
        }
    }

    let mut slug = result.trim_end_matches('-').to_string();
    // macOS HFS+/APFS limit: 255 bytes per path component;
    // leave room for /index.html in pretty URL mode
    if slug.len() > 200 {
        slug.truncate(200);
        slug = slug.trim_end_matches('-').to_string();
    }
    slug
}

/// Rewrite `../media/<filename>` references in every page's content_md to
/// `<gateway>/ipfs/<cid>` using a filename→CID map loaded from `map_path`.
/// Falls through unknown filenames untouched. Returns the number of refs
/// rewritten across all pages.
///
/// Must run on every (re)parse — both initial build and live-reload — or
/// re-rendered pages revert to the raw markdown path and images break.
pub fn apply_ipfs_rewrites(
    pages: &mut [ParsedPage],
    map_path: &std::path::Path,
    gateway: &str,
) -> anyhow::Result<usize> {
    let raw = std::fs::read_to_string(map_path)
        .map_err(|e| anyhow::anyhow!("reading ipfs map {}: {}", map_path.display(), e))?;
    let map: std::collections::HashMap<String, String> = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("parsing ipfs map {}: {}", map_path.display(), e))?;
    let gateway = gateway.trim_end_matches('/');
    let re = regex::Regex::new(r#"\.\./media/([^\s\)"'\]<>]+)"#).unwrap();
    let mut rewrites = 0usize;
    for page in pages.iter_mut() {
        let rewritten = re
            .replace_all(&page.content_md, |caps: &regex::Captures| {
                match map.get(&caps[1]) {
                    Some(cid) => {
                        rewrites += 1;
                        format!("{}/ipfs/{}", gateway, cid)
                    }
                    None => caps[0].to_string(),
                }
            })
            .to_string();
        if rewritten != page.content_md {
            page.content_md = rewritten;
        }
    }
    Ok(rewrites)
}

/// Resolve which IPFS map path to use: explicit `config.media.ipfs_map` if
/// set, else `<input_dir>/ipfs-cache.json` if it exists, else None.
pub fn resolve_ipfs_map(config: &crate::config::SiteConfig) -> Option<std::path::PathBuf> {
    config.media.ipfs_map.clone().or_else(|| {
        let default = config.build.input_dir.join("ipfs-cache.json");
        default.exists().then_some(default)
    })
}

/// One-stop call: resolve the map and apply rewrites to all pages. Returns
/// `(count, map_path)` so callers can log; `count = 0` and `map_path = None`
/// when no map is configured.
pub fn apply_ipfs_rewrites_for_config(
    pages: &mut [ParsedPage],
    config: &crate::config::SiteConfig,
) -> anyhow::Result<(usize, Option<std::path::PathBuf>)> {
    let Some(map_path) = resolve_ipfs_map(config) else {
        return Ok((0, None));
    };
    let count = apply_ipfs_rewrites(pages, &map_path, &config.media.ipfs_gateway)?;
    Ok((count, Some(map_path)))
}

/// For every namespace dir referenced by a page, ensure an index page exists
/// at that slug. Without this, optica's parent-page rendering emits folder
/// links into its sidebar (any subdir with content shows up) but the linked
/// URL 404s — there's no page to render. Synthesizes a minimal `# <dir>`
/// stub for any missing index, attributing it to the owning subgraph (if
/// any) so render-time grouping stays correct.
pub fn synthesize_dir_indexes(pages: &mut Vec<ParsedPage>, subgraph_names: &[String]) {
    let existing_ids: std::collections::HashSet<String> =
        pages.iter().map(|p| p.id.clone()).collect();
    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for page in pages.iter() {
        if let Some(ref ns) = page.namespace {
            let mut accumulated = String::new();
            for segment in ns.split('/').filter(|s| !s.is_empty()) {
                if accumulated.is_empty() {
                    accumulated = segment.to_string();
                } else {
                    accumulated = format!("{}/{}", accumulated, segment);
                }
                seen_dirs.insert(accumulated.clone());
            }
        }
    }
    for dir_name in &seen_dirs {
        let dir_slug = slugify_page_name(dir_name);
        if existing_ids.contains(&dir_slug) {
            continue;
        }
        let short_name = dir_name.rsplit('/').next().unwrap_or(dir_name);
        let owning_subgraph = subgraph_names
            .iter()
            .find(|sg| dir_name == *sg || dir_name.starts_with(&format!("{}/", sg)))
            .cloned();
        pages.push(ParsedPage {
            id: dir_slug,
            meta: PageMeta {
                title: dir_name.clone(),
                properties: std::collections::HashMap::new(),
                tags: vec![],
                public: Some(true),
                aliases: vec![],
                date: None,
                icon: None,
                menu_order: None,
                stake: None,
            },
            kind: PageKind::Page,
            source_path: std::path::PathBuf::new(),
            namespace: dir_name.rsplitn(2, '/').nth(1).map(|s| s.to_string()),
            subgraph: owning_subgraph,
            content_md: format!("# {}\n", short_name),
            outgoing_links: vec![],
        });
    }
}

pub fn parse_all(discovered: &DiscoveredFiles) -> Result<Vec<ParsedPage>> {
    let mut pages = Vec::new();

    for file in &discovered.pages {
        let page = parse_file(file)?;
        pages.push(page);
    }

    for file in &discovered.journals {
        let page = parse_file(file)?;
        pages.push(page);
    }

    for file in &discovered.files {
        let page = parse_non_md_file(file)?;
        pages.push(page);
    }

    Ok(pages)
}

pub fn parse_file(file: &DiscoveredFile) -> Result<ParsedPage> {
    let content = std::fs::read_to_string(&file.path)?;

    // Step 1: Extract properties (YAML frontmatter or legacy property:: lines)
    let (meta, content_after_props) = properties::extract_properties(&content, &file.name);

    // Step 2: Normalize outliner bullets only if content looks like outliner format
    let normalized = if looks_like_outliner(&content_after_props) {
        outliner::normalize(&content_after_props)
    } else {
        content_after_props
    };

    // Step 2b: Transform admonition blocks
    let normalized = admonitions::transform_admonitions(&normalized);

    // Step 3: Collect wikilinks from the normalized content
    let outgoing_links = wikilinks::collect_wikilinks(&normalized);

    // Determine namespace
    let namespace = extract_namespace(&file.name);

    // Determine kind
    let kind = match file.kind {
        FileKind::Journal => PageKind::Journal,
        _ => PageKind::Page,
    };

    let id = slugify_page_name(&file.name);

    // Rewrite relative markdown links for subgraph pages so they resolve
    // to the correct slugified URLs within the subgraph namespace.
    let is_readme = file.path.file_stem()
        .map(|s| s.to_string_lossy().eq_ignore_ascii_case("readme"))
        .unwrap_or(false);
    let normalized = if file.subgraph.is_some() {
        rewrite_relative_links(&normalized, &file.name, is_readme)
    } else {
        normalized
    };

    Ok(ParsedPage {
        id,
        meta,
        kind,
        source_path: file.path.clone(),
        namespace,
        subgraph: file.subgraph.clone(),
        content_md: normalized,
        outgoing_links,
    })
}

/// Parse a non-markdown file into a graph node.
/// Text files get wrapped in code fences; binary files get a metadata description.
fn parse_non_md_file(file: &DiscoveredFile) -> Result<ParsedPage> {
    let id = slugify_page_name(&file.name);
    let namespace = extract_namespace(&file.name);
    let ext = file
        .path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    // Auto-tag based on extension and directory
    let mut tags = Vec::new();
    if let Some(lang_tag) = extension_to_tag(&ext) {
        tags.push(lang_tag.to_string());
    }
    // For root graph files, auto-tag with the top-level directory.
    // Skip for subgraph files — their namespace IS the subgraph name,
    // and tagging every file with it floods the subgraph root page with backlinks.
    if file.subgraph.is_none() {
        if let Some(ns) = &namespace {
            let top_dir = ns.split('/').next().unwrap_or(ns);
            if !top_dir.is_empty() && !tags.contains(&top_dir.to_string()) {
                tags.push(top_dir.to_string());
            }
        }
    }

    // Try to read as text
    let (content_md, outgoing_links) = match std::fs::read_to_string(&file.path) {
        Ok(text) => {
            let lang = extension_to_lang(&ext);
            // Only extract wikilinks from root graph files — source code in
            // subgraphs contains [[attr]] / arr[[i]] patterns that are not links.
            let outgoing_links = if file.subgraph.is_none() {
                wikilinks::collect_wikilinks(&text)
            } else {
                Vec::new()
            };
            let content_md = format!("```{}\n{}\n```", lang, text);
            (content_md, outgoing_links)
        }
        Err(_) => {
            // Binary file — show metadata
            let size = std::fs::metadata(&file.path)
                .map(|m| format_size(m.len()))
                .unwrap_or_else(|_| "unknown size".to_string());
            let content_md = format!(
                "Binary file: `{}`\n\nSize: {}\nType: {}",
                file.name,
                size,
                if ext.is_empty() {
                    "unknown"
                } else {
                    &ext
                }
            );
            (content_md, Vec::new())
        }
    };

    Ok(ParsedPage {
        id,
        meta: PageMeta {
            title: file.name.clone(),
            properties: HashMap::new(),
            tags,
            public: Some(true),
            aliases: Vec::new(),
            date: None,
            icon: None,
            menu_order: None,
            stake: None,
        },
        kind: PageKind::File,
        source_path: file.path.clone(),
        namespace,
        subgraph: file.subgraph.clone(),
        content_md,
        outgoing_links,
    })
}

fn extract_namespace(name: &str) -> Option<String> {
    if name.contains('/') {
        let parts: Vec<&str> = name.rsplitn(2, '/').collect();
        if parts.len() == 2 {
            Some(parts[1].to_string())
        } else {
            None
        }
    } else {
        None
    }
}

/// Map file extension to a language identifier for code fence syntax highlighting.
fn extension_to_lang(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "nu" => "nu",
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "jsx" => "jsx",
        "tsx" => "tsx",
        "css" => "css",
        "html" | "htm" => "html",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" | "zsh" => "bash",
        "sql" => "sql",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "lua" => "lua",
        "zig" => "zig",
        "nix" => "nix",
        "md" | "markdown" => "markdown",
        "xml" => "xml",
        "csv" => "csv",
        "txt" => "text",
        "edn" => "clojure",
        "gitignore" => "gitignore",
        _ => "",
    }
}

/// Map file extension to a human-readable tag for the graph.
fn extension_to_tag(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "nu" => Some("nushell"),
        "py" => Some("python"),
        "js" => Some("javascript"),
        "ts" => Some("typescript"),
        "jsx" | "tsx" => Some("react"),
        "css" => Some("css"),
        "html" | "htm" => Some("html"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "sh" | "bash" | "zsh" => Some("shell"),
        "sql" => Some("sql"),
        "go" => Some("go"),
        "c" | "h" => Some("c"),
        "cpp" | "hpp" | "cc" => Some("cpp"),
        "java" => Some("java"),
        "rb" => Some("ruby"),
        "lua" => Some("lua"),
        "zig" => Some("zig"),
        "nix" => Some("nix"),
        "xml" => Some("xml"),
        "md" | "markdown" => Some("markdown"),
        "zip" | "tar" | "gz" | "bz2" | "xz" => Some("archive"),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => Some("image"),
        "mp4" | "mov" | "webm" | "avi" => Some("video"),
        "mp3" | "wav" | "ogg" | "flac" => Some("audio"),
        "pdf" => Some("pdf"),
        "woff" | "woff2" | "ttf" | "otf" => Some("font"),
        "ipynb" => Some("jupyter"),
        _ => None,
    }
}

/// Format byte size into human-readable string.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Detect if content is in Logseq outliner format (majority of lines are bullets).
fn looks_like_outliner(content: &str) -> bool {
    let non_empty: Vec<&str> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .collect();

    if non_empty.is_empty() {
        return false;
    }

    let bullet_count = non_empty
        .iter()
        .filter(|l| {
            let trimmed = l.trim_start();
            trimmed.starts_with("- ")
        })
        .count();

    (bullet_count as f64 / non_empty.len() as f64) > 0.5
}

/// Known media/binary extensions that should be served as static files.
fn is_media_extension(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "avif"
            | "mp4" | "webm" | "ogg" | "mp3" | "wav" | "flac"
            | "pdf" | "zip" | "tar" | "gz" | "woff" | "woff2" | "ttf" | "eot"
    )
}

/// Resolve a relative URL against a base directory path.
/// Returns the resolved path with `../` traversals applied.
fn resolve_relative_url<'a>(url: &'a str, base: &str) -> String {
    if url.starts_with("../") {
        let mut parts: Vec<&str> = base.split('/').collect();
        let mut rel = url;
        while let Some(rest) = rel.strip_prefix("../") {
            parts.pop();
            rel = rest;
        }
        if parts.is_empty() {
            rel.to_string()
        } else {
            format!("{}/{}", parts.join("/"), rel)
        }
    } else {
        format!("{}/{}", base, url)
    }
}

/// Rewrite relative links and media references in subgraph pages.
/// Handles markdown links, markdown images, and HTML src/href attributes.
/// `is_readme` indicates this page came from a README.md (directory page),
/// so relative links resolve against the page name itself, not its parent.
fn rewrite_relative_links(content: &str, page_name: &str, is_readme: bool) -> String {
    use regex::Regex;

    lazy_static::lazy_static! {
        // Markdown link: [text](url) — preceded by non-! character
        static ref MD_LINK: Regex = Regex::new(
            r"(^|[^!])\[([^\]]*)\]\(([^)]+)\)"
        ).unwrap();
        // Markdown image: ![alt](url)
        static ref MD_IMG: Regex = Regex::new(
            r"!\[([^\]]*)\]\(([^)]+)\)"
        ).unwrap();
        // HTML src="..." or href="..." (in img, a, video, source tags)
        static ref HTML_ATTR: Regex = Regex::new(
            r#"((?:src|href)\s*=\s*")([^"]+)(")"#
        ).unwrap();
    }

    // Base directory of this page within the subgraph namespace.
    // README-backed pages (directory pages) use the page name as the base,
    // since relative links in a README resolve from its directory.
    // Regular pages use the parent directory.
    let base = if is_readme {
        page_name
    } else if let Some(pos) = page_name.rfind('/') {
        &page_name[..pos]
    } else {
        page_name
    };

    // Subgraph name is the first path component
    let subgraph_name = page_name.split('/').next().unwrap_or(page_name);

    // Transparent graph-dir prefixes (mirror scanner::subgraph::resolve_subgraph_graph_dir).
    // The scanner strips these from page names, so a link like `root/foo.md`
    // resolved to `<subgraph>/root/foo` must collapse to `<subgraph>/foo`.
    let strip_graph_dir = |resolved: &str| -> String {
        for d in ["root", "graph", "pages"] {
            let prefix = format!("{}/{}/", subgraph_name, d);
            if let Some(rest) = resolved.strip_prefix(&prefix) {
                return format!("{}/{}", subgraph_name, rest);
            }
        }
        resolved.to_string()
    };

    let is_external = |url: &str| -> bool {
        url.starts_with("http://")
            || url.starts_with("https://")
            || url.starts_with('#')
            || url.starts_with('/')
            || url.starts_with("data:")
            || url.starts_with("mailto:")
    };

    // 1. Rewrite markdown images → /media/{subgraph}/path
    let content = MD_IMG.replace_all(&content, |caps: &regex::Captures| {
        let alt = &caps[1];
        let url = &caps[2];
        if is_external(url) {
            return caps[0].to_string();
        }
        let resolved = resolve_relative_url(url, base);
        // Strip subgraph prefix to get repo-relative path for media URL
        let repo_relative = resolved
            .strip_prefix(&format!("{}/", subgraph_name))
            .unwrap_or(&resolved);
        format!("![{}](/media/{}/{})", alt, subgraph_name, repo_relative)
    });

    // 2. Rewrite markdown links → /slugified-path
    let content = MD_LINK.replace_all(&content, |caps: &regex::Captures| {
        let prefix = &caps[1];
        let text = &caps[2];
        let raw_url = &caps[3];
        if is_external(raw_url) {
            return caps[0].to_string();
        }

        // Split off #fragment before resolving
        let (url, fragment): (&str, &str) = match raw_url.find('#') {
            Some(pos) => (&raw_url[..pos], &raw_url[pos..]),
            None => (raw_url, ""),
        };

        // Empty path with fragment only (e.g., "#section") — already handled by is_external
        // but handle the case where url is empty after split
        if url.is_empty() {
            return caps[0].to_string();
        }

        let resolved = resolve_relative_url(url, base);

        // Media files link to the static copy
        if is_media_extension(&resolved) {
            let repo_relative = resolved
                .strip_prefix(&format!("{}/", subgraph_name))
                .unwrap_or(&resolved);
            return format!("{}[{}](/media/{}/{}{})", prefix, text, subgraph_name, repo_relative, fragment);
        }

        // Page links: strip the transparent root/graph/pages prefix the scanner
        // hides, then slugify. Without this, [foo](root/foo.md) from a README
        // resolves to /<sg>/root/foo, but the page lives at /<sg>/foo.
        let resolved = strip_graph_dir(&resolved);
        let resolved = resolved
            .strip_suffix(".md")
            .or_else(|| resolved.strip_suffix(".markdown"))
            .unwrap_or(&resolved)
            .to_string();
        let resolved = resolved
            .strip_suffix("/index")
            .unwrap_or(&resolved)
            .trim_end_matches('/')
            .to_string();
        let slug = slugify_page_name(&resolved);
        format!("{}[{}](/{slug}{})", prefix, text, fragment)
    });

    // 3. Rewrite HTML src="..." and href="..." attributes
    let content = HTML_ATTR.replace_all(&content, |caps: &regex::Captures| {
        let attr_prefix = &caps[1]; // e.g., `src="`
        let raw_url = &caps[2];
        let quote_end = &caps[3]; // closing `"`
        if is_external(raw_url) {
            return caps[0].to_string();
        }

        // Split off #fragment
        let (url, fragment): (&str, &str) = match raw_url.find('#') {
            Some(pos) => (&raw_url[..pos], &raw_url[pos..]),
            None => (raw_url, ""),
        };
        if url.is_empty() {
            return caps[0].to_string();
        }

        let resolved = resolve_relative_url(url, base);
        let repo_relative = resolved
            .strip_prefix(&format!("{}/", subgraph_name))
            .unwrap_or(&resolved);

        if is_media_extension(url) {
            format!("{}/media/{}/{}{}{}", attr_prefix, subgraph_name, repo_relative, fragment, quote_end)
        } else {
            let resolved = strip_graph_dir(&resolved);
            let resolved = resolved
                .strip_suffix(".md")
                .or_else(|| resolved.strip_suffix(".markdown"))
                .unwrap_or(&resolved)
                .to_string();
            let slug = slugify_page_name(&resolved);
            format!("{}/{}{}{}", attr_prefix, slug, fragment, quote_end)
        }
    });

    content.to_string()
}

/// Demote every ATX heading in `md` by one level (h1→h2, h2→h3, …, h6 stays).
/// Skips lines inside fenced code blocks so shell `# comments` and the like
/// aren't accidentally treated as headings.
pub fn demote_headings(md: &str) -> String {
    let mut out = String::with_capacity(md.len() + 32);
    let mut in_fence = false;
    let mut fence_char: Option<u8> = None;
    for line in md.split_inclusive('\n') {
        let trimmed = line.trim_start();
        // Fence open/close detection — supports ``` and ~~~ runs of >= 3.
        let fence_run = trimmed
            .as_bytes()
            .iter()
            .take_while(|&&b| b == b'`' || b == b'~')
            .copied()
            .collect::<Vec<u8>>();
        if fence_run.len() >= 3 && fence_run.iter().all(|&b| b == fence_run[0]) {
            if !in_fence {
                in_fence = true;
                fence_char = Some(fence_run[0]);
            } else if fence_char == Some(fence_run[0]) {
                in_fence = false;
                fence_char = None;
            }
            out.push_str(line);
            continue;
        }
        if !in_fence {
            let bytes = trimmed.as_bytes();
            let hashes = bytes.iter().take_while(|&&b| b == b'#').count();
            if (1..=5).contains(&hashes)
                && (bytes.get(hashes) == Some(&b' ') || bytes.get(hashes) == Some(&b'\t'))
            {
                let leading_ws = line.len() - trimmed.len();
                out.push_str(&line[..leading_ws]);
                out.push('#');
                out.push_str(trimmed);
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

/// Compose the merged page body for a subgraph declaration.
/// Root-graph content sits first, then a horizontal rule, then a
/// level-1 heading naming the subgraph, then the README content
/// with every heading demoted by one (so a README's own h1s
/// become h2s under the divider — proper hierarchy).
pub fn merge_subgraph_content(root_md: &str, subgraph_name: &str, readme_md: &str) -> String {
    let mut out = String::with_capacity(root_md.len() + readme_md.len() + 64);
    out.push_str(root_md);
    out.push_str(&format!("\n\n---\n\n# from subgraph {}\n\n", subgraph_name));
    out.push_str(&demote_headings(readme_md));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(
            slugify_page_name("Collective Focus Theorem"),
            "collective-focus-theorem"
        );
        assert_eq!(
            slugify_page_name("projects/Cyber Valley"),
            "projects/cyber-valley"
        );
        assert_eq!(slugify_page_name("2025-02-08"), "2025-02-08");
        assert_eq!(slugify_page_name("$BOOT"), "$boot");
        assert_eq!(slugify_page_name("$PUSSY on $SOL"), "$pussy-on-$sol");
        assert_eq!(slugify_page_name(".moon names"), ".moon-names");

        // NFC and NFD forms of ö must produce the same slug
        let nfc = "G\u{00F6}del prison"; // ö as single codepoint
        let nfd = "Go\u{0308}del prison"; // o + combining diaeresis
        assert_eq!(
            slugify_page_name(nfc),
            slugify_page_name(nfd),
            "NFC and NFD slugs must match"
        );
    }

    #[test]
    fn test_extension_to_lang() {
        assert_eq!(extension_to_lang("rs"), "rust");
        assert_eq!(extension_to_lang("nu"), "nu");
        assert_eq!(extension_to_lang("py"), "python");
        assert_eq!(extension_to_lang("unknown_ext"), "");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn test_extract_namespace_simple() {
        // "cyber/core" → Some("cyber")
        assert_eq!(extract_namespace("cyber/core"), Some("cyber".to_string()));
    }

    #[test]
    fn test_extract_namespace_deep() {
        // "a/b/c/page" → Some("a/b/c")
        assert_eq!(
            extract_namespace("a/b/c/page"),
            Some("a/b/c".to_string())
        );
    }

    #[test]
    fn test_extract_namespace_root() {
        // "page" → None (no namespace)
        assert_eq!(extract_namespace("page"), None);
        // Also test empty string
        assert_eq!(extract_namespace("simple-page"), None);
    }
}
