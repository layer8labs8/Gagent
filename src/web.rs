//! The internet for the agent: a web search, and a page read as plain text.
//!
//! What comes from the internet is data, never an instruction: the tools say so in front of
//! every result. Addresses on a private network are not read, a redirect to another host is
//! asked about again, and the user is asked before anything is fetched (see
//! [`crate::permission::Approvals::authorize_web`]).

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

use crate::http::Http;

/// Characters of a page that one call returns.
pub const MAX_PAGE_CHARS: usize = 12_000;
/// The biggest page that is downloaded.
const MAX_BODY_BYTES: u64 = 3 << 20;
/// Redirects followed one by one (each is looked at).
const MAX_HOPS: usize = 5;
/// Search results returned.
const MAX_HITS: usize = 8;
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Gagent; +https://github.com/layer8labs8/Gagent)";

/// An address that may be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub url: String,
    pub host: String,
    /// The user's own computer (a server being tested): no question is asked.
    pub local: bool,
}

/// A result of a web search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("the pattern is fixed and valid"))
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// The host in `user@host:port`.
fn host_of_authority(authority: &str) -> String {
    let host = if authority.starts_with('[') {
        authority.split_inclusive(']').next().unwrap_or(authority)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.to_ascii_lowercase()
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host.starts_with("127.") || host == "[::1]"
}

/// Addresses of the private network, of the link-local range (where cloud servers keep their
/// secrets), and names that only exist on a local network.
fn is_private(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
        let n: Vec<u8> = parts.iter().filter_map(|p| p.parse().ok()).collect();
        return matches!(
            (n[0], n[1]),
            (0, _) | (10, _) | (100, 64..=127) | (169, 254) | (172, 16..=31) | (192, 168) | (224..=255, _)
        );
    }
    if host.starts_with('[') {
        let h = host.trim_start_matches('[');
        return h.starts_with("fc") || h.starts_with("fd") || h.starts_with("fe8") || h.starts_with("fe9")
            || h.starts_with("fea") || h.starts_with("feb") || h.starts_with("::ffff:");
    }
    // Numbers in other forms (`2130706433`, `0x7f.1`) and single-word names (`router`).
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') || host.split('.').any(|l| l.starts_with("0x")) {
        return true;
    }
    !host.contains('.')
        || [".local", ".internal", ".lan", ".home", ".corp", ".intranet"]
            .iter()
            .any(|s| host.ends_with(s))
}

/// Looks at an address: only http and https, no user name in it, nothing on a private network.
pub fn check_url(raw: &str) -> Result<Target, String> {
    let raw = raw.trim();
    let url = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let (scheme, rest) = url.split_once("://").unwrap_or(("", ""));
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!("only http and https addresses can be read, not {scheme}:"));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err("the address has no host".into());
    }
    if authority.contains('@') {
        return Err("addresses with a user name in them are not read".into());
    }
    let host = host_of_authority(authority);
    let local = is_loopback(&host);
    if !local && is_private(&host) {
        return Err(format!(
            "{host} is on a private network or has no public name; only public web sites and localhost are read"
        ));
    }
    Ok(Target { url, host, local })
}

/// The full address of a link found on `base`; `None` for links that lead nowhere to read.
pub fn absolutize(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if href.is_empty()
        || href.starts_with('#')
        || ["mailto:", "javascript:", "tel:", "data:"].iter().any(|p| lower.starts_with(p))
    {
        return None;
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(href.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    let origin_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..origin_end]);
    if let Some(r) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{r}"));
    }
    if href.starts_with('/') {
        return Some(resolve_dots(&format!("{origin}{href}")));
    }
    let path = rest[origin_end..].split(['?', '#']).next().unwrap_or("");
    let dir = path.rfind('/').map_or("/", |i| &path[..=i]);
    Some(resolve_dots(&format!("{origin}{dir}{href}")))
}

