// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
//! Load subgraph declarations from a TOML config file instead of discovering
//! them via frontmatter in graph pages. Used by `optica build --subgraphs <path>`.
//!
//! The org-workspace model (see cyberia-to/.github/SPEC.md) keeps the census
//! of repositories in the workspace root's `subgraphs.toml`. `build.nu`
//! materializes a config listing absolute paths to each subgraph and passes
//! it here. Optica stops needing to know about orgs, GitHub, cloning, or
//! frontmatter-based discovery.
//!
//! A subgraph may declare its own children in a `subgraphs.toml` at its
//! root, in the declaration format of the workspace census (`[[subgraph]]`
//! tables with `name`, `visibility`, `mount`, `archived`, `local-only`, …).
//! The root file then names only the top of each tree — `cyberia` — and
//! `cyberia/subgraphs.toml` names what hangs under it. Children mount under
//! their parent (`cyberia/cyberia-my`), live as sibling checkouts of the
//! parent (`<root_dir>/<name>`) unless `path` says otherwise, inherit a
//! private parent's privacy, and are dropped from a public-only build when
//! private or local-only. A nested file may itself declare children; the
//! walk stops at [`MAX_DEPTH`] and never enters the same checkout twice.

use crate::parser::PageId;
use crate::scanner::subgraph::{SubgraphDecl, DEFAULT_EXCLUDES};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Name of the file a subgraph keeps its children in.
pub const NESTED_FILE: &str = "subgraphs.toml";

/// How deep nested declarations may go (root entries are depth 0).
pub const MAX_DEPTH: usize = 8;

#[derive(Debug, Deserialize, Default)]
pub struct SubgraphsFile {
    /// Set by the materializer for a public deploy: a private entry is left
    /// out wherever it is declared — the root entries of this file, and the
    /// nested ones, which go for local-only too.
    #[serde(default)]
    pub public_only: bool,
    #[serde(default)]
    pub subgraphs: Vec<SubgraphEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SubgraphEntry {
    pub name: String,
    pub path: PathBuf,
    /// URL mount point for this subgraph's pages.
    /// Empty string or absent → use `name` as the prefix (default behavior).
    /// Set to `""` for a root-mounted subgraph (pages at `/`).
    /// Set to `"warriors/zoya"` for pages at `/warriors/zoya/…`.
    #[serde(default)]
    pub mount: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub menu: bool,
    #[serde(default)]
    pub menu_order: Option<i32>,
    #[serde(default)]
    pub commit: Option<String>,
}

/// A subgraph's own `subgraphs.toml`: the declaration format of the
/// workspace census. Both `[[subgraph]]` and `[[subgraphs]]` are accepted.
#[derive(Debug, Deserialize, Default)]
struct NestedFile {
    #[serde(default, alias = "subgraphs")]
    subgraph: Vec<NestedEntry>,
}

#[derive(Debug, Deserialize)]
struct NestedEntry {
    name: String,
    /// The subgraph this one hangs under. Absent = the declaring subgraph.
    /// May name another subgraph already loaded (a sibling declared earlier
    /// in the same file, for a tree deeper than one file per level).
    #[serde(default)]
    parent: Option<String>,
    /// Checkout of the child. Absent = `<parent's directory>/../<name>`, the
    /// sibling layout of the workspace. Relative = relative to the
    /// declaring subgraph's checkout.
    #[serde(default)]
    path: Option<PathBuf>,
    /// URL prefix. Absent = `<parent mount>/<name>`.
    #[serde(default)]
    mount: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    orphan: bool,
    #[serde(default, rename = "local-only")]
    local_only: bool,
    #[serde(default)]
    exclude: Vec<String>,
    /// Nested subgraphs stay out of the nav menu unless they ask.
    #[serde(default)]
    menu: Option<bool>,
}

/// Decls are reloaded on every incremental rebuild of a serve; a checkout
/// that is missing stays missing, so its line goes to the log once.
fn warn_once(key: String, line: String) {
    use std::sync::{Mutex, OnceLock};
    static SAID: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let said = SAID.get_or_init(|| Mutex::new(HashSet::new()));
    if said.lock().map(|mut s| s.insert(key)).unwrap_or(true) {
        eprintln!("{line}");
    }
}

fn is_private(visibility: Option<&str>) -> bool {
    visibility
        .map(|v| v.eq_ignore_ascii_case("private"))
        .unwrap_or(false)
}

fn exclude_patterns(extra: Vec<String>) -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
    patterns.extend(extra);
    patterns
}

