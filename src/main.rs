// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::{Path, PathBuf};

use optica::config::SiteConfig;
use optica::graph::PageStore;

#[derive(Parser)]
#[command(name = "cyber-publish")]
#[command(
    version,
    about = "A Rust-native static site publisher for the cyber knowledge graph"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to config file
    #[arg(short, long, default_value = "publish.toml")]
    config: PathBuf,

    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Suppress output
    #[arg(short, long)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Build the static site
    Build {
        /// Logseq graph directory
        #[arg(default_value = ".")]
        input: PathBuf,

        /// Output directory
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Include non-public pages
        #[arg(long)]
        drafts: bool,

        /// Override base URL
        #[arg(long)]
        base_url: Option<String>,

        /// Path to subgraphs TOML. When set, replaces frontmatter-based
        /// subgraph discovery. Each entry: { name, path, exclude[] }.
        #[arg(long)]
        subgraphs: Option<PathBuf>,

        /// Path to JSON mapping filename → IPFS CID. When set, rewrites
        /// `../media/<name>` references in markdown to gateway IPFS URLs.
        #[arg(long)]
        ipfs_map: Option<PathBuf>,

        /// IPFS gateway base URL used with --ipfs-map.
        #[arg(long, default_value = "https://gateway.pinata.cloud")]
        ipfs_gateway: String,
    },

    /// Build and serve with live reload
    Serve {
        /// Logseq graph directory
        #[arg(default_value = ".")]
        input: PathBuf,

        /// Output directory
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Server port (overrides base_url port from config)
        #[arg(short, long)]
        port: Option<u16>,

        /// Bind address
        #[arg(short, long, default_value = "127.0.0.1")]
        bind: String,

        /// Disable live reload
        #[arg(long)]
        no_reload: bool,

        /// Open browser automatically
        #[arg(long)]
        open: bool,

        /// Include non-public pages
        #[arg(long)]
        drafts: bool,

        /// Path to subgraphs TOML. When set, replaces frontmatter-based
        /// subgraph discovery. Each entry: { name, path, exclude[] }.
        #[arg(long)]
        subgraphs: Option<PathBuf>,

        /// Path to JSON mapping filename → IPFS CID. When set, rewrites
        /// `../media/<name>` references in markdown to gateway IPFS URLs.
        #[arg(long)]
        ipfs_map: Option<PathBuf>,

        /// IPFS gateway base URL used with --ipfs-map.
        #[arg(long, default_value = "https://gateway.pinata.cloud")]
        ipfs_gateway: String,
    },

    /// Initialize a new publish.toml config
    Init {
        /// Directory to initialize in
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Validate graph and report broken links
    Check {
        /// Logseq graph directory
        #[arg(default_value = ".")]
        input: PathBuf,

        /// Path to subgraphs TOML. When set, replaces frontmatter-based
        /// subgraph discovery. Each entry: { name, path, exclude[] }.
        #[arg(long)]
        subgraphs: Option<PathBuf>,
    },

    /// Compile cyberlinks into graph-native transformer embeddings
    Compile {
        /// Path to cyberlinks JSONL file
        input: PathBuf,
        /// Path to neuron stakes JSON (optional)
        #[arg(long)]
        stakes: Option<PathBuf>,
        /// Output path for embeddings
        #[arg(short, long, default_value = "bostrom_model.bin")]
        output: PathBuf,
        /// Max singular vectors to compute
        #[arg(short, long, default_value = "100")]
        k: usize,
    },

    /// Query the compiled model — pure graph intelligence
    Query {
        /// Path to compiled model file
        #[arg(default_value = "bostrom_model.bin")]
        model: PathBuf,
        /// Text query
        query: String,
        /// Path to CID text index JSON
        #[arg(long)]
        index: Option<PathBuf>,
        /// Number of neighbors
        #[arg(short, long, default_value = "15")]
        k: usize,
        /// Query mode: neighbors, role, or full (default)
        #[arg(long, default_value = "full")]
        mode: String,
    },
}

/// Try to extract port from a URL like "http://localhost:8888"
fn port_from_url(url: &str) -> Option<u16> {
    // Look for :PORT at the end
    if let Some(pos) = url.rfind(':') {
        let after_colon = &url[pos + 1..];
        // Strip trailing slash
        let port_str = after_colon.trim_end_matches('/');
        port_str.parse::<u16>().ok()
    } else {
        None
    }
}

