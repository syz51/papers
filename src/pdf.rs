//! Downloaded PDFs (untrusted data): detection, text extraction, and PDF links
//! on open-access landing pages.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::http::{host, resolve};

/// PDF links taken from one landing page.
const MAX_PAGE_LINKS: usize = 3;

/// A real PDF: a PDF content type and the `%PDF-` magic bytes.
pub fn is_pdf(content_type: &str, body: &[u8]) -> bool {
    content_type.to_ascii_lowercase().contains("pdf") && body.starts_with(b"%PDF-")
}

/// Pages larger than this are real pages, not check interstitials.
const CHALLENGE_PAGE_LIMIT: usize = 64 << 10;

/// Markers of a page that only runs a JavaScript check before serving the
/// content (lowercase): AWS WAF challenges (as on figshare) and generic notices.
const CHALLENGE_MARKERS: [&str; 6] = [
    "awswaf",
    "challenge.js",
    "javascript is required",
    "enable javascript to continue",
    "javascript must be enabled",
    "requires javascript to",
];

/// Whether a non-PDF reply is a JavaScript check page, the one case where
/// `read` retries in a browser: a 202 HTML reply (AWS WAF's challenge status)
/// or a small 200 HTML page carrying a challenge marker. Refusals (403, 429,
/// any other status) never are: those hosts block automated clients, and
/// that is reported, not worked around.
pub fn is_js_challenge(status: u16, content_type: &str, body: &[u8]) -> bool {
    if !content_type.to_ascii_lowercase().contains("html") {
        return false;
    }
    match status {
        202 => true,
        200 if body.len() <= CHALLENGE_PAGE_LIMIT => {
            let page = String::from_utf8_lossy(body).to_lowercase();
            CHALLENGE_MARKERS.iter().any(|m| page.contains(m))
        }
        _ => false,
    }
}

/// Extracts the text of a PDF. The parser can panic on malformed input; that
/// is reported as an error rather than aborting.
pub fn extract_text(bytes: &[u8]) -> Result<String, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }));
    std::panic::set_hook(hook);
    match result {
        Ok(Ok(text)) if text.trim().is_empty() => {
            Err("the PDF has no extractable text (scanned images?)".to_owned())
        }
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(format!("PDF text extraction failed: {e}")),
        Err(_) => Err("PDF text extraction failed: the parser panicked on this file".to_owned()),
    }
}

/// Download paths of open repositories whose file links lack a `.pdf` suffix.
const DOWNLOAD_PATHS: [&str; 1] = ["/ndownloader/files/"]; // figshare

/// PDF links on an HTML landing page at `base`: the `citation_pdf_url` meta tag,
/// then `href`s on the page's own host that end in `.pdf` or are a known
/// repository download path. At most a few, deduplicated.
pub fn page_pdf_links(html: &str, base: &str) -> Vec<String> {
    let Ok(base_host) = host(base) else {
        return Vec::new();
    };
    let mut links = Vec::new();
    let mut push = |raw: &str, same_host_only: bool| {
        let raw = raw.replace("&amp;", "&");
        if let Some(url) = resolve(base, &raw)
            && (!same_host_only || host(&url).is_ok_and(|h| h == base_host))
            && !links.contains(&url)
            && links.len() < MAX_PAGE_LINKS
        {
            links.push(url);
        }
    };
    let lower = html.to_ascii_lowercase();
    for (start, _) in lower.match_indices("<meta") {
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        let tag = &html[start..end];
        if attr(tag, "name").is_some_and(|n| n.eq_ignore_ascii_case("citation_pdf_url"))
            && let Some(content) = attr(tag, "content")
        {
            push(content, false);
        }
    }
    for (start, _) in lower.match_indices("href=") {
        let Some(value) = quoted(&html[start + 5..]) else {
            continue;
        };
        let path = value.split(['?', '#']).next().unwrap_or_default();
        let path = path.to_ascii_lowercase();
        if path.ends_with(".pdf") || DOWNLOAD_PATHS.iter().any(|p| path.contains(p)) {
            push(value, true);
        }
    }
    links
}

