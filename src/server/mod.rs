// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
mod reload;

use crate::config::SiteConfig;
use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn serve(
    config: &SiteConfig,
    bind: &str,
    port: u16,
    live_reload: bool,
    open_browser: bool,
    subgraphs: Option<&Path>,
) -> Result<()> {
    let output_dir = config.build.output_dir.clone();
    let addr = format!("{}:{}", bind, port);
    let url = format!("http://{}", addr);

    println!(
        "{} {} → {}",
        "Serving".green().bold(),
        output_dir.display(),
        url
    );

    if live_reload {
        println!("  {} Live reload enabled", "Watch".dimmed());
    }

    let server = Arc::new(
        tiny_http::Server::http(&addr)
            .map_err(|e| anyhow::anyhow!("Failed to start server: {}", e))?,
    );

    if open_browser {
        open_url(&url);
    }

    // Build version counter — incremented after each rebuild
    let build_version = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));

    // Start file watcher + rebuild thread
    if live_reload {
        reload::start_watch_rebuild(
            config.clone(),
            build_version.clone(),
            subgraphs.map(|p| p.to_path_buf()),
        );
    }

    println!("  Press Ctrl+C to stop\n");

    // Ctrl+C handler
    {
        let r = running.clone();
        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        })
        .expect("Failed to set Ctrl+C handler");
    }

    while running.load(Ordering::SeqCst) {
        match server.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(request)) => {
                let url_path = request.url().to_string();
                let url_path_clean = url_path.split('?').next().unwrap_or(&url_path);

                if url_path_clean == "/__reload" {
                    // Parse client's last-known version from query string
                    let client_version: Option<u64> = url_path
                        .split('?')
                        .nth(1)
                        .and_then(|q| q.strip_prefix("v="))
                        .and_then(|v| v.parse().ok());
                    // Handle SSE in a separate thread so the server keeps serving
                    let version = build_version.clone();
                    let is_running = running.clone();
                    std::thread::spawn(move || {
                        handle_sse_reload(request, &version, &is_running, client_version);
                    });
                } else {
                    // Handle regular requests in a thread to keep the main loop responsive.
                    // This prevents serialized request handling from blocking concurrent loads.
                    let dir = output_dir.clone();
                    std::thread::spawn(move || {
                        handle_request(request, &dir, live_reload);
                    });
                }
            }
            Ok(None) => {
                // Timeout — loop continues, checks running flag
            }
            Err(_) => break,
        }
    }

    println!("\n{} Server stopped.", "Bye!".green().bold());
    Ok(())
}