fn resolve_config(cli_config: &PathBuf, input: &Path) -> (PathBuf, SiteConfig) {
    let config_path = if cli_config.is_relative() {
        input.join(cli_config)
    } else {
        cli_config.clone()
    };
    let mut config = SiteConfig::load(&config_path).unwrap_or_default();
    config.build.input_dir = input.to_path_buf();

    // Resolve output_dir relative to input_dir if it's relative
    if config.build.output_dir.is_relative() {
        config.build.output_dir = input.join(&config.build.output_dir);
    }

    (config_path, config)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            input,
            output,
            drafts,
            base_url,
            subgraphs,
            ipfs_map,
            ipfs_gateway,
        } => {
            let (_config_path, mut config) = resolve_config(&cli.config, &input);
            if let Some(ref out) = output {
                config.build.output_dir = out.clone();
            }
            if let Some(ref url) = base_url {
                config.site.base_url = url.clone();
            }
            if drafts {
                config.content.public_only = false;
            }
            config.media.ipfs_map = ipfs_map;
            config.media.ipfs_gateway = ipfs_gateway;

            build_site(&config, cli.quiet, subgraphs.as_deref())?;
        }
        Commands::Serve {
            input,
            output,
            port,
            bind,
            no_reload,
            open,
            drafts,
            subgraphs,
            ipfs_map,
            ipfs_gateway,
        } => {
            let (_config_path, mut config) = resolve_config(&cli.config, &input);

            if let Some(ref out) = output {
                config.build.output_dir = out.clone();
            }

            // Resolve port: CLI flag > config base_url > default 8080
            let port = port
                .or_else(|| port_from_url(&config.site.base_url))
                .unwrap_or(8080);

            config.site.base_url = format!("http://{}:{}", bind, port);

            if drafts {
                config.content.public_only = false;
            }
            config.media.ipfs_map = ipfs_map;
            config.media.ipfs_gateway = ipfs_gateway;

            // Serve immediately: the port opens on the previous snapshot while
            // the fresh build runs in the background. A restart therefore never
            // takes the site down — stale-for-minutes beats dead-for-minutes.
            // The build hands the watcher everything it produced — parses,
            // store, rendered pages — so no warm-up repeats the work.
            let (seed_tx, seed_rx) = std::sync::mpsc::channel();
            {
                let cfg = config.clone();
                let quiet = cli.quiet;
                let subs = subgraphs.clone();
                std::thread::spawn(move || {
                    match optica::server::reload::build_full(&cfg, quiet, subs.as_deref()) {
                        Ok(seed) => { let _ = seed_tx.send(seed); }
                        Err(e) => eprintln!("initial build failed: {e:#}"),
                    }
                });
            }

            optica::server::serve(&config, &bind, port, !no_reload, open, subgraphs.as_deref(), Some(seed_rx))?;
        }
        Commands::Init { path } => {
            std::fs::create_dir_all(&path)?;
            let config_path = path.join("publish.toml");
            if config_path.exists() {
                eprintln!("{} publish.toml already exists", "Warning:".yellow());
                return Ok(());
            }
            let default_config = include_str!("../default-config.toml");
            std::fs::write(&config_path, default_config)?;
            if !cli.quiet {
                println!(
                    "{} Created {}",
                    "Done!".green().bold(),
                    config_path.display()
                );
            }
        }
        Commands::Check { input, subgraphs } => {
            let (_config_path, config) = resolve_config(&cli.config, &input);
            check_site(&config, subgraphs.as_deref())?;
        }
        Commands::Compile {
            input,
            stakes,
            output,
            k,
        } => {
            optica::compile::run_compile(
                &input,
                stakes.as_deref(),
                &output,
                k,
            )?;
        }
        Commands::Query {
            model,
            query,
            index,
            k,
            mode,
        } => {
            let qmode = match mode.as_str() {
                "neighbors" => optica::model_query::QueryMode::Neighbors,
                "role" => optica::model_query::QueryMode::Role,
                _ => optica::model_query::QueryMode::Full,
            };
            optica::model_query::run_query(
                &model,
                &query,
                index.as_deref(),
                k,
                qmode,
            )?;
        }
    }

    Ok(())
}

