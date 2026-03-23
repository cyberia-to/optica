// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
//! Query the compiled model — pure graph intelligence, no LLM.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

pub struct Model {
    pub n: usize, pub k: usize,
    pub vocab: Vec<String>, pub pagerank: Vec<f64>,
    pub sigmas: Vec<f64>, pub vectors: Vec<Vec<f64>>,
    pub spectral_gap: f64, pub lambda2: f64,
    pub embeddings: Vec<Vec<f64>>, // n×k, L2-normalized
}

fn ru32(b: &[u8], p: &mut usize) -> u32 { let v = u32::from_le_bytes(b[*p..*p+4].try_into().unwrap()); *p += 4; v }
fn ru64(b: &[u8], p: &mut usize) -> u64 { let v = u64::from_le_bytes(b[*p..*p+8].try_into().unwrap()); *p += 8; v }
fn rf64(b: &[u8], p: &mut usize) -> f64 { let v = f64::from_le_bytes(b[*p..*p+8].try_into().unwrap()); *p += 8; v }

pub fn load_model(path: &Path) -> Result<Model> {
    let mut buf = Vec::new();
    std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?.read_to_end(&mut buf)?;
    let mut p = 0usize;
    if &buf[p..p+4] != b"CYBR" { bail!("bad magic: expected CYBR"); }
    p += 4;
    let ver = ru32(&buf, &mut p);
    if ver != 1 { bail!("unsupported model version {}", ver); }
    let n = ru64(&buf, &mut p) as usize;
    let k = ru64(&buf, &mut p) as usize;
    let mut vocab = Vec::with_capacity(n);
    for _ in 0..n {
        let len = ru32(&buf, &mut p) as usize;
        vocab.push(std::str::from_utf8(&buf[p..p+len]).context("bad UTF-8")?.to_string());
        p += len;
    }
    let pagerank: Vec<f64> = (0..n).map(|_| rf64(&buf, &mut p)).collect();
    let sigmas: Vec<f64> = (0..k).map(|_| rf64(&buf, &mut p)).collect();
    let vectors: Vec<Vec<f64>> = (0..k).map(|_| (0..n).map(|_| rf64(&buf, &mut p)).collect()).collect();
    let spectral_gap = rf64(&buf, &mut p);
    let lambda2 = rf64(&buf, &mut p);
    let embeddings = (0..n).map(|i| {
        let mut row: Vec<f64> = (0..k).map(|j| vectors[j][i]).collect();
        let norm = row.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm > 1e-14 { row.iter_mut().for_each(|x| *x /= norm); }
        row
    }).collect();
    Ok(Model { n, k, vocab, pagerank, sigmas, vectors, spectral_gap, lambda2, embeddings })
}

pub fn load_cid_index(path: &Path) -> Result<HashMap<String, String>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?)?)
}

// ── Text search (exact → substring → word, skip stopwords) ─────────

const STOP: &[&str] = &[
    "the","a","an","is","are","was","were","be","been","being","in","on","at",
    "to","for","of","with","by","from","and","or","not","it","this","that",
];

fn find_vertex(m: &Model, query: &str, ci: &Option<HashMap<String, String>>) -> Option<usize> {
    let q = query.to_lowercase();
    // Direct vocab match
    if let Some(i) = m.vocab.iter().position(|v| v.to_lowercase() == q) { return Some(i); }
    if let Some(idx) = ci {
        // Exact text
        for (cid, text) in idx {
            if text.to_lowercase() == q { if let Some(i) = m.vocab.iter().position(|v| v == cid) { return Some(i); } }
        }
        // Substring text
        for (cid, text) in idx {
            if text.to_lowercase().contains(&q) { if let Some(i) = m.vocab.iter().position(|v| v == cid) { return Some(i); } }
        }
        // Word match (skip stopwords)
        let words: Vec<&str> = q.split_whitespace().filter(|w| !STOP.contains(w)).collect();
        if !words.is_empty() {
            for (cid, text) in idx {
                let t = text.to_lowercase();
                if words.iter().any(|w| t.contains(w)) {
                    if let Some(i) = m.vocab.iter().position(|v| v == cid) { return Some(i); }
                }
            }
        }
    }
    // Substring vocab match
    m.vocab.iter().position(|v| v.to_lowercase().contains(&q))
}

// ── Cosine neighbors ────────────────────────────────────────────────