/// Takes `.` and `..` out of the path of an address (`/a/b/../c` is `/a/c`).
fn resolve_dots(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let start = rest.find('/').unwrap_or(rest.len());
    let (host, tail) = rest.split_at(start);
    let cut = tail.find(['?', '#']).unwrap_or(tail.len());
    let (path, suffix) = tail.split_at(cut);
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    let mut path = parts.join("/");
    if path.is_empty() {
        path.push('/');
    } else if matches!(path.rsplit('/').next(), Some("")) || tail[..cut].ends_with("/.") || tail[..cut].ends_with("/..") {
        if !path.ends_with('/') {
            path.push('/');
        }
    }
    format!("{scheme}://{host}{path}{suffix}")
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// What a page says, as plain text.
#[derive(Clone, Debug)]
pub struct Page {
    pub url: String,
    pub text: String,
}

/// Reads a page. `approve` is asked when a redirect leads to another host.
pub fn fetch(
    target: &Target,
    approve: &dyn Fn(&Target) -> Result<(), String>,
) -> Result<Page, String> {
    let http = Http::without_redirects(Duration::from_secs(30), MAX_BODY_BYTES);
    let mut current = target.clone();
    for hop in 0..=MAX_HOPS {
        let response = http
            .get(
                &current.url,
                &[
                    ("User-Agent", USER_AGENT),
                    ("Accept", "text/html,text/plain,application/json;q=0.9,*/*;q=0.5"),
                ],
            )
            .map_err(|e| e.to_string())?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let Some(location) = &response.location else {
                return Err(format!("the server redirected ({}) without saying where", response.status));
            };
            if hop == MAX_HOPS {
                return Err("too many redirects".into());
            }
            let next = absolutize(&current.url, location)
                .ok_or_else(|| "the server redirected to an address that cannot be read".to_string())?;
            let next = check_url(&next)?;
            if next.host != current.host && !next.local {
                approve(&next)?;
            }
            current = next;
            continue;
        }
        if !response.is_success() {
            return Err(format!("the server answered HTTP {}", response.status));
        }
        let kind = response.content_type.clone().unwrap_or_default();
        let body = response.text();
        let looks_html = kind.contains("html")
            || body.trim_start().to_ascii_lowercase().starts_with("<!doctype html")
            || body.trim_start().to_ascii_lowercase().starts_with("<html");
        let text = if looks_html {
            html_to_text(&body, &current.url)
        } else if kind.is_empty()
            || kind.starts_with("text/")
            || ["json", "xml", "javascript", "yaml", "markdown", "toml"].iter().any(|k| kind.contains(k))
        {
            body
        } else {
            return Err(format!("this is not text ({kind})"));
        };
        return Ok(Page {
            url: current.url,
            text,
        });
    }
    Err("too many redirects".into())
}

/// `max` characters of `text` from character `start`, and where the rest begins (when there is more).
pub fn window(text: &str, start: usize, max: usize) -> (String, Option<usize>) {
    let total = text.chars().count();
    let shown: String = text.chars().skip(start).take(max).collect();
    let end = start + shown.chars().count();
    (shown, (end < total).then_some(end))
}

