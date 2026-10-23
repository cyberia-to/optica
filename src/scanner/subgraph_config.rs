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
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Name of the file a subgraph keeps its children in.
pub const NESTED_FILE: &str = "subgraphs.toml";

/// How deep nested declarations may go (root entries are depth 0).
pub const MAX_DEPTH: usize = 8;

#[derive(Debug, Deserialize, Default)]
pub struct SubgraphsFile {
    /// Set by the materializer for a public deploy: nested declarations that
    /// are private or local-only are left out, as the root ones were by the
    /// materializer itself.
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
    let mut names: HashSet<String> = HashSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    for entry in parsed.subgraphs {
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
        names.insert(entry.name);
        visited.insert(repo_path);
        decls.push(decl);
    }

    // Walk the trees: every loaded subgraph may carry a file of children,
    // and every child may carry its own. Breadth by index so children
    // appended during the walk get their turn.
    let mut i = 0;
    while i < decls.len() {
        let parent = decls[i].clone();
        let depth = depth_of(&decls, &parent);
        if depth < MAX_DEPTH {
            load_nested(&parent, depth, public_only, &mut decls, &mut names, &mut visited)?;
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

/// Depth of a decl: how many `/` its mount carries beyond its root ancestor's.
/// A root entry declared with an explicit nested mount counts as nested too,
/// which only makes the depth guard stricter.
fn depth_of(_decls: &[SubgraphDecl], decl: &SubgraphDecl) -> usize {
    decl.mount.matches('/').count()
}

fn load_nested(
    parent: &SubgraphDecl,
    depth: usize,
    public_only: bool,
    decls: &mut Vec<SubgraphDecl>,
    names: &mut HashSet<String>,
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
            continue;
        }
        if entry.name == parent.name {
            continue; // a census line for the file's own repo, as the root file keeps one
        }
        if names.contains(&entry.name) {
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
        if depth + 1 > MAX_DEPTH {
            eprintln!(
                "  Warning: subgraph '{}' would sit deeper than {}; skipped",
                entry.name, MAX_DEPTH
            );
            continue;
        }
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
        names.insert(entry.name);
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A workspace: `root_dir/<name>` checkouts, a materialized config that
    /// lists the top-level ones, and nested files where the test puts them.
    struct Workspace {
        dir: TempDir,
    }

    impl Workspace {
        fn new() -> Self {
            Workspace { dir: TempDir::new().unwrap() }
        }
        fn root(&self) -> PathBuf {
            self.dir.path().canonicalize().unwrap()
        }
        fn repo(&self, name: &str) -> PathBuf {
            let p = self.root().join(name);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("README.md"), format!("# {name}\n")).unwrap();
            p
        }
        fn nested(&self, repo: &str, toml: &str) {
            fs::write(self.root().join(repo).join(NESTED_FILE), toml).unwrap();
        }
        fn materialized(&self, public_only: bool, tops: &[(&str, &str)]) -> PathBuf {
            let mut s = String::new();
            if public_only {
                s.push_str("public_only = true\n");
            }
            for (name, vis) in tops {
                s.push_str(&format!(
                    "[[subgraphs]]\nname = \"{name}\"\npath = {:?}\nvisibility = \"{vis}\"\nmenu = true\n",
                    self.root().join(name)
                ));
            }
            let p = self.root().join("materialized.toml");
            fs::write(&p, s).unwrap();
            p
        }
    }

    fn names(decls: &[SubgraphDecl]) -> Vec<String> {
        decls.iter().map(|d| d.name.clone()).collect()
    }

    #[test]
    fn nested_children_mount_under_their_parent_as_sibling_checkouts() {
        let ws = Workspace::new();
        ws.repo("cyberia");
        ws.repo("cyberia-my");
        ws.repo("cve");
        ws.nested(
            "cyberia",
            "[[subgraph]]\nname = \"cyberia-my\"\nvisibility = \"public\"\n\n[[subgraph]]\nname = \"cve\"\n",
        );
        let cfg = ws.materialized(false, &[("cyberia", "public")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(names(&decls), vec!["cyberia", "cyberia-my", "cve"]);
        let my = &decls[1];
        assert_eq!(my.mount, "cyberia/cyberia-my");
        assert_eq!(my.repo_path, ws.root().join("cyberia-my"));
        assert!(!my.is_private);
        assert!(!my.menu, "nested subgraphs stay out of the menu by default");
        assert_eq!(my.menu_order, None);
        assert_eq!(decls[2].mount, "cyberia/cve");
    }

    #[test]
    fn a_tree_goes_as_deep_as_its_files_and_accepts_both_table_names() {
        let ws = Workspace::new();
        ws.repo("a");
        ws.repo("b");
        ws.repo("c");
        ws.nested("a", "[[subgraph]]\nname = \"b\"\n");
        ws.nested("b", "[[subgraphs]]\nname = \"c\"\n");
        let cfg = ws.materialized(false, &[("a", "public")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(names(&decls), vec!["a", "b", "c"]);
        assert_eq!(decls[2].mount, "a/b/c");
    }

    #[test]
    fn an_explicit_parent_in_the_same_file_nests_deeper() {
        let ws = Workspace::new();
        ws.repo("a");
        ws.repo("b");
        ws.repo("c");
        ws.nested(
            "a",
            "[[subgraph]]\nname = \"b\"\n\n[[subgraph]]\nname = \"c\"\nparent = \"b\"\n",
        );
        let cfg = ws.materialized(false, &[("a", "public")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(decls[2].mount, "a/b/c");
    }

    #[test]
    fn explicit_mount_and_path_win() {
        let ws = Workspace::new();
        ws.repo("a");
        let inner = ws.root().join("a").join("docs").join("inner");
        fs::create_dir_all(&inner).unwrap();
        ws.repo("elsewhere");
        ws.nested(
            "a",
            "[[subgraph]]\nname = \"inner\"\npath = \"docs/inner\"\nmount = \"a/deep/inner\"\n\n[[subgraph]]\nname = \"far\"\npath = \"../elsewhere\"\n",
        );
        let cfg = ws.materialized(false, &[("a", "public")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(decls[1].mount, "a/deep/inner");
        assert_eq!(decls[1].repo_path, inner.canonicalize().unwrap());
        assert_eq!(decls[2].name, "far");
        assert_eq!(decls[2].repo_path, ws.root().join("elsewhere"));
        assert_eq!(decls[2].mount, "a/far");
    }

    #[test]
    fn privacy_is_inherited_and_public_only_drops_private_and_local_only() {
        let ws = Workspace::new();
        ws.repo("pub");
        ws.repo("secret");
        ws.repo("local");
        ws.repo("open");
        ws.repo("priv");
        ws.repo("child-of-priv");
        ws.nested(
            "pub",
            "[[subgraph]]\nname = \"secret\"\nvisibility = \"private\"\n\n[[subgraph]]\nname = \"local\"\nlocal-only = true\n\n[[subgraph]]\nname = \"open\"\n",
        );
        ws.nested("priv", "[[subgraph]]\nname = \"child-of-priv\"\nvisibility = \"public\"\n");

        let cfg = ws.materialized(false, &[("pub", "public"), ("priv", "private")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(
            names(&decls),
            vec!["pub", "priv", "secret", "local", "open", "child-of-priv"]
        );
        let by = |n: &str| decls.iter().find(|d| d.name == n).unwrap();
        assert!(by("secret").is_private);
        assert!(!by("local").is_private);
        assert!(by("child-of-priv").is_private, "a private parent makes its children private");
        let private = load_private_names(&cfg);
        assert!(private.contains("secret") && private.contains("priv") && private.contains("child-of-priv"));
        assert!(!private.contains("open"));

        let cfg = ws.materialized(true, &[("pub", "public")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(names(&decls), vec!["pub", "open"]);
    }

    #[test]
    fn archived_orphan_self_missing_duplicate_and_cycles_are_skipped() {
        let ws = Workspace::new();
        ws.repo("a");
        ws.repo("b");
        ws.repo("dup");
        ws.nested(
            "a",
            concat!(
                "[[subgraph]]\nname = \"a\"\n",                        // the census line for itself
                "[[subgraph]]\nname = \"old\"\narchived = true\n",     // archived: no checkout needed
                "[[subgraph]]\nname = \"gone\"\norphan = true\n",
                "[[subgraph]]\nname = \"never-cloned\"\n",             // no checkout
                "[[subgraph]]\nname = \"b\"\n",
                "[[subgraph]]\nname = \"dup\"\n",
                "[[subgraph]]\nname = \"dup\"\n",                      // declared twice
                "[[subgraph]]\nname = \"also-a\"\npath = \"../a\"\n", // the same checkout again
            ),
        );
        ws.nested("b", "[[subgraph]]\nname = \"a\"\n"); // b claims a: a cycle
        let cfg = ws.materialized(false, &[("a", "public")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(names(&decls), vec!["a", "b", "dup"]);
    }

    #[test]
    fn a_broken_nested_file_is_a_warning_not_a_failure() {
        let ws = Workspace::new();
        ws.repo("a");
        ws.nested("a", "[[subgraph]\nname = ");
        let cfg = ws.materialized(false, &[("a", "public")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(names(&decls), vec!["a"]);
    }

    #[test]
    fn root_entries_are_unchanged_without_nested_files() {
        let ws = Workspace::new();
        ws.repo("solo");
        let cfg = ws.materialized(false, &[("solo", "private")]);
        let decls = load(&cfg).unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].mount, "solo");
        assert!(decls[0].is_private);
        assert!(decls[0].menu);
    }
}
