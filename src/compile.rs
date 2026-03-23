// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
//! Compile cyberlinks into graph-native transformer embeddings.
//! Pipeline: load JSONL → sparse adjacency → PageRank (+ spectral gap) → randomized SVD → binary.
//! Zero external linear algebra deps: sparse CSR, Gram-Schmidt, and randomized SVD from scratch.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Deserialize)]
struct Cyberlink {
    particle_from: String,
    particle_to: String,
    #[allow(dead_code)]
    neuron: Option<String>,
}

// ── Sparse CSR matrix ────────────────────────────────────────────────

struct SparseMatrix {
    n: usize,
    row_ptr: Vec<usize>,
    col_idx: Vec<usize>,
    values: Vec<f64>,
}

impl SparseMatrix {
    fn spmv(&self, x: &[f64], y: &mut [f64]) {
        y.iter_mut().for_each(|v| *v = 0.0);
        for i in 0..self.n {
            let mut acc = 0.0;
            for j in self.row_ptr[i]..self.row_ptr[i + 1] {
                acc += self.values[j] * x[self.col_idx[j]];
            }
            y[i] = acc;
        }
    }
    fn spmv_t(&self, x: &[f64], y: &mut [f64]) {
        y.iter_mut().for_each(|v| *v = 0.0);
        for i in 0..self.n {
            if x[i] == 0.0 { continue; }
            for j in self.row_ptr[i]..self.row_ptr[i + 1] {
                y[self.col_idx[j]] += self.values[j] * x[i];
            }
        }
    }
    fn nnz(&self) -> usize { self.values.len() }
}

// ── PRNG (xorshift64) ───────────────────────────────────────────────

struct Xorshift64(u64);
impl Xorshift64 {
    fn new(seed: u64) -> Self { Self(if seed == 0 { 0xDEAD_BEEF_CAFE_BABE } else { seed }) }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        self.0 = x; x
    }
    fn next_gaussian(&mut self) -> f64 {
        let u1 = ((self.next_u64() as f64) / (u64::MAX as f64)).max(1e-15);
        let u2 = (self.next_u64() as f64) / (u64::MAX as f64);
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

// ── Graph loading ────────────────────────────────────────────────────

struct GraphData {
    n: usize,
    adj: SparseMatrix,
    out_degree: Vec<f64>,
    #[cfg_attr(not(test), allow(dead_code))]
    id_to_idx: HashMap<String, usize>,
    idx_to_id: Vec<String>,
}

fn load_cyberlinks(path: &Path) -> Result<GraphData> {
    let reader = BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut id_map: HashMap<String, usize> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();

    let get_idx = |id_map: &mut HashMap<String, usize>, names: &mut Vec<String>, name: &str| -> usize {
        if let Some(&idx) = id_map.get(name) { return idx; }
        let idx = names.len();
        id_map.insert(name.to_string(), idx);
        names.push(name.to_string());
        idx
    };

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() { continue; }
        let link: Cyberlink = serde_json::from_str(line).with_context(|| "parse JSONL line")?;
        let from = get_idx(&mut id_map, &mut names, &link.particle_from);
        let to = get_idx(&mut id_map, &mut names, &link.particle_to);
        edges.push((from, to));
    }

    let n = names.len();
    if n == 0 { anyhow::bail!("no vertices found in cyberlinks file"); }

    // Build CSR
    let mut row_count = vec![0usize; n];
    for &(from, _) in &edges { row_count[from] += 1; }
    let mut row_ptr = vec![0usize; n + 1];
    for i in 0..n { row_ptr[i + 1] = row_ptr[i] + row_count[i]; }
    let nnz = edges.len();
    let mut col_idx = vec![0usize; nnz];
    let values = vec![1.0f64; nnz];
    let mut offset = row_ptr.clone();
    for &(from, to) in &edges {
        col_idx[offset[from]] = to;
        offset[from] += 1;
    }
    let out_degree: Vec<f64> = (0..n).map(|i| (row_ptr[i + 1] - row_ptr[i]) as f64).collect();

    Ok(GraphData {
        n,
        adj: SparseMatrix { n, row_ptr, col_idx, values },
        out_degree, id_to_idx: id_map, idx_to_id: names,
    })
}

// ── PageRank with spectral gap ───────────────────────────────────────

