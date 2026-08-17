// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use crate::scanner::DiscoveredFiles;
use anyhow::Result;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

/// Media extensions that must be served as raw files (not HTML graph pages).
pub fn is_media_extension(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "avif"
            | "mp4" | "webm" | "mp3" | "wav" | "ogg" | "pdf"
    )
}

/// True when a pretty-URL page id is a media asset under `media/…`.
/// Those collide with `copy_media` output (`media/foo.svg` file vs
/// `media/foo.svg/index.html` directory) and must not be written as HTML.
pub fn is_media_asset_page_id(page_id: &str) -> bool {
    let id = page_id.trim_start_matches('/');
    let Some(rest) = id.strip_prefix("media/") else {
        return false;
    };
    if rest.is_empty() || rest.contains("..") {
        return false;
    }
    Path::new(rest)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(is_media_extension)
}

/// Copy discovered media to output directory.
///
/// Runs **after** page HTML writes. Media files are also registered as graph
/// File nodes, so a pretty-URL build would create `media/foo.svg/index.html`
/// directories that shadow the raw asset. We always force the raw file to win.
pub fn copy_media(discovered: &DiscoveredFiles, output_dir: &Path) -> Result<()> {
    if discovered.media.is_empty() {
        return Ok(());
    }

    let media_output = output_dir.join("media");
    fs::create_dir_all(&media_output)?;

    for file in &discovered.media {
        let dest = media_output.join(&file.name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        // Pretty-URL page write may have left a directory here — remove it
        // so the raw asset can occupy the URL path browsers request.
        if dest.is_dir() {
            fs::remove_dir_all(&dest)?;
        } else if dest.exists() {
            fs::remove_file(&dest)?;
        }
        fs::copy(&file.path, &dest).map_err(|e| {
            anyhow::anyhow!(
                "copy media {} → {}: {}",
                file.path.display(),
                dest.display(),
                e
            )
        })?;
    }

    Ok(())
}

/// Recursively copy a directory.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        let relative = entry.path().strip_prefix(src)?;
        let target = dst.join(relative);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
