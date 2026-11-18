// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
//! Tests for the subgraphs config loader and the walk of nested
//! `subgraphs.toml` files. Kept beside the module to hold it under the
//! 500-line limit of the quality rules.

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

    // A public build: the private root goes too, with the child under it.
    let cfg = ws.materialized(true, &[("pub", "public"), ("priv", "private")]);
    let decls = load(&cfg).unwrap();
    assert_eq!(names(&decls), vec!["pub", "open"]);
    assert!(load_private_names(&cfg).is_empty());
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

#[test]
fn the_walk_stops_at_max_depth_however_the_mounts_read() {
    // Every child sets a flat mount of its own, so nothing in the mount
    // string says how deep its subgraph sits: the depth is the walk's.
    let ws = Workspace::new();
    let deepest = MAX_DEPTH + 2;
    for i in 0..=deepest {
        ws.repo(&format!("r{i}"));
    }
    for i in 0..deepest {
        ws.nested(
            &format!("r{i}"),
            &format!("[[subgraph]]\nname = \"r{n}\"\nmount = \"r{n}\"\n", n = i + 1),
        );
    }
    let cfg = ws.materialized(false, &[("r0", "public")]);
    let decls = load(&cfg).unwrap();
    let expected: Vec<String> = (0..=MAX_DEPTH).map(|i| format!("r{i}")).collect();
    assert_eq!(names(&decls), expected, "r{} sits at depth {} and is skipped", MAX_DEPTH + 1, MAX_DEPTH + 1);
}

#[test]
fn an_explicit_parent_cannot_reach_past_max_depth() {
    // The depth is the parent's, not the declaring file's: the file of a
    // subgraph at depth 7 may name r8 as the parent of one more entry, and
    // that one sits deeper than the limit and is refused.
    let ws = Workspace::new();
    for i in 0..=MAX_DEPTH {
        ws.repo(&format!("r{i}"));
    }
    ws.repo("late");
    for i in 0..MAX_DEPTH {
        ws.nested(&format!("r{i}"), &format!("[[subgraph]]\nname = \"r{}\"\n", i + 1));
    }
    ws.nested(
        &format!("r{}", MAX_DEPTH - 1),
        &format!(
            "[[subgraph]]\nname = \"r{d}\"\n\n[[subgraph]]\nname = \"late\"\nparent = \"r{d}\"\n",
            d = MAX_DEPTH
        ),
    );
    let cfg = ws.materialized(false, &[("r0", "public")]);
    let decls = load(&cfg).unwrap();
    let loaded = names(&decls);
    assert!(loaded.contains(&format!("r{MAX_DEPTH}")), "the chain reaches depth {MAX_DEPTH}");
    assert!(!loaded.contains(&"late".to_string()));
}