struct PageRankResult { pi: Vec<f64>, spectral_gap: f64, lambda2: f64, iterations: usize }

fn compute_pagerank(adj: &SparseMatrix, out_degree: &[f64], alpha: f64, max_iter: usize) -> PageRankResult {
    let n = adj.n;
    let teleport = (1.0 - alpha) / n as f64;
    let mut pi = vec![1.0 / n as f64; n];
    let mut pi_new = vec![0.0; n];
    let mut diffs: Vec<f64> = Vec::new();
    let mut iters_used = 0;

    for iter in 0..max_iter {
        iters_used = iter + 1;
        pi_new.iter_mut().for_each(|v| *v = teleport);
        let dangling: f64 = pi.iter().enumerate()
            .filter(|(i, _)| out_degree[*i] == 0.0).map(|(_, v)| v).sum();
        pi_new.iter_mut().for_each(|v| *v += alpha * dangling / n as f64);

        for row in 0..n {
            if out_degree[row] == 0.0 { continue; }
            let w = alpha * pi[row] / out_degree[row];
            for j in adj.row_ptr[row]..adj.row_ptr[row + 1] {
                pi_new[adj.col_idx[j]] += w * adj.values[j];
            }
        }
        let sum: f64 = pi_new.iter().sum();
        if sum > 0.0 { pi_new.iter_mut().for_each(|v| *v /= sum); }

        let diff: f64 = pi.iter().zip(pi_new.iter()).map(|(a, b)| (a - b).abs()).sum();
        diffs.push(diff);
        std::mem::swap(&mut pi, &mut pi_new);
        if diff < 1e-10 { break; }
    }

    // Spectral gap from convergence rate
    let mut ratios: Vec<f64> = Vec::new();
    for i in diffs.len().saturating_sub(5)..diffs.len() {
        if i > 0 && diffs[i - 1] > 1e-15 { ratios.push(diffs[i] / diffs[i - 1]); }
    }
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let kappa = if ratios.is_empty() { alpha } else { ratios[ratios.len() / 2] };
    let lambda2 = (1.0 - kappa / alpha).max(0.0).min(1.0);

    PageRankResult { pi, spectral_gap: kappa, lambda2, iterations: iters_used }
}

// ── Gram-Schmidt ─────────────────────────────────────────────────────

fn gram_schmidt(vecs: &mut [Vec<f64>]) {
    for i in 0..vecs.len() {
        for j in 0..i {
            let dot: f64 = vecs[i].iter().zip(vecs[j].iter()).map(|(a, b)| a * b).sum();
            for d in 0..vecs[i].len() { vecs[i][d] -= dot * vecs[j][d]; }
        }
        let norm: f64 = vecs[i].iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm > 1e-14 { vecs[i].iter_mut().for_each(|x| *x /= norm); }
    }
}

// ── Randomized SVD ───────────────────────────────────────────────────
// Top-k singular vectors of A_w = diag(sqrt(pi)) * A * diag(1/sqrt(pi+eps))

struct SvdResult { vectors: Vec<Vec<f64>>, sigmas: Vec<f64> }