fn build_site(config: &SiteConfig, quiet: bool, subgraphs_override: Option<&Path>) -> Result<()> {
    optica::server::reload::build_full(config, quiet, subgraphs_override).map(|_| ())
}

/// Copy media/binary files from a subgraph repo to output/media/{subgraph}/.
/// Only copies files with known media extensions.

fn check_site(config: &SiteConfig, subgraphs_override: Option<&Path>) -> Result<()> {
    println!("{} {}", "Checking".cyan().bold(), config.site.title);

    let discovered = optica::scanner::scan(&config.build.input_dir, &config.content)?;
    let mut parsed_pages = optica::parser::parse_all(&discovered)?;

    // Load subgraph decls, enforce monopoly, ingest each — single shared pipeline.
    let subgraph_decls = optica::scanner::subgraph::load_subgraph_decls(subgraphs_override)?;
    if !subgraph_decls.is_empty() {
        let subgraph_namespaces: Vec<String> =
            subgraph_decls.iter().map(|d| d.mount.clone()).filter(|m| !m.is_empty()).collect();
        let evicted = optica::scanner::subgraph::enforce_namespace_monopoly(
            &mut parsed_pages,
            &subgraph_namespaces,
        );
        for (id, reason) in &evicted {
            println!("  {} Evicted '{}': {}", "Monopoly".yellow(), id, reason);
        }
        for decl in &subgraph_decls {
            let ingestion = optica::scanner::subgraph::ingest_subgraph(decl, &mut parsed_pages)?;
            println!("  {} Subgraph '{}' scanned", "Scan".dimmed(), decl.name);
            parsed_pages.extend(ingestion.pages);
        }
    }

    let mut page_store = optica::graph::build_graph(parsed_pages)?;
    for decl in &subgraph_decls {
        if decl.is_private {
            page_store.subgraph_private.insert(decl.name.clone());
        }
    }

    let public_count = page_store.public_pages(&config.content).len();
    let total_count = page_store.pages.len();
    println!(
        "  {} {}/{} pages pass public filter",
        "Pages".dimmed(),
        public_count,
        total_count
    );

    // Broken links grouped by subgraph
    let mut broken_by_subgraph: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();

    for (page_id, links) in &page_store.forward_links {
        if !PageStore::is_page_public(&page_store.pages[page_id], &config.content) {
            continue;
        }
        let subgraph_name = page_store
            .subgraph_pages
            .iter()
            .find(|(_, ids)| ids.contains(page_id))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "root".to_string());

        for link in links {
            if !page_store.pages.contains_key(link) {
                broken_by_subgraph
                    .entry(subgraph_name.clone())
                    .or_default()
                    .push((page_id.clone(), link.clone()));
            }
        }
    }

    let total_broken: usize = broken_by_subgraph.values().map(|v| v.len()).sum();
    if total_broken == 0 {
        println!("{} No broken links found!", "OK".green().bold());
    } else {
        for (sg, broken) in &broken_by_subgraph {
            println!(
                "\n  {} [{}] {} broken link(s):",
                "Broken:".red(),
                sg,
                broken.len()
            );
            for (from, to) in broken {
                println!("    {} → {}", from, to);
            }
        }
        println!(
            "\n{} {} broken link(s) found",
            "Warning:".yellow().bold(),
            total_broken
        );
    }

    // Crystal metadata validation
    let mut crystal_warnings = 0;
    for (page_id, page) in &page_store.pages {
        if page_store.stub_pages.contains(page_id) {
            continue;
        }
        // Skip crystal validation for subgraph pages
        if page.subgraph.is_some() {
            continue;
        }
        for warn in optica::validator::validate_page(page) {
            println!(
                "  {} {} — {}",
                "Invalid:".red(),
                warn.source_path.display(),
                warn.message
            );
            crystal_warnings += 1;
        }
    }

    if crystal_warnings == 0 {
        println!(
            "{} Crystal metadata valid on all pages!",
            "OK".green().bold()
        );
    } else {
        println!(
            "\n{} {} crystal metadata warning(s) found",
            "Warning:".yellow().bold(),
            crystal_warnings
        );
    }

    Ok(())
}
