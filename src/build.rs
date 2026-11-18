// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
//! The build: one implementation of scan → parse → ingest → graph → render →
//! write, used by `optica build` and by `optica serve`. It lives here rather
//! than in the live-reload module so that building a site does not depend on
//! the server: the watcher adopts the `BuildSeed` this returns, and nothing
//! points the other way.

use crate::config::SiteConfig;
use crate::parser::ParsedPage;
use crate::render::RenderedPage;
use anyhow::Result;
use colored::Colorize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Everything the initial build produced that the watcher would otherwise
/// recompute in its warm-up: the parses, the store, the rendered pages.
pub struct BuildSeed {
    pub discovered: crate::scanner::DiscoveredFiles,
    pub root_parse: HashMap<PathBuf, (SystemTime, ParsedPage)>,
    pub subgraph_parse_caches: HashMap<String, crate::scanner::subgraph::SubgraphParseCache>,
    pub subgraph_pages_by_name: HashMap<String, Vec<ParsedPage>>,
    pub subgraph_repo_by_name: HashMap<String, PathBuf>,
    pub store: crate::graph::PageStore,
    pub rendered: Vec<RenderedPage>,
}

/// The full build: scan, parse, ingest the subgraphs, graph, render, write,
/// media — and return what the watcher needs to continue from here.
pub fn build_full(config: &SiteConfig, quiet: bool, subgraphs_override: Option<&Path>) -> Result<BuildSeed> {
    let start = std::time::Instant::now();
    if !quiet {
        println!("{} {}", "Building".cyan().bold(), config.site.title);
    }
    let discovered = crate::scanner::scan(&config.build.input_dir, &config.content)?;
    if !quiet {
        println!(
            "  {} Discovered {} pages, {} journals, {} media, {} files",
            "Scan".dimmed(), discovered.pages.len(), discovered.journals.len(),
            discovered.media.len(), discovered.files.len()
        );
    }
    // Root pages one file at a time, so the watcher inherits the per-file parse.
    let mut root_parse: HashMap<PathBuf, (SystemTime, ParsedPage)> = HashMap::new();
    let mut parsed_pages: Vec<ParsedPage> = Vec::new();
    for file in discovered.pages.iter().chain(discovered.journals.iter()) {
        let mtime = std::fs::metadata(&file.path).ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
        let page = crate::parser::parse_file(file)?;
        root_parse.insert(file.path.clone(), (mtime, page.clone()));
        parsed_pages.push(page);
    }
    parsed_pages.extend(crate::parser::parse_all(&crate::scanner::DiscoveredFiles {
        pages: vec![], journals: vec![], media: discovered.media.clone(), files: discovered.files.clone(),
    })?);
    if !quiet {
        println!("  {} Parsed {} pages", "Parse".dimmed(), parsed_pages.len());
    }
    let subgraph_decls = crate::scanner::subgraph::load_subgraph_decls(subgraphs_override)?;
    let mut subgraph_parse_caches: HashMap<String, crate::scanner::subgraph::SubgraphParseCache> = HashMap::new();
    let mut subgraph_pages_by_name: HashMap<String, Vec<ParsedPage>> = HashMap::new();
    let mut subgraph_repo_by_name: HashMap<String, PathBuf> = HashMap::new();
    if !subgraph_decls.is_empty() {
        if !quiet {
            if let Some(path) = subgraphs_override {
                println!("  {} Loaded subgraphs from {}", "Config".dimmed(), path.display());
            }
        }
        let mounts: Vec<String> = subgraph_decls.iter().map(|d| d.mount.clone()).filter(|m| !m.is_empty()).collect();
        let evicted = crate::scanner::subgraph::enforce_namespace_monopoly(&mut parsed_pages, &mounts);
        if !quiet {
            for (id, reason) in &evicted {
                println!("  {} Evicted '{}': {}", "Monopoly".yellow(), id, reason);
            }
        }
        for decl in &subgraph_decls {
            subgraph_repo_by_name.insert(decl.name.clone(), decl.repo_path.clone());
            let sg_cache = subgraph_parse_caches.entry(decl.name.clone()).or_default();
            let ingestion = crate::scanner::subgraph::ingest_subgraph_cached(decl, &mut parsed_pages, sg_cache)?;
            if !quiet {
                println!(
                    "  {} Subgraph '{}': {} pages, {} files",
                    "Scan".dimmed(), ingestion.stats.name, ingestion.stats.page_count, ingestion.stats.file_count
                );
            }
            subgraph_pages_by_name.insert(decl.name.clone(), ingestion.pages.clone());
            parsed_pages.extend(ingestion.pages);
        }
    }
    let subgraph_names: Vec<String> = subgraph_decls.iter().map(|d| d.name.clone()).collect();
    crate::parser::synthesize_dir_indexes(&mut parsed_pages, &subgraph_names);
    let (count, map_path) = crate::parser::apply_ipfs_rewrites_for_config(&mut parsed_pages, config)?;
    if !quiet {
        if let Some(p) = map_path {
            println!("  {} Rewrote {} media ref{} via {}", "IPFS".dimmed(), count, if count == 1 { "" } else { "s" }, p.display());
        }
    }
    let t_parsed = start.elapsed();
    let mut store = crate::graph::build_graph(parsed_pages)?;
    let t_graphed = start.elapsed();
    for decl in &subgraph_decls {
        if decl.is_private {
            store.subgraph_private.insert(decl.name.clone());
        }
    }
    if !quiet {
        let total_links: usize = store.forward_links.values().map(|v| v.len()).sum();
        let public_count = store.public_pages(&config.content).len();
        let total_count = store.pages.len();
        println!("  {} Built graph with {} links", "Graph".dimmed(), total_links);
        if config.content.public_only && public_count < total_count {
            println!(
                "  {} {}/{} pages are public (set default_public = true or add public:: true to pages)",
                "Filter".yellow(), public_count, total_count
            );
        }
    }
    let rendered = crate::render::render_all(&store, config)?;
    let t_rendered = start.elapsed();
    if !quiet {
        println!("  {} Rendered {} pages", "Render".dimmed(), rendered.len());
    }
    crate::output::write_output(&rendered, &store, config, &discovered)?;
    let t_written = start.elapsed();
    for decl in &subgraph_decls {
        crate::output::media::copy_subgraph_media(decl, &config.build.output_dir)?;
    }
    if !quiet {
        println!(
            "  {} scan+parse {:.1}s · graph {:.1}s · render {:.1}s · write {:.1}s",
            "Phases".dimmed(),
            t_parsed.as_secs_f64(), (t_graphed - t_parsed).as_secs_f64(),
            (t_rendered - t_graphed).as_secs_f64(), (t_written - t_rendered).as_secs_f64()
        );
        println!(
            "{} Built in {:.2}s → {}",
            "Done!".green().bold(), start.elapsed().as_secs_f64(), config.build.output_dir.display()
        );
    }
    Ok(BuildSeed { discovered, root_parse, subgraph_parse_caches, subgraph_pages_by_name, subgraph_repo_by_name, store, rendered })
}
