// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
//! Load subgraph declarations from a TOML config file instead of discovering
//! them via frontmatter in graph pages. Used by `optica build --subgraphs <path>`.
//!
//! The new org-workspace model (see cyberia-to/.github/SPEC.md) keeps
//! subgraph declarations in `.github/subgraphs/` rather than inside the
//! content graph. `build.nu` materializes a config listing absolute paths
//! to each subgraph and passes it here. Optica stops needing to know about
//! orgs, GitHub, cloning, or frontmatter-based discovery.

use crate::parser::PageId;
use crate::scanner::subgraph::SubgraphDecl;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct SubgraphsFile {
    #[serde(default)]
    pub subgraphs: Vec<SubgraphEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SubgraphEntry {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub visibility: Option<String>,
}

const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/**",
    "target/**",
    "**/target/**",
    "node_modules/**",
    "**/node_modules/**",
    "build/**",
    "**/build/**",
    ".claude/**",
    "**/.DS_Store",
    "Cargo.lock",
    "**/Cargo.lock",
];

pub fn load(config_path: &Path) -> Result<Vec<SubgraphDecl>> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading subgraphs config from {}", config_path.display()))?;
    let parsed: SubgraphsFile = toml::from_str(&raw)
        .with_context(|| format!("parsing subgraphs config from {}", config_path.display()))?;

    let decls = parsed
        .subgraphs
        .into_iter()
        .map(|entry| {
            let repo_path = entry
                .path
                .canonicalize()
                .unwrap_or_else(|_| entry.path.clone());
            let mut exclude_patterns: Vec<String> =
                DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
            exclude_patterns.extend(entry.exclude);
            let is_private = entry
                .visibility
                .as_deref()
                .map(|v| v.eq_ignore_ascii_case("private"))
                .unwrap_or(false);
            SubgraphDecl {
                name: entry.name.clone(),
                repo_path,
                exclude_patterns,
                // No declaring page in the graph — the subgraph's own README
                // becomes its namespace root. Downstream lookups for a declaring
                // page will miss gracefully; that is the intended behavior.
                declaring_page_id: PageId::from(entry.name),
                is_private,
            }
        })
        .collect();
    Ok(decls)
}

/// Read just the set of subgraph names marked visibility: private.
/// Returns an empty set if the file is absent or unparseable — callers that
/// only need this subset should not fail when subgraph config is missing.
pub fn load_private_names(config_path: &Path) -> HashSet<String> {
    let Ok(raw) = std::fs::read_to_string(config_path) else {
        return HashSet::new();
    };
    let Ok(parsed) = toml::from_str::<SubgraphsFile>(&raw) else {
        return HashSet::new();
    };
    parsed
        .subgraphs
        .into_iter()
        .filter(|e| {
            e.visibility
                .as_deref()
                .map(|v| v.eq_ignore_ascii_case("private"))
                .unwrap_or(false)
        })
        .map(|e| e.name)
        .collect()
}