fn randomized_svd(adj: &SparseMatrix, pi: &[f64], k: usize, n_power_iter: usize, seed: u64) -> SvdResult {
    let n = adj.n;
    let k = k.min(n);
    let mut rng = Xorshift64::new(seed);
    let eps = 1e-12;
    let pi_sqrt: Vec<f64> = pi.iter().map(|p| (p + eps).sqrt()).collect();
    let pi_inv: Vec<f64> = pi_sqrt.iter().map(|p| 1.0 / p).collect();

    let omega: Vec<Vec<f64>> = (0..k).map(|_| (0..n).map(|_| rng.next_gaussian()).collect()).collect();

    let aw = |x: &[f64], y: &mut Vec<f64>, t: &mut Vec<f64>| {
        for i in 0..n { t[i] = pi_inv[i] * x[i]; }
        adj.spmv(t, y);
        for i in 0..n { y[i] *= pi_sqrt[i]; }
    };
    let awt = |x: &[f64], y: &mut Vec<f64>, t: &mut Vec<f64>| {
        for i in 0..n { t[i] = pi_sqrt[i] * x[i]; }
        adj.spmv_t(t, y);
        for i in 0..n { y[i] *= pi_inv[i]; }
    };

    let mut y: Vec<Vec<f64>> = vec![vec![0.0; n]; k];
    let mut tmp = vec![0.0; n];
    for col in 0..k { aw(&omega[col], &mut y[col], &mut tmp); }

    let mut z = vec![0.0; n];
    for _ in 0..n_power_iter {
        gram_schmidt(&mut y);
        for col in 0..k {
            awt(&y[col], &mut z, &mut tmp);
            aw(&z, &mut y[col], &mut tmp);
        }
    }
    gram_schmidt(&mut y);

    // Singular values: sigma[i] = ||A_w * y[i]||
    let mut sigmas = vec![0.0f64; k];
    let mut ay = vec![0.0; n];
    for col in 0..k {
        aw(&y[col], &mut ay, &mut tmp);
        sigmas[col] = ay.iter().map(|x| x * x).sum::<f64>().sqrt();
    }

    // Sort descending
    let mut ord: Vec<usize> = (0..k).collect();
    ord.sort_by(|&a, &b| sigmas[b].partial_cmp(&sigmas[a]).unwrap_or(std::cmp::Ordering::Equal));
    SvdResult {
        vectors: ord.iter().map(|&i| y[i].clone()).collect(),
        sigmas: ord.iter().map(|&i| sigmas[i]).collect(),
    }
}

// ── Binary output ────────────────────────────────────────────────────
// "CYBR" | version:u32 | n:u64 | k:u64 | vocab | pagerank | sigmas | vectors | spectral_gap | lambda2

fn save_embeddings(path: &Path, graph: &GraphData, pr: &PageRankResult, svd: &SvdResult) -> Result<()> {
    let mut f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(b"CYBR")?;
    f.write_all(&1u32.to_le_bytes())?;
    f.write_all(&(graph.n as u64).to_le_bytes())?;
    f.write_all(&(svd.sigmas.len() as u64).to_le_bytes())?;
    for name in &graph.idx_to_id {
        let b = name.as_bytes();
        f.write_all(&(b.len() as u32).to_le_bytes())?;
        f.write_all(b)?;
    }
    for &v in &pr.pi { f.write_all(&v.to_le_bytes())?; }
    for &s in &svd.sigmas { f.write_all(&s.to_le_bytes())?; }
    for vec in &svd.vectors { for &v in vec { f.write_all(&v.to_le_bytes())?; } }
    f.write_all(&pr.spectral_gap.to_le_bytes())?;
    f.write_all(&pr.lambda2.to_le_bytes())?;
    Ok(())
}

// ── Public entry point ───────────────────────────────────────────────

