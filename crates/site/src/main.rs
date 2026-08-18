mod components;
mod data;
mod feeds;
mod html;
mod layout;
mod markdown;
mod og;
mod pages;
mod procedures;

use anyhow::{Context, Result};
use data::SiteData;
use include_dir::{include_dir, Dir};
use layout::Page;
use std::path::{Path, PathBuf};

pub(crate) static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");

const USAGE: &str = "usage: pollywiki-site [--out <dir>] [--serve [port]]
  BUNDLES_DIR   bundles directory (default data/sample/bundles)
  SITE_URL      canonical origin (default https://pollywiki.au)
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return;
    }
    let out_dir = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("dist"));
    let serve_port = args.iter().position(|a| a == "--serve").map(|i| {
        args.get(i + 1)
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(4321)
    });

    if let Err(err) = build(&out_dir) {
        eprintln!("build failed: {err:#}");
        std::process::exit(1);
    }
    if let Some(port) = serve_port {
        if let Err(err) = serve(&out_dir, port) {
            eprintln!("serve failed: {err:#}");
            std::process::exit(1);
        }
    }
}

fn bundles_dir() -> PathBuf {
    std::env::var("BUNDLES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/sample/bundles"))
}

fn build(out_dir: &Path) -> Result<()> {
    let bundles = bundles_dir();
    let site_url = std::env::var("SITE_URL").unwrap_or_else(|_| "https://pollywiki.au".to_string());
    let data = SiteData::load(&bundles, &site_url)
        .with_context(|| format!("loading bundles from {}", bundles.display()))?;

    if out_dir.exists() {
        std::fs::remove_dir_all(out_dir)?;
    }
    std::fs::create_dir_all(out_dir)?;

    let css_href = write_assets(out_dir)?;
    copy_public(&data, out_dir)?;

    let mut page_list: Vec<Page> = vec![
        pages::home(&data),
        pages::people_index(&data),
        pages::divisions_index(&data),
        pages::bills_index(&data),
        pages::electorates_index(&data),
        pages::parties_index(&data),
        pages::search_page(),
        pages::about_index(),
        pages::data_sources(&data),
        pages::methodology(),
        pages::corrections(),
    ];
    for person in &data.people {
        page_list.push(pages::person_page(&data, person));
    }
    // Per-division share cards. Best-effort: a division with no card falls
    // back to the site-wide default image.
    let cards = match og::Cards::load() {
        Some(cards) => cards.write_all(out_dir, &data)?,
        None => std::collections::HashMap::new(),
    };
    if !cards.is_empty() {
        println!("og: {} division cards", cards.len());
    }
    for division in &data.divisions {
        let mut page = pages::division_page(&data, division);
        page.og_image = cards.get(&division.id).cloned();
        page_list.push(page);
    }
    for bill in &data.bills {
        page_list.push(pages::bill_page(&data, bill));
    }
    for electorate in &data.electorates {
        page_list.push(pages::electorate_page(&data, electorate));
    }
    for party in &data.parties {
        page_list.push(pages::party_page(&data, party));
    }

    let mut sitemap_urls: Vec<(String, Option<String>)> = Vec::new();
    for page in &page_list {
        let html = layout::render(&data, &site_url, &css_href, page);
        let rel = page.path.trim_start_matches('/');
        let file = out_dir.join(rel).join("index.html");
        std::fs::create_dir_all(file.parent().unwrap())?;
        std::fs::write(&file, html)?;
        sitemap_urls.push((page.path.clone(), page.lastmod.clone()));
    }

    // The not-found page lives at /404.html, outside the sitemap.
    let not_found = pages::not_found();
    std::fs::write(
        out_dir.join("404.html"),
        layout::render(&data, &site_url, &css_href, &not_found),
    )?;

    write_sitemap(out_dir, &site_url, &sitemap_urls)?;
    feeds::write_feeds(out_dir, &site_url, &data)?;

    println!(
        "site: {} pages from {}",
        page_list.len() + 1,
        bundles.display()
    );

    run_pagefind(out_dir)?;
    Ok(())
}

fn write_assets(out_dir: &Path) -> Result<String> {
    let fonts_css = ASSETS
        .get_file("fonts.css")
        .context("fonts.css missing from embedded assets")?
        .contents_utf8()
        .context("fonts.css not utf8")?;
    let global_css = ASSETS
        .get_file("global.css")
        .context("global.css missing from embedded assets")?
        .contents_utf8()
        .context("global.css not utf8")?;
    let css = format!("{fonts_css}\n{global_css}");
    let css_name = format!("site.{:08x}.css", fnv1a(css.as_bytes()) & 0xffff_ffff);

    let assets_dir = out_dir.join("_assets");
    std::fs::create_dir_all(assets_dir.join("fonts"))?;
    std::fs::write(assets_dir.join(&css_name), &css)?;
    if let Some(fonts) = ASSETS.get_dir("fonts") {
        for file in fonts.files() {
            let name = file.path().file_name().unwrap();
            std::fs::write(assets_dir.join("fonts").join(name), file.contents())?;
        }
    }
    Ok(format!("/_assets/{css_name}"))
}