fn cosine_neighbors(m: &Model, idx: usize, k: usize) -> Vec<(usize, f64)> {
    let e = &m.embeddings[idx];
    let mut s: Vec<(usize, f64)> = m.embeddings.iter().enumerate()
        .filter(|(i, _)| *i != idx)
        .map(|(i, o)| (i, e.iter().zip(o).map(|(a, b)| a * b).sum()))
        .collect();
    s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    s.truncate(k); s
}

// ── Spectral role ───────────────────────────────────────────────────

#[derive(Debug)]
pub enum Role { Hub, Authority, Specialist, Bridge, Member }
impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", match self { Role::Hub=>"HUB", Role::Authority=>"AUTHORITY",
            Role::Specialist=>"SPECIALIST", Role::Bridge=>"BRIDGE", Role::Member=>"MEMBER" })
    }
}

fn classify_role(m: &Model, idx: usize) -> Role {
    let pr = m.pagerank[idx];
    let mut spr = m.pagerank.clone();
    spr.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let high_pr = pr > spr[spr.len() / 2] * 10.0;
    let emb = &m.embeddings[idx];
    let mut av: Vec<f64> = emb.iter().map(|x| x.abs()).collect();
    av.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let te: f64 = av.iter().map(|x| x * x).sum();
    let t2: f64 = av.iter().take(2).map(|x| x * x).sum();
    let conc = if te > 1e-14 { t2 / te } else { 0.0 };
    let sa: f64 = av.iter().sum();
    let ent = if sa > 1e-14 {
        -av.iter().map(|x| x / sa).filter(|p| *p > 1e-14).map(|p| p * p.ln()).sum::<f64>()
    } else { 0.0 };
    let ne = if m.k > 1 { ent / (m.k as f64).ln() } else { 0.0 };
    if high_pr && ne > 0.6 { Role::Hub }
    else if high_pr && conc > 0.5 { Role::Authority }
    else if conc > 0.7 { Role::Specialist }
    else if ne > 0.7 && !high_pr { Role::Bridge }
    else { Role::Member }
}

// ── Display ─────────────────────────────────────────────────────────

fn label(m: &Model, i: usize, ci: &Option<HashMap<String, String>>) -> String {
    let cid = &m.vocab[i];
    if let Some(ref ix) = ci {
        if let Some(t) = ix.get(cid) { return format!("{} ({})", t, &cid[..cid.len().min(12)]); }
    }
    cid.clone()
}

// ── Public entry ────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub enum QueryMode { Neighbors, Role, Full }

pub fn run_query(model_path: &Path, query: &str, index_path: Option<&Path>, k: usize, mode: QueryMode) -> Result<()> {
    use colored::Colorize;
    let m = load_model(model_path)?;
    eprintln!("{} n={}, k={}, gap={:.6}, l2={:.6}", "Model".cyan().bold(), m.n, m.k, m.spectral_gap, m.lambda2);
    let ci = index_path.map(load_cid_index).transpose()?;
    let idx = find_vertex(&m, query, &ci).with_context(|| format!("vertex not found: '{}'", query))?;
    println!("{} {} [pr={:.8}]", "Vertex".green().bold(), label(&m, idx, &ci), m.pagerank[idx]);
    match mode {
        QueryMode::Neighbors => print_neighbors(&m, idx, k, &ci),
        QueryMode::Role => print_role(&m, idx),
        QueryMode::Full => { print_role(&m, idx); println!(); print_neighbors(&m, idx, k, &ci); }
    }
    Ok(())
}

fn print_neighbors(m: &Model, idx: usize, k: usize, ci: &Option<HashMap<String, String>>) {
    use colored::Colorize;
    println!("{} (cosine, k={})", "Neighbors".cyan().bold(), k);
    for (r, (ni, sim)) in cosine_neighbors(m, idx, k).iter().enumerate() {
        println!("  {:>3}. {:.6}  {} [pr={:.8}]", r + 1, sim, label(m, *ni, ci), m.pagerank[*ni]);
    }
}

fn print_role(m: &Model, idx: usize) {
    use colored::Colorize;
    let role = classify_role(m, idx);
    let emb = &m.embeddings[idx];
    let mut dims: Vec<(usize, f64)> = emb.iter().enumerate().map(|(i, &v)| (i, v)).collect();
    dims.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap_or(std::cmp::Ordering::Equal));
    println!("{} {}", "Role".cyan().bold(), role);
    println!("  top dims: {}", dims.iter().take(3).map(|(d, v)| format!("s{}={:.4}", d, v)).collect::<Vec<_>>().join("  "));
}