pub fn load(config_path: &Path) -> Result<Vec<SubgraphDecl>> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading subgraphs config from {}", config_path.display()))?;
    let parsed: SubgraphsFile = toml::from_str(&raw)
        .with_context(|| format!("parsing subgraphs config from {}", config_path.display()))?;
    let public_only = parsed.public_only;

    let mut decls: Vec<SubgraphDecl> = Vec::new();
    // Depth of every loaded subgraph by name; also the set of loaded names.
    // Depth is carried on the walk: a mount may be set explicitly and then
    // says nothing about where in the tree its subgraph sits. Roots are 0.
    let mut depth_by_name: HashMap<String, usize> = HashMap::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    for entry in parsed.subgraphs {
        // The flag means the same thing in both loops: a public build carries
        // no private subgraph, root or nested.
        if public_only && is_private(entry.visibility.as_deref()) {
            continue;
        }
        let repo_path = entry
            .path
            .canonicalize()
            .unwrap_or_else(|_| entry.path.clone());
        let mount = match &entry.mount {
            Some(m) => m.trim_matches('/').to_string(),
            None => entry.name.clone(),
        };
        let decl = SubgraphDecl {
            name: entry.name.clone(),
            mount,
            repo_path: repo_path.clone(),
            exclude_patterns: exclude_patterns(entry.exclude),
            // No declaring page in the graph — the subgraph's own README
            // becomes its namespace root. Downstream lookups for a declaring
            // page will miss gracefully; that is the intended behavior.
            declaring_page_id: PageId::from(entry.name.clone()),
            is_private: is_private(entry.visibility.as_deref()),
            menu: entry.menu,
            menu_order: entry.menu_order,
        };
        depth_by_name.insert(entry.name, 0);
        visited.insert(repo_path);
        decls.push(decl);
    }

    // Walk the trees: every loaded subgraph may carry a file of children,
    // and every child may carry its own. Breadth by index so children
    // appended during the walk get their turn.
    let mut i = 0;
    while i < decls.len() {
        let parent = decls[i].clone();
        let depth = depth_by_name.get(&parent.name).copied().unwrap_or(0);
        if depth < MAX_DEPTH {
            load_nested(&parent, public_only, &mut decls, &mut depth_by_name, &mut visited)?;
        } else {
            eprintln!(
                "  Warning: subgraph '{}' sits at depth {}; its {} is not read",
                parent.name, depth, NESTED_FILE
            );
        }
        i += 1;
    }
    Ok(decls)
}