fn copy_public(data: &SiteData, out_dir: &Path) -> Result<()> {
    if let Some(public) = ASSETS.get_dir("public") {
        for file in public.files() {
            let name = file.path().file_name().unwrap();
            std::fs::write(out_dir.join(name), file.contents())?;
        }
    }
    // Build-time data artefacts that must also be served as static files.
    let quick_search = data.bundles_dir.join("quick-search.json");
    if quick_search.exists() {
        std::fs::copy(&quick_search, out_dir.join("quick-search.json"))?;
    } else {
        eprintln!(
            "prepare-public: {} missing, skipped",
            quick_search.display()
        );
    }
    Ok(())
}

fn write_sitemap(
    out_dir: &Path,
    site_url: &str,
    entries: &[(String, Option<String>)],
) -> Result<()> {
    let origin = site_url.trim_end_matches('/');
    let mut urls: Vec<(String, Option<&str>)> = entries
        .iter()
        .map(|(path, lastmod)| (format!("{origin}{path}"), lastmod.as_deref()))
        .collect();
    // Natural sort (digit runs compare numerically), matching the reference
    // sitemap: /bills/s996/ sorts before /bills/s1138/.
    urls.sort_by_key(|(url, _)| natural_key(url));
    let mut sitemap = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" xmlns:news=\"http://www.google.com/schemas/sitemap-news/0.9\" xmlns:xhtml=\"http://www.w3.org/1999/xhtml\" xmlns:image=\"http://www.google.com/schemas/sitemap-image/1.1\" xmlns:video=\"http://www.google.com/schemas/sitemap-video/1.1\">",
    );
    for (url, lastmod) in &urls {
        match lastmod {
            Some(date) => sitemap.push_str(&format!(
                "<url><loc>{url}</loc><lastmod>{date}</lastmod></url>"
            )),
            None => sitemap.push_str(&format!("<url><loc>{url}</loc></url>")),
        }
    }
    sitemap.push_str("</urlset>");
    std::fs::write(out_dir.join("sitemap-0.xml"), sitemap)?;
    std::fs::write(
        out_dir.join("sitemap-index.xml"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\"><sitemap><loc>{origin}/sitemap-0.xml</loc></sitemap></sitemapindex>"
        ),
    )?;
    Ok(())
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum NaturalToken {
    Number(u64),
    Text(String),
}

fn natural_key(input: &str) -> Vec<NaturalToken> {
    let mut tokens = Vec::new();
    let mut buffer = String::new();
    let mut digits = false;
    for c in input.chars() {
        if c.is_ascii_digit() == digits {
            buffer.push(c);
        } else {
            if !buffer.is_empty() {
                tokens.push(natural_token(&buffer, digits));
            }
            buffer = c.to_string();
            digits = c.is_ascii_digit();
        }
    }
    if !buffer.is_empty() {
        tokens.push(natural_token(&buffer, digits));
    }
    tokens
}

fn natural_token(buffer: &str, digits: bool) -> NaturalToken {
    if digits {
        match buffer.parse() {
            Ok(n) => NaturalToken::Number(n),
            Err(_) => NaturalToken::Text(buffer.to_string()),
        }
    } else {
        NaturalToken::Text(buffer.to_string())
    }
}

fn run_pagefind(out_dir: &Path) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let mut index = pagefind::api::PagefindIndex::new(None)
            .map_err(|e| anyhow::anyhow!("pagefind init: {e}"))?;
        let count = index
            .add_directory(out_dir.to_string_lossy().into_owned(), None)
            .await
            .map_err(|e| anyhow::anyhow!("pagefind index: {e}"))?;
        index
            .write_files(Some(
                out_dir.join("pagefind").to_string_lossy().into_owned(),
            ))
            .await
            .map_err(|e| anyhow::anyhow!("pagefind write: {e}"))?;
        println!("pagefind: indexed {count} pages");
        Ok::<(), anyhow::Error>(())
    })
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn serve(out_dir: &Path, port: u16) -> Result<()> {
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("bind failed: {e}"))?;
    println!("serving {} at http://127.0.0.1:{port}/", out_dir.display());
    for request in server.incoming_requests() {
        let url_path = request.url().split('?').next().unwrap_or("/").to_string();
        let mut rel = url_path.trim_start_matches('/').to_string();
        if rel.is_empty() || rel.ends_with('/') {
            rel.push_str("index.html");
        }
        let mut file = out_dir.join(&rel);
        if !file.exists() && file.extension().is_none() {
            file = out_dir.join(&rel).join("index.html");
        }
        if !file.exists() {
            file = out_dir.join("404.html");
        }
        match std::fs::read(&file) {
            Ok(body) => {
                let content_type = match file.extension().and_then(|e| e.to_str()) {
                    Some("html") => "text/html; charset=utf-8",
                    Some("css") => "text/css",
                    Some("js") => "text/javascript",
                    Some("json") => "application/json",
                    Some("svg") => "image/svg+xml",
                    Some("xml") => "application/xml",
                    Some("woff2") => "font/woff2",
                    Some("woff") => "font/woff",
                    Some("wasm") | Some("pagefind") => "application/octet-stream",
                    _ => "application/octet-stream",
                };
                let header =
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
                        .unwrap();
                let _ = request.respond(tiny_http::Response::from_data(body).with_header(header));
            }
            Err(_) => {
                let _ = request.respond(tiny_http::Response::empty(404));
            }
        }
    }
    Ok(())
}