/// Value of `name="..."` (or single-quoted) inside a tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    lower
        .match_indices(&needle)
        .filter(|(i, _)| *i > 0 && tag.as_bytes()[i - 1].is_ascii_whitespace())
        .find_map(|(i, _)| quoted(&tag[i + needle.len()..]))
}

fn quoted(s: &str) -> Option<&str> {
    let quote = s.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &s[1..];
    rest.find(quote).map(|end| &rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_javascript_check_pages_count_as_challenges() {
        let html = "text/html; charset=utf-8";
        let notice = b"<html><body>JavaScript is required to view this page.</body></html>";
        assert!(is_js_challenge(
            202,
            html,
            b"<html><script src=x></script></html>"
        ));
        assert!(is_js_challenge(
            200,
            html,
            b"<script src=\"https://x.awswaf.com/challenge.js\">"
        ));
        assert!(is_js_challenge(200, html, notice));
        // Bot blocks and rate limits are reported, never retried in a browser.
        assert!(!is_js_challenge(403, html, notice));
        assert!(!is_js_challenge(429, html, notice));
        assert!(!is_js_challenge(
            200,
            html,
            b"<html><body>An ordinary landing page</body></html>"
        ));
        assert!(!is_js_challenge(200, "application/json", notice));
        let big = format!(
            "<html>{}JavaScript is required</html>",
            " ".repeat(CHALLENGE_PAGE_LIMIT)
        );
        assert!(!is_js_challenge(200, html, big.as_bytes()));
    }

    #[test]
    fn detects_pdfs_by_type_and_magic() {
        assert!(is_pdf("application/pdf", b"%PDF-1.7\n..."));
        assert!(is_pdf("Application/PDF; charset=binary", b"%PDF-1.4"));
        assert!(!is_pdf("text/html", b"%PDF-1.4"));
        assert!(!is_pdf("application/pdf", b"<!DOCTYPE html>"));
    }

    #[test]
    fn rejects_non_pdf_bytes() {
        assert!(extract_text(b"not a pdf").is_err());
    }

    #[test]
    fn finds_landing_page_pdf_links() {
        let html = r#"<html><head>
            <meta name="citation_title" content="X">
            <meta content="https://cdn.example.org/x.pdf" name="citation_pdf_url" />
            </head><body>
            <a href="/wp-content/uploads/2026/04/sap2026-13.pdf">PDF</a>
            <a href='https://other.org/elsewhere.pdf'>off-site</a>
            <a href="report.PDF?download=1&amp;x=2">rel</a>
            <a href="/page">not a pdf</a>
            <a href="javascript:x.pdf">bad</a>
            <a href="/wp-content/uploads/2026/04/sap2026-13.pdf">dup</a>
            </body></html>"#;
        let links = page_pdf_links(
            html,
            "https://www.example.org/2026/04/staff-analytical-paper-2026-13/",
        );
        assert_eq!(
            links,
            [
                "https://cdn.example.org/x.pdf",
                "https://www.example.org/wp-content/uploads/2026/04/sap2026-13.pdf",
                "https://www.example.org/2026/04/staff-analytical-paper-2026-13/report.PDF?download=1&x=2",
            ]
        );
        assert!(page_pdf_links(html, "not a url").is_empty());
    }

    #[test]
    fn finds_repository_download_links() {
        let html = r#"<a href="https://figshare.com/ndownloader/files/52627142/1">Download</a>
            <a href="https://figshare.com/articles/x/25886773">item</a>"#;
        assert_eq!(
            page_pdf_links(html, "https://figshare.com/articles/x/25886773"),
            ["https://figshare.com/ndownloader/files/52627142/1"]
        );
    }
}