fn load_nested(
    parent: &SubgraphDecl,
    public_only: bool,
    decls: &mut Vec<SubgraphDecl>,
    depth_by_name: &mut HashMap<String, usize>,
    visited: &mut HashSet<PathBuf>,
) -> Result<()> {
    let file = parent.repo_path.join(NESTED_FILE);
    if !file.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&file)
        .with_context(|| format!("reading nested subgraphs from {}", file.display()))?;
    let parsed: NestedFile = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            // A typo in one repo's file must not take the whole graph down
            // in serve; the line in the log is the signal.
            eprintln!("  Warning: {} of '{}' is not read: {}", NESTED_FILE, parent.name, e);
            return Ok(());
        }
    };
    let root_dir = parent.repo_path.parent().map(|p| p.to_path_buf());

    for entry in parsed.subgraph {
        if entry.archived || entry.orphan {
            // Otherwise the repo leaves the graph silently: a 404 with
            // nothing in the log to say why. Said once, as the state holds.
            warn_once(
                format!("skipped:{}", entry.name),
                format!(
                    "  Warning: subgraph '{}' (declared in {} of '{}') is {}; skipped",
                    entry.name,
                    NESTED_FILE,
                    parent.name,
                    if entry.archived { "archived" } else { "an orphan" }
                ),
            );
            continue;
        }
        if entry.name == parent.name {
            continue; // a census line for the file's own repo, as the root file keeps one
        }
        if depth_by_name.contains_key(&entry.name) {
            eprintln!(
                "  Warning: subgraph '{}' declared again in {} of '{}'; the first declaration stands",
                entry.name, NESTED_FILE, parent.name
            );
            continue;
        }
        // The parent this entry hangs under: the declaring subgraph, or a
        // subgraph already loaded that the entry names.
        let under: SubgraphDecl = match &entry.parent {
            Some(p) if p != &parent.name => match decls.iter().find(|d| &d.name == p) {
                Some(d) => d.clone(),
                None => {
                    eprintln!(
                        "  Warning: subgraph '{}' names parent '{}', which is not loaded; skipped",
                        entry.name, p
                    );
                    continue;
                }
            },
            _ => parent.clone(),
        };
        let depth = depth_by_name.get(&under.name).copied().unwrap_or(0) + 1;
        if depth > MAX_DEPTH {
            eprintln!(
                "  Warning: subgraph '{}' would sit deeper than {}; skipped",
                entry.name, MAX_DEPTH
            );
            continue;
        }
        let private = under.is_private || is_private(entry.visibility.as_deref());
        if public_only && (private || entry.local_only) {
            continue;
        }
        let path = match &entry.path {
            Some(p) if p.is_absolute() => p.clone(),
            Some(p) => parent.repo_path.join(p),
            None => match &root_dir {
                Some(dir) => dir.join(&entry.name),
                None => continue,
            },
        };
        if !path.is_dir() {
            warn_once(
                format!("missing:{}", entry.name),
                format!(
                    "  Warning: subgraph '{}' (declared in {} of '{}') has no checkout at {}; skipped",
                    entry.name, NESTED_FILE, parent.name, path.display()
                ),
            );
            continue;
        }
        let repo_path = path.canonicalize().unwrap_or(path);
        if visited.contains(&repo_path) {
            eprintln!(
                "  Warning: subgraph '{}' points at {} which is already a subgraph; skipped",
                entry.name, repo_path.display()
            );
            continue;
        }
        let mount = match &entry.mount {
            Some(m) => m.trim_matches('/').to_string(),
            None if under.mount.is_empty() => entry.name.clone(),
            None => format!("{}/{}", under.mount, entry.name),
        };
        let decl = SubgraphDecl {
            name: entry.name.clone(),
            mount,
            repo_path: repo_path.clone(),
            exclude_patterns: exclude_patterns(entry.exclude),
            declaring_page_id: PageId::from(entry.name.clone()),
            is_private: private,
            menu: entry.menu.unwrap_or(false),
            menu_order: None,
        };
        depth_by_name.insert(entry.name, depth);
        visited.insert(repo_path);
        decls.push(decl);
    }
    Ok(())
}

/// Read just the set of subgraph names that end up private — declared so in
/// the config, or hanging under a private parent in a nested file.
/// Returns an empty set if the file is absent or unparseable — callers that
/// only need this subset should not fail when subgraph config is missing.
pub fn load_private_names(config_path: &Path) -> HashSet<String> {
    match load(config_path) {
        Ok(decls) => decls
            .into_iter()
            .filter(|d| d.is_private)
            .map(|d| d.name)
            .collect(),
        Err(_) => HashSet::new(),
    }
}

#[cfg(test)]
mod tests;
