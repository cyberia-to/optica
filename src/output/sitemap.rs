// ---
// tags: optica, rust
// crystal-type: source
// crystal-domain: comp
// ---
use crate::config::SiteConfig;
use crate::render::RenderedPage;
use anyhow::Result;
use std::path::Path;

pub fn generate_sitemap(
    rendered: &[RenderedPage],
    config: &SiteConfig,
    output_dir: &Path,
) -> Result<()> {
    let url_paths: Vec<String> = rendered.iter().map(|p| p.url_path.clone()).collect();
    generate_sitemap_paths(&url_paths, config, output_dir)
}

/// The sitemap from url paths alone — what the live-reload path has: it
/// renders only the dirty pages, and the whole site's paths come from its
/// render cache.
pub fn generate_sitemap_paths(
    url_paths: &[String],
    config: &SiteConfig,
    output_dir: &Path,
) -> Result<()> {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
"#,
    );

    for page_url in url_paths {
        let url_path = if page_url.ends_with("/index.html") {
            page_url.trim_end_matches("index.html").to_string()
        } else {
            page_url.clone()
        };

        xml.push_str(&format!(
            "  <url>\n    <loc>{}{}</loc>\n  </url>\n",
            config.site.base_url.trim_end_matches('/'),
            url_path
        ));
    }

    xml.push_str("</urlset>\n");

    std::fs::write(output_dir.join("sitemap.xml"), xml)?;

    Ok(())
}
