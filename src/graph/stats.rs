// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use crate::parser::ParsedPage;
use rayon::prelude::*;
use std::path::Path;

/// Count source-line equivalents for a single file.
/// Binary files (NUL byte in the first 8 KB) count as 1 — the user
/// rule is "binary can be counted as 1 line", which keeps assets
/// (images, archives, fonts) from skewing the line total to zero
/// while not letting them inflate it either.
/// Empty file → 0.
pub fn count_lines(path: &Path) -> u64 {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    if bytes.is_empty() {
        return 0;
    }
    let probe_end = bytes.len().min(8192);
    if bytes[..probe_end].contains(&0) {
        return 1;
    }
    let nls = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    if *bytes.last().unwrap() == b'\n' {
        nls
    } else {
        nls + 1
    }
}

/// Compute graph-wide totals for size and lines across the public
/// page set. Parallelised because we touch the filesystem ~22k times
/// — sequential reads add ~10s to the build, par_iter cuts it to ~2s.
pub fn compute_global_stats(pages: &[&ParsedPage]) -> (u64, u64) {
    pages
        .par_iter()
        .map(|p| {
            if p.source_path.as_os_str().is_empty() {
                return (0u64, 0u64);
            }
            let size = std::fs::metadata(&p.source_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let lines = count_lines(&p.source_path);
            (size, lines)
        })
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}
