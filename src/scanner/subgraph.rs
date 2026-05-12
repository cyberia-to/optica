// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use crate::parser::{PageId, ParsedPage};
use crate::scanner::{DiscoveredFile, DiscoveredFiles, FileKind};
use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Declaration of an external repository to include as a subgraph.
#[derive(Debug, Clone)]
pub struct SubgraphDecl {
    pub name: String,
    pub repo_path: PathBuf,
    pub exclude_patterns: Vec<String>,
    pub declaring_page_id: PageId,
    pub is_private: bool,
}

/// Default exclude patterns applied to all subgraphs.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/**",
    "target/**",
    "**/target/**",
    "node_modules/**",
    "**/node_modules/**",
    "build/**",
    "**/build/**",
    "dist/**",
    "**/dist/**",
    ".next/**",
    "**/.next/**",
    "out/**",
    "**/out/**",
    "**/.DS_Store",
    "Cargo.lock",
    "**/Cargo.lock",
    // Web build output. Optica generates its own index.html for every page,
    // so subgraphs containing static-site output conflict with rendering.
    // Knowledge graphs use markdown; HTML in a subgraph is almost always
    // a compiled artifact, not authored content.
    "**/index.html",
    "**/index.htm",
    // Built JS bundles.
    "**/*.min.js",
    "**/*.min.css",
    // Lock files for JS toolchains.
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
    // Vendored dependencies. Not authored content; each file would render
    // to a full HTML page and balloon output size by an order of magnitude.
    "vendor/**",
    "**/vendor/**",
    // Python runtime caches and envs.
    "**/__pycache__/**",
    "**/*.pyc",
    "**/*.pyo",
    "**/.venv/**",
    "**/venv/**",
    // Compiled binaries and object files.
    "**/*.wasm",
    "**/*.so",
    "**/*.dylib",
    "**/*.a",
    "**/*.rlib",
    "**/*.o",
    "**/*.exe",
    "**/*.dll",
    // Coverage and benchmark output.
    "**/coverage/**",
    "**/.nyc_output/**",
];

/// Load subgraph declarations from a TOML config file. With no path, returns
/// an empty list — optica acts on a single repo with no embedded subgraphs.
///
/// One call site for decl loading prevents the regression class where a new
/// code path forgets workspace mode and silently drops every subgraph.
pub fn load_subgraph_decls(subgraphs_path: Option<&Path>) -> Result<Vec<SubgraphDecl>> {
    match subgraphs_path {
        Some(path) => crate::scanner::subgraph_config::load(path),
        None => Ok(Vec::new()),
    }
}

/// Stats reported by `ingest_subgraph` for one decl. Callers print or aggregate.
pub struct SubgraphScanStats {
    pub name: String,
    pub page_count: usize,
    pub file_count: usize,
}

/// Result of ingesting one subgraph: all new pages it contributes (md + non-md)
/// plus a stats summary. The declaring page (if present in `root_pages`) is
/// removed in place so the README produced by this ingestion takes its slot.
pub struct SubgraphIngestion {
    pub pages: Vec<ParsedPage>,
    pub stats: SubgraphScanStats,
}

