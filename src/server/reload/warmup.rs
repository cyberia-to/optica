// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
//! Warm-up of the live-reload watcher: waiting out the initial full build, and
//! forgetting what the cache learned about files that were edited while it ran.

use super::BuildCache;
use crate::parser::PageId;
use colored::Colorize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Block until the thread running the initial full build sets the flag.
///
/// No timeout on purpose: a full build of a large graph takes minutes, and a
/// wait that gives up early would warm up mid-build — exactly the race this
/// fix removes. The flag is instead guaranteed to be set: `main` stores it
/// after a `catch_unwind`, so a panic inside the build sets it too and this
/// loop cannot hang on one.
pub(super) fn wait_for_initial_build(build_done: Option<&Arc<AtomicBool>>) {
    if let Some(done) = build_done {
        while !done.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Take every event the channel has buffered so far, without blocking.
pub(super) fn drain_buffered(rx: &mpsc::Receiver<Vec<PathBuf>>) -> Vec<PathBuf> {
    let mut buffered: Vec<PathBuf> = Vec::new();
    while let Ok(more) = rx.try_recv() {
        buffered.extend(more);
    }
    buffered
}

/// Drop every cache entry belonging to a file named by `late_paths` — the
/// events that arrived while the initial build and the warm-up were running.
///
/// Those pages were written by the build from older bytes, while the warm-up
/// has already parsed and hashed the current ones; without this the first
/// incremental pass finds no change and the served page stays stale. Returns
/// how many pages were forgotten.
pub(super) fn forget_late_events(cache: &mut BuildCache, late_paths: &[PathBuf]) -> usize {
    if late_paths.is_empty() {
        return 0;
    }
    let late: HashSet<PathBuf> = late_paths.iter().cloned().collect();
    let mut forgotten = 0usize;
    let root_hits: Vec<(PathBuf, PageId)> = cache.parse_cache.iter()
        .filter(|(path, _)| late.contains(*path))
        .map(|(path, (_, page))| (path.clone(), page.id.clone()))
        .collect();
    for (path, id) in root_hits {
        cache.parse_cache.remove(&path);
        forget_hashes(cache, &id);
        forgotten += 1;
    }
    let sg_names: Vec<String> = cache.subgraph_parse_caches.keys().cloned().collect();
    for name in sg_names {
        let hits: Vec<(PathBuf, PageId)> = match cache.subgraph_parse_caches.get(&name) {
            Some(sg_cache) => sg_cache.iter()
                .filter(|(path, _)| late.contains(*path))
                .map(|(path, (_, page))| (path.clone(), page.id.clone()))
                .collect(),
            None => continue,
        };
        for (path, id) in hits {
            if let Some(sg_cache) = cache.subgraph_parse_caches.get_mut(&name) {
                sg_cache.remove(&path);
            }
            forget_hashes(cache, &id);
            forgotten += 1;
        }
    }
    eprintln!(
        "  {} {} file{} changed during the build; {} page{} will be re-rendered from current bytes",
        "Watch".dimmed(), late_paths.len(), if late_paths.len() == 1 { "" } else { "s" },
        forgotten, if forgotten == 1 { "" } else { "s" }
    );
    forgotten
}

/// Forget every per-page hash the incremental pass compares against, so the
/// page counts as changed whatever its current bytes hash to.
fn forget_hashes(cache: &mut BuildCache, id: &PageId) {
    cache.content_hashes.remove(id);
    cache.meta_hashes.remove(id);
    cache.tag_hashes.remove(id);
    cache.link_hashes.remove(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{PageKind, PageMeta, ParsedPage};
    use std::collections::HashMap;
    use std::time::SystemTime;

    fn make_page(id: &str, path: &str) -> ParsedPage {
        ParsedPage {
            id: id.to_string(),
            meta: PageMeta {
                title: id.to_string(),
                properties: HashMap::new(),
                tags: vec![],
                public: Some(true),
                aliases: vec![],
                date: None,
                icon: None,
                menu_order: None,
                stake: None,
            },
            kind: PageKind::Page,
            source_path: PathBuf::from(path),
            namespace: None,
            subgraph: None,
            content_md: "old bytes".to_string(),
            outgoing_links: vec![],
        }
    }

    /// Put a page in the root parse cache and give it the four hashes the
    /// warm-up snapshots.
    fn seed_root(cache: &mut BuildCache, id: &str, path: &str) {
        let page = make_page(id, path);
        cache.parse_cache
            .insert(PathBuf::from(path), (SystemTime::UNIX_EPOCH, page));
        cache.content_hashes.insert(id.to_string(), 1);
        cache.meta_hashes.insert(id.to_string(), 2);
        cache.tag_hashes.insert(id.to_string(), 3);
        cache.link_hashes.insert(id.to_string(), 4);
    }

    #[test]
    fn forgets_the_late_page_and_keeps_the_others() {
        let mut cache = BuildCache::new();
        seed_root(&mut cache, "late", "/graph/late.md");
        seed_root(&mut cache, "quiet", "/graph/quiet.md");

        let forgotten = forget_late_events(&mut cache, &[PathBuf::from("/graph/late.md")]);

        assert_eq!(forgotten, 1, "exactly the one late page is forgotten");
        assert!(!cache.parse_cache.contains_key(&PathBuf::from("/graph/late.md")));
        assert!(!cache.content_hashes.contains_key("late"));
        assert!(!cache.meta_hashes.contains_key("late"));
        assert!(!cache.tag_hashes.contains_key("late"));
        assert!(!cache.link_hashes.contains_key("late"));
        // The untouched page keeps everything, or the first pass re-renders the world.
        assert!(cache.parse_cache.contains_key(&PathBuf::from("/graph/quiet.md")));
        assert!(cache.content_hashes.contains_key("quiet"));
        assert!(cache.meta_hashes.contains_key("quiet"));
        assert!(cache.tag_hashes.contains_key("quiet"));
        assert!(cache.link_hashes.contains_key("quiet"));
    }

    #[test]
    fn forgets_a_late_page_of_a_subgraph() {
        let mut cache = BuildCache::new();
        let path = PathBuf::from("/repos/sub/page.md");
        let sg_cache = cache.subgraph_parse_caches.entry("sub".to_string()).or_default();
        sg_cache.insert(
            path.clone(),
            (SystemTime::UNIX_EPOCH, make_page("sub/page", "/repos/sub/page.md")),
        );
        cache.content_hashes.insert("sub/page".to_string(), 1);
        cache.meta_hashes.insert("sub/page".to_string(), 2);
        cache.tag_hashes.insert("sub/page".to_string(), 3);
        cache.link_hashes.insert("sub/page".to_string(), 4);

        let forgotten = forget_late_events(&mut cache, &[path.clone()]);

        assert_eq!(forgotten, 1);
        assert!(!cache.subgraph_parse_caches["sub"].contains_key(&path));
        assert!(!cache.content_hashes.contains_key("sub/page"));
        assert!(!cache.link_hashes.contains_key("sub/page"));
    }

    #[test]
    fn drain_takes_every_buffered_event_without_blocking() {
        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        tx.send(vec![PathBuf::from("/graph/a.md")]).unwrap();
        tx.send(vec![PathBuf::from("/graph/b.md"), PathBuf::from("/graph/c.md")]).unwrap();

        let drained = drain_buffered(&rx);

        assert_eq!(drained.len(), 3, "every buffered batch is flattened into one list");
        assert!(drain_buffered(&rx).is_empty(), "a second drain finds nothing left");
    }

    #[test]
    fn a_path_no_cache_knows_forgets_nothing() {
        let mut cache = BuildCache::new();
        seed_root(&mut cache, "quiet", "/graph/quiet.md");

        let forgotten = forget_late_events(&mut cache, &[PathBuf::from("/graph/never-seen.md")]);

        assert_eq!(forgotten, 0);
        assert!(cache.content_hashes.contains_key("quiet"));
    }

    #[test]
    fn the_wait_returns_when_the_build_sets_the_flag() {
        let done = Arc::new(AtomicBool::new(false));
        let setter = done.clone();
        let builder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            setter.store(true, Ordering::SeqCst);
        });

        wait_for_initial_build(Some(&done));

        assert!(done.load(Ordering::SeqCst), "the wait only returns once the flag is set");
        builder.join().unwrap();
    }

    #[test]
    fn the_wait_returns_at_once_without_a_flag() {
        wait_for_initial_build(None);
    }
}
