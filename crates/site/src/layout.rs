use crate::data::{format_date, SiteData};
use crate::html::{esc, esc_attr};

pub const DEFAULT_DESCRIPTION: &str = "The Australian federal record, unedited. Parliamentarians, divisions, bills and election results generated from official sources.";

const GTAG_SCRIPT: &str = "\n      window.dataLayer = window.dataLayer || []\n      function gtag() {\n        dataLayer.push(arguments)\n      }\n      gtag('js', new Date())\n      gtag('config', 'G-ZHJ3V1JWPX')\n    ";

// Compiled quick-search behaviour, inlined at the end of every page.
const QUICK_SEARCH_JS: &str = include_str!("../assets/js/quick-search.js");

const NAV: [(&str, &str); 6] = [
    ("People", "/people/"),
    ("Divisions", "/divisions/"),
    ("Bills", "/bills/"),
    ("Electorates", "/electorates/"),
    ("Parties", "/parties/"),
    ("Search", "/search/"),
];

fn source_label(name: &str) -> &str {
    match name {
        "wikidata" => "Wikidata",
        "aec" => "AEC",
        "aec-profiles" => "AEC profiles",
        "tvfy" => "They Vote For You",
        "aph-bills" => "APH bills",
        "handbook" => "Parliamentary Handbook",
        other => other,
    }
}

pub struct Page {
    pub title: String,
    pub description: Option<String>,
    /// URL path with trailing slash, e.g. "/people/anthony-albanese/".
    pub path: String,
    pub body: String,
    /// Rendered right before </main>, matching the authored script position.
    pub page_script: Option<&'static str>,
    pub footer_note: Option<String>,
}

impl Page {
    pub fn new(
        title: impl Into<String>,
        description: Option<String>,
        path: impl Into<String>,
        body: String,
    ) -> Self {
        Page {
            title: title.into(),
            description,
            path: path.into(),
            body,
            page_script: None,
            footer_note: None,
        }
    }
}

pub fn render(data: &SiteData, site_url: &str, css_href: &str, page: &Page) -> String {
    let description = page.description.as_deref().unwrap_or(DEFAULT_DESCRIPTION);
    let title_tag = if page.title == "pollywiki" {
        page.title.clone()
    } else {
        format!("{} · pollywiki", page.title)
    };

    let mut out = String::with_capacity(page.body.len() + 6 * 1024);
    out.push_str("<!DOCTYPE html><html lang=\"en-AU\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>");
    out.push_str(&esc(&title_tag));
    out.push_str("</title><meta name=\"description\" content=\"");
    out.push_str(&esc_attr(description));
    out.push_str("\"><link rel=\"icon\" href=\"/favicon.svg\" type=\"image/svg+xml\"><link rel=\"canonical\" href=\"");
    out.push_str(&esc_attr(&format!(
        "{}{}",
        site_url.trim_end_matches('/'),
        page.path
    )));
    out.push_str("\"><meta property=\"og:title\" content=\"");
    out.push_str(&esc_attr(&page.title));
    out.push_str("\"><meta property=\"og:description\" content=\"");
    out.push_str(&esc_attr(description));
    out.push_str("\"><meta property=\"og:type\" content=\"website\"><meta name=\"generator\" content=\"pollywiki\"><!-- Google tag (gtag.js) --><script async src=\"https://www.googletagmanager.com/gtag/js?id=G-ZHJ3V1JWPX\"></script><script>");
    out.push_str(GTAG_SCRIPT);
    out.push_str("</script>");
    out.push_str("<link rel=\"stylesheet\" href=\"");
    out.push_str(css_href);
    out.push_str("\"></head><body>");

    if data.meta.sample {
        out.push_str("<div class=\"sample-banner\">Sample data only. This build is not showing the parliamentary record.</div>");
    }

    out.push_str("<header class=\"site-header\"><div class=\"wrap\"><a class=\"wordmark\" href=\"/\">pollywiki<span class=\"tld\">.au</span></a><nav class=\"site-nav\" aria-label=\"Primary\">");
    let current = page.path.as_str();
    for (label, href) in NAV {
        if current.starts_with(href) {
            out.push_str(&format!(
                "<a href=\"{href}\" aria-current=\"page\">{label}</a>"
            ));
        } else {
            out.push_str(&format!("<a href=\"{href}\">{label}</a>"));
        }
    }
    out.push_str("</nav><div class=\"quick-search\"><input type=\"search\" id=\"quick-search-input\" placeholder=\"Find a person or electorate\" autocomplete=\"off\" aria-label=\"Find a person or electorate\"><ul id=\"quick-search-results\" hidden></ul></div></div></header><main class=\"wrap\">");
    out.push_str(&page.body);
    if let Some(script) = page.page_script {
        out.push_str("<script type=\"module\">");
        out.push_str(script);
        out.push_str("</script>");
    }
    out.push_str("</main><footer class=\"site-footer\"><div class=\"wrap\">");
    if let Some(note) = &page.footer_note {
        out.push_str(note);
    }
    out.push_str("<p class=\"disclaimer\">This service does not evaluate politicians or laws. Every page reproduces official records, linked to their source; machine-written summaries are labelled \u{201C}AI-generated\u{201D} and are never part of the record.</p><div class=\"freshness\">");
    for (name, status) in &data.meta.sources {
        out.push_str(&format!(
            "<span class=\"{}\">{} · {}</span>",
            if status.ok { "ok" } else { "stale" },
            esc(source_label(name)),
            esc(&format_date(
                &status.last_sync.chars().take(10).collect::<String>()
            ))
        ));
    }
    out.push_str(&format!(
        "<span>built {}</span>",
        esc(&format_date(
            &data.meta.generated_at.chars().take(10).collect::<String>()
        ))
    ));
    out.push_str("</div><nav aria-label=\"About\"><a href=\"/about/\">About</a><a href=\"/about/data-sources/\">Data sources</a><a href=\"/about/methodology/\">Methodology</a><a href=\"/about/corrections/\">Corrections</a><a href=\"https://github.com/logan-han/pollywiki\">Source code</a></nav><p class=\"legal\">Voting data © <a href=\"https://theyvoteforyou.org.au\">They Vote For You</a> (ODbL). Election data © AEC (CC BY 4.0). Photos via Wikimedia Commons, credited per page.</p></div></footer><script type=\"module\">");
    out.push_str(QUICK_SEARCH_JS);
    out.push_str("</script></body></html>");
    out
}