/// Scan, parse, and merge a single subgraph. Encapsulates every step that used
/// to be duplicated across build, check, watch warm-up, and incremental rebuild:
///
///   1. WalkDir over the subgraph repo, classifying files
///   2. Parse markdown pages; non-md files become code-fence preview pages
///   3. If a page's id matches the decl's `declaring_page_id`, copy the root
///      declaring page's metadata + outgoing links and prepend its body
///   4. Remove the declaring page from `root_pages` so its slot is freed
///
/// One canonical implementation — every caller routes through here.
pub fn ingest_subgraph(
    decl: &SubgraphDecl,
    root_pages: &mut Vec<ParsedPage>,
) -> Result<SubgraphIngestion> {
    let subgraph_files = scan_subgraph(decl)?;
    let page_count = subgraph_files
        .iter()
        .filter(|f| f.kind == FileKind::Page)
        .count();
    let file_count = subgraph_files
        .iter()
        .filter(|f| f.kind == FileKind::File)
        .count();

    // Capture the declaring page so we can hoist its metadata into the README.
    // Cloned because we will later evict it from root_pages.
    let declaring_page = root_pages
        .iter()
        .find(|p| p.id == decl.declaring_page_id)
        .cloned();

    let decl_slug = crate::parser::slugify_page_name(&decl.declaring_page_id);

    let mut pages = Vec::with_capacity(page_count + file_count);

    // Step 1: markdown pages, with README merging the declaring page's metadata
    for file in &subgraph_files {
        if file.kind != FileKind::Page {
            continue;
        }
        let mut page = crate::parser::parse_file(file)?;
        if page.id == decl.declaring_page_id || page.id == decl_slug {
            if let Some(ref dp) = declaring_page {
                page.meta.tags = dp.meta.tags.clone();
                page.meta.aliases = dp.meta.aliases.clone();
                page.meta.properties = dp.meta.properties.clone();
                page.meta.public = dp.meta.public;
                page.meta.icon = dp.meta.icon.clone();
                page.meta.stake = dp.meta.stake;
                if !dp.content_md.trim().is_empty() {
                    let readme_content = std::mem::take(&mut page.content_md);
                    page.content_md = crate::parser::merge_subgraph_content(
                        &dp.content_md,
                        &decl.name,
                        &readme_content,
                    );
                }
                for link in &dp.outgoing_links {
                    if !page.outgoing_links.contains(link) {
                        page.outgoing_links.push(link.clone());
                    }
                }
            }
        }
        pages.push(page);
    }

    // Step 2: evict the declaring page from root — the subgraph README owns it now
    if declaring_page.is_some() {
        root_pages.retain(|p| !(p.id == decl.declaring_page_id && p.subgraph.is_none()));
    }

    // Step 3: non-markdown files as code-preview pages
    let sg_files: Vec<DiscoveredFile> = subgraph_files
        .into_iter()
        .filter(|f| f.kind == FileKind::File)
        .collect();
    let sg_discovered = DiscoveredFiles {
        pages: Vec::new(),
        journals: Vec::new(),
        media: Vec::new(),
        files: sg_files,
    };
    pages.extend(crate::parser::parse_all(&sg_discovered)?);

    Ok(SubgraphIngestion {
        pages,
        stats: SubgraphScanStats {
            name: decl.name.clone(),
            page_count,
            file_count,
        },
    })
}

/// Resolve the graph directory inside a subgraph repo, using the same
/// fallback chain as the main scanner: root → graph → pages → repo root.
fn resolve_subgraph_graph_dir(repo_path: &Path) -> PathBuf {
    for name in &["root", "graph", "pages"] {
        let dir = repo_path.join(name);
        if dir.exists() {
            return dir;
        }
    }
    // No dedicated page directory — pages live at repo root
    repo_path.to_path_buf()
}

/// Scan an external repository and return discovered files under the subgraph namespace.
/// All files are collected; markdown files become Pages, everything else becomes Files.
pub fn scan_subgraph(decl: &SubgraphDecl) -> Result<Vec<DiscoveredFile>> {
    if !decl.repo_path.exists() {
        eprintln!(
            "Warning: subgraph '{}' repo path does not exist: {} — skipping",
            decl.name,
            decl.repo_path.display()
        );
        return Ok(vec![]);
    }

    let graph_dir = resolve_subgraph_graph_dir(&decl.repo_path);

    // Build exclude glob set
    let mut builder = GlobSetBuilder::new();
    for pattern in &decl.exclude_patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    let exclude_set = builder.build()?;

    // Directories to skip entirely — prevents WalkDir from descending into
    // .git/objects, target/, node_modules/ etc. which can contain thousands of files.
    let skip_dirs: std::collections::HashSet<&str> =
        [".git", "target", "node_modules", "build"].into();

    let mut files = Vec::new();

    for entry in WalkDir::new(&decl.repo_path)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !skip_dirs.contains(name.as_ref())
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path().to_path_buf();

        // Get path relative to repo root for exclusion matching
        let relative = path
            .strip_prefix(&decl.repo_path)
            .unwrap_or(&path)
            .to_string_lossy();

        if exclude_set.is_match(relative.as_ref()) {
            continue;
        }

        let is_md = path
            .extension()
            .map(|ext| ext == "md" || ext == "markdown")
            .unwrap_or(false);

        if is_md {
            // Pages inside graph_dir get names relative to graph_dir
            // (strips the root/graph/pages prefix), others relative to repo root
            let base = if path.starts_with(&graph_dir) {
                &graph_dir
            } else {
                &decl.repo_path
            };
            let name = subgraph_page_name(&path, base, &decl.name);
            files.push(DiscoveredFile {
                path,
                kind: FileKind::Page,
                name,
                subgraph: Some(decl.name.clone()),
            });
        } else {
            let name = subgraph_file_name(&path, &decl.repo_path, &decl.name);
            files.push(DiscoveredFile {
                path,
                kind: FileKind::File,
                name,
                subgraph: Some(decl.name.clone()),
            });
        }
    }

    Ok(files)
}