/// The text of an HTML page: no scripts, styles or tags; paragraphs and list items on their own
/// lines; links as `text (address)`.
pub fn html_to_text(html: &str, base_url: &str) -> String {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static JUNK: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    static ITEM: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    let title = regex(&TITLE, r"(?is)<title[^>]*>(.*?)</title>")
        .captures(html)
        .map(|c| plain(&c[1]))
        .filter(|t| !t.is_empty());
    // Pages with a main part (documentation, articles) are read from that part only.
    static MAIN: OnceLock<Regex> = OnceLock::new();
    let html = regex(&MAIN, r"(?is)<main\b.*?</main>|<article\b.*?</article>")
        .find(html)
        .map(|m| m.as_str())
        .filter(|m| m.len() > 800)
        .unwrap_or(html);
    let html = regex(
        &JUNK,
        r"(?is)<head\b.*?</head>|<script\b.*?</script>|<style\b.*?</style>|<noscript\b.*?</noscript>|<svg\b.*?</svg>|<template\b.*?</template>|<nav\b.*?</nav>|<footer\b.*?</footer>|<aside\b.*?</aside>|<!--.*?-->",
    )
    .replace_all(html, " ");
    let html = regex(
        &LINK,
        r#"(?is)<a\s[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#,
    )
    .replace_all(&html, |c: &regex::Captures| {
        let label = c[2].to_string();
        match absolutize(base_url, &c[1]) {
            Some(url) if url.len() <= 160 && !plain(&label).is_empty() => format!("{label} ({url})"),
            _ => label,
        }
    });
    let html = regex(&ITEM, r"(?i)<li\b[^>]*>").replace_all(&html, "\n- ");
    let html = regex(
        &BLOCK,
        r"(?i)</?(?:p|div|br|ul|ol|h[1-6]|tr|table|section|article|header|footer|pre|blockquote|hr|nav|main|form|dd|dt|dl)\b[^>]*>",
    )
    .replace_all(&html, "\n");
    let text = regex(&TAG, r"(?s)<[^>]*>").replace_all(&html, "");
    let text = decode_entities(&text);
    let mut out = String::new();
    if let Some(t) = title {
        out.push_str(&format!("# {t}\n"));
    }
    let mut blank = 0;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank == 1 && !out.ends_with("\n\n") {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// Text of a piece of HTML without tags, entities decoded, on one line.
fn plain(html: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let text = regex(&TAG, r"(?s)<[^>]*>").replace_all(html, " ");
    decode_entities(&text).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "hellip" => Some('…'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "copy" => Some('©'),
            _ => name
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Searches the web (DuckDuckGo's page without scripts).
pub fn search(query: &str) -> Result<Vec<Hit>, String> {
    let body = format!("q={}", crate::http::url_encode(query));
    let response = Http::new(Duration::from_secs(30), 2 << 20)
        .send(
            "POST",
            "https://html.duckduckgo.com/html/",
            &[("User-Agent", USER_AGENT), ("Accept", "text/html")],
            Some("application/x-www-form-urlencoded"),
            body.as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    if !response.is_success() {
        return Err(format!("the search service answered HTTP {}", response.status));
    }
    let hits = parse_results(&response.text());
    if hits.is_empty() {
        return Err("the search gave no results (the service may be busy: try again later, or read a page you know with web_fetch)".into());
    }
    Ok(hits)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The address a result link leads to (DuckDuckGo wraps it in a redirect).
fn result_url(href: &str) -> Option<String> {
    let href = href.replace("&amp;", "&");
    let url = match href.split_once("uddg=") {
        Some((_, rest)) => percent_decode(rest.split('&').next().unwrap_or(rest)),
        None => href.clone(),
    };
    let url = if url.starts_with("//") { format!("https:{url}") } else { url };
    (url.starts_with("http") && !url.contains("duckduckgo.com/y.js") && !url.contains("ad_provider"))
        .then_some(url)
}

/// The results in the page of the search service.
pub fn parse_results(html: &str) -> Vec<Hit> {
    static RESULT: OnceLock<Regex> = OnceLock::new();
    static SNIPPET: OnceLock<Regex> = OnceLock::new();
    static HREF: OnceLock<Regex> = OnceLock::new();
    let results: Vec<(usize, usize, String, String)> = regex(
        &RESULT,
        r#"(?is)(<a\s[^>]*class="[^"]*result__a[^"]*"[^>]*>)(.*?)</a>"#,
    )
    .captures_iter(html)
    .filter_map(|c| {
        let whole = c.get(0)?;
        let href = regex(&HREF, r#"href="([^"]+)""#).captures(&c[1])?[1].to_string();
        Some((whole.start(), whole.end(), href, plain(&c[2])))
    })
    .collect();
    let snippets: Vec<(usize, String)> = regex(
        &SNIPPET,
        r#"(?is)<a\s[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>"#,
    )
    .captures_iter(html)
    .filter_map(|c| Some((c.get(0)?.start(), plain(&c[1]))))
    .collect();
    let mut hits = Vec::new();
    for (i, (_, end, href, title)) in results.iter().enumerate() {
        let Some(url) = result_url(href) else {
            continue;
        };
        let next_start = results.get(i + 1).map_or(usize::MAX, |r| r.0);
        let snippet = snippets
            .iter()
            .find(|(s, _)| s > end && *s < next_start)
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        hits.push(Hit {
            title: title.clone(),
            url,
            snippet,
        });
        if hits.len() >= MAX_HITS {
            break;
        }
    }
    hits
}

/// The results as text for the model.
pub fn format_hits(hits: &[Hit]) -> String {
    let mut out = String::new();
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, h.title, h.url));
        if !h.snippet.is_empty() {
            out.push_str(&format!("   {}\n", h.snippet));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_web_addresses_are_read() {
        let ok = check_url("https://docs.rs/egui/latest/egui/").unwrap();
        assert_eq!((ok.host.as_str(), ok.local), ("docs.rs", false));
        assert_eq!(check_url("docs.rs/regex").unwrap().url, "https://docs.rs/regex");
        // The user's own server is fine, and needs no question.
        for local in ["http://localhost:8000/a", "http://127.0.0.1:5000", "http://[::1]:3000/x"] {
            assert!(check_url(local).unwrap().local, "{local}");
        }
        // Everything else that is not the public web is not read.
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/a",
            "http://192.168.1.1/admin",
            "http://10.0.0.5:8080",
            "http://172.20.3.4",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1",
            "http://[fd00::1]/",
            "http://router/",
            "http://printer.local/",
            "http://2130706433/",
            "http://0x7f.1/",
            "https://user:pw@example.com/",
            "https://",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn links_are_made_complete() {
        let base = "https://example.com/docs/guide/intro.html?x=1#top";
        assert_eq!(absolutize(base, "https://other.org/a").unwrap(), "https://other.org/a");
        assert_eq!(absolutize(base, "//cdn.example.com/x").unwrap(), "https://cdn.example.com/x");
        assert_eq!(absolutize(base, "/api/index.html").unwrap(), "https://example.com/api/index.html");
        assert_eq!(absolutize(base, "next.html").unwrap(), "https://example.com/docs/guide/next.html");
        assert_eq!(absolutize(base, "../up.html#x").unwrap(), "https://example.com/docs/up.html#x");
        assert_eq!(absolutize(base, "../../../x/./y.html?a=1").unwrap(), "https://example.com/x/y.html?a=1");
        assert_eq!(absolutize(base, "../").unwrap(), "https://example.com/docs/");
        for none in ["#section", "mailto:a@b.c", "javascript:void(0)", ""] {
            assert!(absolutize(base, none).is_none(), "{none}");
        }
    }

    #[test]
    fn a_page_becomes_readable_text() {
        let html = r#"<html><head><title>Guide &amp; Tips</title><style>p{color:red}</style></head>
<body><script>var secret = 1;</script><!-- hidden -->
<h1>Welcome</h1><p>Use <a href="/api/new.html">the new API</a> now.<br>It&#39;s &lt;fast&gt; &nbsp;and small.</p>
<ul><li>one</li><li>two <b>bold</b></li></ul><pre>let x = 1;</pre></body></html>"#;
        let text = html_to_text(html, "https://example.com/docs/");
        assert!(text.starts_with("# Guide & Tips"), "{text}");
        assert!(text.contains("Welcome") && text.contains("Use the new API (https://example.com/api/new.html) now."), "{text}");
        assert!(text.contains("It's <fast> and small."), "{text}");
        assert!(text.contains("- one") && text.contains("- two bold"), "{text}");
        assert!(text.contains("let x = 1;"), "{text}");
        assert!(!text.contains("secret") && !text.contains("color:red") && !text.contains("hidden"), "{text}");
    }

    #[test]
    fn the_main_part_of_a_page_is_enough() {
        let filler = "Important sentence about the library. ".repeat(30);
        let html = format!(
            "<html><body><nav><a href=\"/a\">Menu item</a></nav><div>Cookie banner</div><main><h1>API</h1><p>{filler}</p></main><footer>Copyright</footer></body></html>"
        );
        let text = html_to_text(&html, "https://example.com/");
        assert!(text.contains("API") && text.contains("Important sentence"), "{text}");
        assert!(!text.contains("Menu item") && !text.contains("Cookie banner") && !text.contains("Copyright"), "{text}");
        // A short main part does not hide the rest of the page.
        let short = "<html><body><div>Outside</div><main>tiny</main></body></html>";
        assert!(html_to_text(short, "https://example.com/").contains("Outside"));
    }

    #[test]
    fn a_long_page_is_read_in_windows() {
        let text = "abcdefghij".repeat(10); // 100 characters
        let (first, more) = window(&text, 0, 40);
        assert_eq!((first.chars().count(), more), (40, Some(40)));
        let (second, more) = window(&text, 40, 40);
        assert_eq!(second, text.chars().skip(40).take(40).collect::<String>());
        assert_eq!(more, Some(80));
        let (last, more) = window(&text, 80, 40);
        assert_eq!((last.chars().count(), more), (20, None));
        assert_eq!(window("short", 0, 40), ("short".to_string(), None));
    }

    /// Needs the internet: `cargo test real_internet -- --ignored`.
    #[test]
    #[ignore]
    fn real_internet() {
        let hits = search("rust egui TextEdit").expect("a search");
        assert!(!hits.is_empty());
        println!("{}", format_hits(&hits));
        let target = check_url(&hits[0].url).unwrap();
        let page = fetch(&target, &|_| Ok(())).expect("a page");
        let (first, more) = window(&page.text, 0, MAX_PAGE_CHARS);
        println!("{}\n...\n[{} chars in the first part, more: {more:?}]", &first[..first.len().min(1200)], first.len());
        assert!(page.text.len() > 200);
    }

    #[test]
    fn search_results_are_read_from_the_page() {
        let html = r#"
<div class="result"><h2><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fegui%2Flatest%2F&amp;rut=abc">egui - <b>Rust</b></a></h2>
<a class="result__snippet" href="x">An easy-to-use <b>immediate mode</b> GUI.</a></div>
<div class="result"><h2><a class="result__a" href="https://github.com/emilk/egui">emilk/egui</a></h2>
<a class="result__snippet" href="y">egui on GitHub</a></div>
<div class="result"><h2><a class="result__a" href="//duckduckgo.com/y.js?ad_provider=bingv7aa&u=1">An advertisement</a></h2></div>"#;
        let hits = parse_results(html);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].title, "egui - Rust");
        assert_eq!(hits[0].url, "https://docs.rs/egui/latest/");
        assert_eq!(hits[0].snippet, "An easy-to-use immediate mode GUI.");
        assert_eq!(hits[1].url, "https://github.com/emilk/egui");
        assert_eq!(hits[1].snippet, "egui on GitHub");
        let text = format_hits(&hits);
        assert!(text.starts_with("1. egui - Rust\n   https://docs.rs/egui/latest/\n"), "{text}");
        assert!(parse_results("<html>nothing</html>").is_empty());
    }
}
