// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use crate::config::SiteConfig;
use crate::graph::PageStore;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct SearchEntry {
    title: String,
    url: String,
    tags: Vec<String>,
    excerpt: String,
    /// Percentile rank (0..1) of the page's tri-kernel focus.
    /// Pre-normalized so the JS scorer can fold it directly into a
    /// multiplier without knowing the global distribution.
    focus_pct: f64,
    /// Percentile rank (0..1) of the page's gravity
    /// (G_i = π_i × Σ_neighbors π_j/d²) — how strongly a page sits
    /// at the center of an important neighborhood.
    gravity_pct: f64,
}

/// Compute percentile ranks (0..1) for a slice of values.
/// Equal values share the same rank-fraction (within float jitter).
fn percentiles(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n <= 1 {
        return vec![0.0; n];
    }
    let mut indexed: Vec<(usize, f64)> = values.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = vec![0.0; n];
    let divisor = (n - 1) as f64;
    for (rank, (orig_idx, _)) in indexed.iter().enumerate() {
        out[*orig_idx] = rank as f64 / divisor;
    }
    out
}

pub fn generate_search_index(
    store: &PageStore,
    config: &SiteConfig,
    output_dir: &Path,
) -> Result<()> {
    let pages = store.public_pages(&config.content);

    // Collect raw focus/gravity values in page order so the
    // percentile vectors line up with the entries we emit.
    let focus_vals: Vec<f64> = pages
        .iter()
        .map(|p| store.focus.get(&p.id).copied().unwrap_or(0.0))
        .collect();
    let gravity_vals: Vec<f64> = pages
        .iter()
        .map(|p| store.gravity.get(&p.id).copied().unwrap_or(0.0))
        .collect();
    let focus_pcts = percentiles(&focus_vals);
    let gravity_pcts = percentiles(&gravity_vals);

    let entries: Vec<SearchEntry> = pages
        .into_iter()
        .enumerate()
        .map(|(i, page)| {
            let excerpt = crate::render::context::generate_excerpt(&page.content_md, 200);
            // Round to 3 decimals — keeps the JSON small (the index
            // already weighs ~MB) without affecting ranking.
            let round3 = |x: f64| (x * 1000.0).round() / 1000.0;
            SearchEntry {
                title: page.meta.title.clone(),
                url: format!("/{}", page.id),
                tags: page.meta.tags.clone(),
                excerpt,
                focus_pct: round3(focus_pcts[i]),
                gravity_pct: round3(gravity_pcts[i]),
            }
        })
        .collect();

    let json = serde_json::to_string(&entries)?;
    std::fs::write(output_dir.join("search-index.json"), json)?;

    Ok(())
}