/// Derive page name for a markdown file in a subgraph.
/// README.md at any level becomes the directory's page.
/// e.g., ~/git/trident/README.md         → "trident"
/// e.g., ~/git/trident/docs/README.md    → "trident/docs"
/// e.g., ~/git/trident/src/README.md     → "trident/src"
/// e.g., ~/git/trident/docs/explanation/vision.md → "trident/docs/explanation/vision"
fn subgraph_page_name(path: &Path, repo_root: &Path, subgraph_name: &str) -> String {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    let stem = relative.with_extension("");
    let name = stem.to_string_lossy();

    // README at any level becomes the parent directory's page
    if name.eq_ignore_ascii_case("README") {
        return subgraph_name.to_string();
    }
    if let Some(parent) = name.strip_suffix("/README").or_else(|| name.strip_suffix("/readme")) {
        return format!("{}/{}", subgraph_name, parent);
    }
    // Case-insensitive check for README as last component
    let last = name.rsplit('/').next().unwrap_or(&name);
    if last.eq_ignore_ascii_case("README") {
        let parent = &name[..name.len() - last.len() - 1];
        return format!("{}/{}", subgraph_name, parent);
    }

    format!("{}/{}", subgraph_name, name)
}

/// Derive file name for a non-markdown file in a subgraph (preserves extension).
/// e.g., ~/git/trident/src/main.rs → "trident/src/main.rs"
fn subgraph_file_name(path: &Path, repo_root: &Path, subgraph_name: &str) -> String {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    let name = relative.to_string_lossy().to_string();
    format!("{}/{}", subgraph_name, name)
}