/// SSE long-poll: wait for build_version to change, then send "reload" event.
/// EventSource on the client will automatically reconnect after receiving.
///
/// Root cause of prior instability: SSE timeout (30s) ≈ rebuild time (30-60s).
/// The version would increment DURING the reconnection window between the old
/// SSE closing and the new SSE opening — the event was lost. The new SSE saw
/// the already-incremented version as "current" and waited for the NEXT change.
///
/// Fix 1: client sends its last-known version as ?v=N. If the server's version
/// is already ahead, send reload immediately (catches the missed event).
/// Fix 2: timeout increased to 300s (5 min) to outlast any rebuild.
fn handle_sse_reload(
    request: tiny_http::Request,
    version: &AtomicU64,
    running: &AtomicBool,
    client_version: Option<u64>,
) {
    let server_version = version.load(Ordering::SeqCst);

    // If client sent a version and it's behind the server, reload immediately.
    // This catches the race where the version incremented during reconnection.
    if let Some(cv) = client_version {
        if cv < server_version {
            let body = format!("data: reload\n\n");
            let response = tiny_http::Response::from_string(body)
                .with_header(
                    tiny_http::Header::from_bytes(b"Content-Type", b"text/event-stream").unwrap(),
                )
                .with_header(tiny_http::Header::from_bytes(b"Cache-Control", b"no-cache").unwrap())
                .with_header(tiny_http::Header::from_bytes(b"Connection", b"close").unwrap());
            let _ = request.respond(response);
            return;
        }
    }

    // Wait up to 300s for a rebuild (poll every 200ms).
    // 300s is 5× longer than the slowest observed rebuild (~60s).
    let start = Instant::now();
    while running.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(300) {
        if version.load(Ordering::SeqCst) != server_version {
            let new_version = version.load(Ordering::SeqCst);
            let body = format!("data: reload:{}\n\n", new_version);
            let response = tiny_http::Response::from_string(body)
                .with_header(
                    tiny_http::Header::from_bytes(b"Content-Type", b"text/event-stream").unwrap(),
                )
                .with_header(tiny_http::Header::from_bytes(b"Cache-Control", b"no-cache").unwrap())
                .with_header(tiny_http::Header::from_bytes(b"Connection", b"close").unwrap());
            let _ = request.respond(response);
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Timeout keep-alive
    let body = format!("data: ping:{}\n\n", server_version);
    let response = tiny_http::Response::from_string(body)
        .with_header(tiny_http::Header::from_bytes(b"Content-Type", b"text/event-stream").unwrap())
        .with_header(tiny_http::Header::from_bytes(b"Cache-Control", b"no-cache").unwrap())
        .with_header(tiny_http::Header::from_bytes(b"Connection", b"close").unwrap());
    let _ = request.respond(response);
}

fn handle_request(request: tiny_http::Request, output_dir: &Path, inject_reload: bool) {
    let url_path = request.url().to_string();
    let url_path = url_path.split('?').next().unwrap_or(&url_path);

    // Determine file path
    let file_path = resolve_file_path(url_path, output_dir);

    if file_path.exists() {
        let content_type = guess_content_type(&file_path);
        let mut content = std::fs::read(&file_path).unwrap_or_default();

        // Inject live reload script into HTML
        if inject_reload && content_type.starts_with("text/html") {
            if let Ok(html) = String::from_utf8(content.clone()) {
                let injected =
                    html.replace("</body>", &format!("{}\n</body>", reload::RELOAD_SCRIPT));
                content = injected.into_bytes();
            }
        }

        let response = tiny_http::Response::from_data(content)
            .with_header(
                tiny_http::Header::from_bytes(b"Content-Type", content_type.as_bytes()).unwrap(),
            )
            .with_header(
                tiny_http::Header::from_bytes(
                    b"Cache-Control",
                    b"no-cache, no-store, must-revalidate",
                )
                .unwrap(),
            )
            .with_header(tiny_http::Header::from_bytes(b"Connection", b"close").unwrap());
        let _ = request.respond(response);
    } else {
        let response = tiny_http::Response::from_string("404 Not Found")
            .with_status_code(404)
            .with_header(tiny_http::Header::from_bytes(b"Content-Type", b"text/html").unwrap())
            .with_header(tiny_http::Header::from_bytes(b"Connection", b"close").unwrap());
        let _ = request.respond(response);
    }
}

fn resolve_file_path(url_path: &str, output_dir: &Path) -> PathBuf {
    if url_path == "/" || url_path.is_empty() {
        return output_dir.join("index.html");
    }

    let clean = url_path.trim_start_matches('/');
    let path = output_dir.join(clean);

    if path.is_dir() {
        path.join("index.html")
    } else if path.exists() {
        path
    } else {
        // Try adding .html
        let with_html = output_dir.join(format!("{}.html", clean));
        if with_html.exists() {
            with_html
        } else {
            // Try as directory with index.html
            let as_dir = output_dir.join(clean).join("index.html");
            if as_dir.exists() {
                as_dir
            } else {
                path
            }
        }
    }
}

fn guess_content_type(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8".to_string(),
        Some("css") => "text/css; charset=utf-8".to_string(),
        Some("js") => "application/javascript; charset=utf-8".to_string(),
        Some("json") => "application/json".to_string(),
        Some("xml") => "application/xml".to_string(),
        Some("png") => "image/png".to_string(),
        Some("jpg") | Some("jpeg") => "image/jpeg".to_string(),
        Some("gif") => "image/gif".to_string(),
        Some("svg") => "image/svg+xml".to_string(),
        Some("webp") => "image/webp".to_string(),
        Some("woff2") => "font/woff2".to_string(),
        Some("woff") => "font/woff".to_string(),
        Some("ico") => "image/x-icon".to_string(),
        Some("pdf") => "application/pdf".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .spawn();
    }
}