pub fn run_compile(input: &Path, _stakes: Option<&Path>, output: &Path, k: usize) -> Result<()> {
    use colored::Colorize;
    let start = std::time::Instant::now();

    println!("{} {}", "Loading".cyan().bold(), input.display());
    let graph = load_cyberlinks(input)?;
    println!("  {} {} particles, {} cyberlinks", "Graph".dimmed(), graph.n, graph.adj.nnz());

    println!("{}", "Computing PageRank...".cyan().bold());
    let pr = compute_pagerank(&graph.adj, &graph.out_degree, 0.85, 100);
    println!("  {} {} iters, spectral_gap={:.6}, lambda2={:.6}",
        "PageRank".dimmed(), pr.iterations, pr.spectral_gap, pr.lambda2);

    let mut ranked: Vec<(usize, f64)> = pr.pi.iter().enumerate().map(|(i, &v)| (i, v)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    println!("  {} Top-10 by PageRank:", "Info".dimmed());
    for &(idx, score) in ranked.iter().take(10) {
        println!("    {:.8}  {}", score, graph.idx_to_id[idx]);
    }

    let ek = k.min(graph.n);
    println!("{} k={}, power_iter=5", "Computing SVD...".cyan().bold(), ek);
    let svd = randomized_svd(&graph.adj, &pr.pi, ek, 5, 42);
    let show = ek.min(20);
    println!("  {} Top-{} sigmas: [{}]", "SVD".dimmed(), show,
        svd.sigmas.iter().take(show).map(|s| format!("{:.4}", s)).collect::<Vec<_>>().join(", "));

    println!("{} {}", "Saving".cyan().bold(), output.display());
    save_embeddings(output, &graph, &pr, &svd)?;
    let sz = std::fs::metadata(output)?.len();
    println!("{} {:.2}s, {:.2} MB  (n={}, k={}, gap={:.6}, l2={:.6})",
        "Done!".green().bold(), start.elapsed().as_secs_f64(),
        sz as f64 / (1024.0 * 1024.0), graph.n, ek, pr.spectral_gap, pr.lambda2);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_jsonl(links: &[(&str, &str)]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for (from, to) in links {
            writeln!(f, r#"{{"particle_from":"{}","particle_to":"{}"}}"#, from, to).unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_load_cyberlinks() {
        let f = make_test_jsonl(&[("a", "b"), ("b", "c"), ("c", "a"), ("a", "c")]);
        let g = load_cyberlinks(f.path()).unwrap();
        assert_eq!(g.n, 3);
        assert_eq!(g.adj.nnz(), 4);
        assert_eq!(g.out_degree[g.id_to_idx["a"]], 2.0);
        assert_eq!(g.out_degree[g.id_to_idx["b"]], 1.0);
    }

    #[test]
    fn test_pagerank_sums_to_one() {
        let f = make_test_jsonl(&[("a", "b"), ("b", "c"), ("c", "a")]);
        let g = load_cyberlinks(f.path()).unwrap();
        let pr = compute_pagerank(&g.adj, &g.out_degree, 0.85, 100);
        let total: f64 = pr.pi.iter().sum();
        assert!((total - 1.0).abs() < 0.01, "PageRank should sum to 1, got {}", total);
    }

    #[test]
    fn test_pagerank_hub_wins() {
        let f = make_test_jsonl(&[("a", "b"), ("b", "c"), ("c", "a"), ("d", "a")]);
        let g = load_cyberlinks(f.path()).unwrap();
        let pr = compute_pagerank(&g.adj, &g.out_degree, 0.85, 100);
        assert!(pr.pi[g.id_to_idx["a"]] > pr.pi[g.id_to_idx["d"]]);
    }

    #[test]
    fn test_gram_schmidt_orthonormal() {
        let mut vecs = vec![vec![1.0, 0.0, 0.0], vec![1.0, 1.0, 0.0], vec![1.0, 1.0, 1.0]];
        gram_schmidt(&mut vecs);
        for i in 0..3 {
            let norm: f64 = vecs[i].iter().map(|x| x * x).sum::<f64>().sqrt();
            assert!((norm - 1.0).abs() < 1e-10, "Vector {} not unit: {}", i, norm);
            for j in 0..i {
                let dot: f64 = vecs[i].iter().zip(vecs[j].iter()).map(|(a, b)| a * b).sum();
                assert!(dot.abs() < 1e-10, "Vectors {},{} not orthogonal: {}", i, j, dot);
            }
        }
    }

    #[test]
    fn test_svd_basic() {
        let f = make_test_jsonl(&[("a","b"),("b","c"),("c","a"),("d","a"),("d","b")]);
        let g = load_cyberlinks(f.path()).unwrap();
        let pr = compute_pagerank(&g.adj, &g.out_degree, 0.85, 100);
        let svd = randomized_svd(&g.adj, &pr.pi, 2, 3, 42);
        assert_eq!(svd.sigmas.len(), 2);
        assert!(svd.sigmas[0] >= svd.sigmas[1]);
        for i in 0..svd.vectors.len() {
            let norm: f64 = svd.vectors[i].iter().map(|x| x * x).sum::<f64>().sqrt();
            assert!((norm - 1.0).abs() < 0.01, "SVD vector {} not unit: {}", i, norm);
        }
    }

    #[test]
    fn test_roundtrip_save_load() {
        let f = make_test_jsonl(&[("a", "b"), ("b", "c"), ("c", "a")]);
        let g = load_cyberlinks(f.path()).unwrap();
        let pr = compute_pagerank(&g.adj, &g.out_degree, 0.85, 100);
        let svd = randomized_svd(&g.adj, &pr.pi, 2, 2, 42);
        let out = tempfile::NamedTempFile::new().unwrap();
        save_embeddings(out.path(), &g, &pr, &svd).unwrap();
        let data = std::fs::read(out.path()).unwrap();
        assert_eq!(&data[0..4], b"CYBR");
        assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 3);
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 2);
    }
}