/// Enforce namespace monopoly: remove root pages whose namespace conflicts
/// with a claimed subgraph namespace.
/// Returns list of (evicted_page_id, reason) for reporting.
pub fn enforce_namespace_monopoly(
    root_pages: &mut Vec<ParsedPage>,
    subgraph_namespaces: &[String],
) -> Vec<(PageId, String)> {
    let mut evicted = Vec::new();

    root_pages.retain(|page| {
        if let Some(ref ns) = page.namespace {
            for sg_ns in subgraph_namespaces {
                if ns == sg_ns || ns.starts_with(&format!("{}/", sg_ns)) {
                    evicted.push((
                        page.id.clone(),
                        format!(
                            "namespace '{}' claimed by subgraph '{}'",
                            ns, sg_ns
                        ),
                    ));
                    return false;
                }
            }
        }
        true
    });

    evicted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_subgraph_page_name() {
        let repo = PathBuf::from("/git/trident");
        // Repo-root README maps to just the subgraph name
        assert_eq!(
            subgraph_page_name(&PathBuf::from("/git/trident/README.md"), &repo, "trident"),
            "trident"
        );
        // Nested files keep full path
        assert_eq!(
            subgraph_page_name(
                &PathBuf::from("/git/trident/docs/explanation/vision.md"),
                &repo,
                "trident"
            ),
            "trident/docs/explanation/vision"
        );
        // Directory README becomes the directory page
        assert_eq!(
            subgraph_page_name(
                &PathBuf::from("/git/trident/src/README.md"),
                &repo,
                "trident"
            ),
            "trident/src"
        );
        assert_eq!(
            subgraph_page_name(
                &PathBuf::from("/git/trident/docs/README.md"),
                &repo,
                "trident"
            ),
            "trident/docs"
        );
    }

    #[test]
    fn test_subgraph_file_name() {
        let repo = PathBuf::from("/git/trident");
        assert_eq!(
            subgraph_file_name(&PathBuf::from("/git/trident/src/main.rs"), &repo, "trident"),
            "trident/src/main.rs"
        );
        assert_eq!(
            subgraph_file_name(&PathBuf::from("/git/trident/Cargo.toml"), &repo, "trident"),
            "trident/Cargo.toml"
        );
    }

    /// Regression: workspace mode (TOML --subgraphs) must produce subgraph
    /// pages everywhere — build, check, and incremental reload. The historical
    /// failure was a second code path silently calling the frontmatter
    /// discovery, returning zero decls, and dropping all subgraph content.
    ///
    /// This test exercises the public surface end-to-end: a TOML config on
    /// disk, load_subgraph_decls, then ingest_subgraph. If either piece
    /// regresses, the assertions fail with a clear signal.
    #[test]
    fn test_load_and_ingest_produces_pages_for_toml_subgraph() {
        use std::fs;
        use tempfile::TempDir;

        let workspace = TempDir::new().unwrap();
        let repo = workspace.path().join("mysub");
        fs::create_dir_all(repo.join("root")).unwrap();
        fs::write(
            repo.join("README.md"),
            "---\ntags: doc\n---\n# mysub\n\nrepo readme",
        ).unwrap();
        fs::write(
            repo.join("root").join("inner.md"),
            "---\ntags: doc\n---\n\ninner page",
        ).unwrap();
        fs::write(repo.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

        let config_path = workspace.path().join("subgraphs.toml");
        fs::write(
            &config_path,
            format!(
                "[[subgraphs]]\nname = \"mysub\"\npath = {:?}\n",
                repo
            ),
        ).unwrap();

        // load_subgraph_decls with TOML must produce a non-empty list.
        // Without it (None), it must produce an empty list — never crash.
        let decls = load_subgraph_decls(Some(&config_path)).unwrap();
        assert_eq!(decls.len(), 1, "TOML config with one subgraph should load one decl");
        assert!(load_subgraph_decls(None).unwrap().is_empty(), "no path → no decls");

        // ingest_subgraph must return non-empty pages (md + non-md), proving
        // the full pipeline runs. The historical bug bypassed this entirely.
        let mut root_pages: Vec<ParsedPage> = vec![];
        let ingestion = ingest_subgraph(&decls[0], &mut root_pages).unwrap();
        assert!(
            ingestion.stats.page_count >= 2,
            "expected at least README + inner page, got {} markdown pages",
            ingestion.stats.page_count
        );
        assert!(
            ingestion.stats.file_count >= 1,
            "expected Cargo.toml as a non-md preview page, got {} file pages",
            ingestion.stats.file_count
        );
        assert!(
            !ingestion.pages.is_empty(),
            "ingest_subgraph must produce pages; the bug was zero pages slipping through silently"
        );
        // Pages should carry the subgraph attribution so downstream filters
        // (private subgraph filtering, badge rendering) work.
        assert!(
            ingestion.pages.iter().all(|p| p.subgraph.as_deref() == Some("mysub")),
            "every ingested page must be tagged with the subgraph name"
        );
    }

    #[test]
    fn test_namespace_monopoly_evicts_matching() {
        use crate::parser::{PageKind, PageMeta};
        use std::collections::HashMap;

        let make = |id: &str, ns: Option<&str>| ParsedPage {
            id: id.to_string(),
            meta: PageMeta {
                title: id.to_string(),
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
            namespace: ns.map(|s| s.to_string()),
            subgraph: None,
            content_md: String::new(),
            outgoing_links: vec![],
        };

        let mut pages = vec![
            make("root-page", None),
            make("trident-thesis", None), // root level, no namespace — should NOT be evicted
            make("trident-sub-thing", Some("trident")), // namespace = trident — EVICTED
            make("other-ns-page", Some("cyber")),
        ];

        let evicted = enforce_namespace_monopoly(&mut pages, &["trident".to_string()]);

        assert_eq!(pages.len(), 3);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, "trident-sub-thing");
        // root-level pages with no namespace stay
        assert!(pages.iter().any(|p| p.id == "trident-thesis"));
    }
}
