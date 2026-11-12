//! Paper references: arXiv ids and URLs, DOIs, and SSRN/NBER URLs mapped to their DOIs.

use std::fmt;

use crate::id::ArxivId;

/// A DOI in canonical form: lowercase, without prefix or trailing punctuation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct Doi(String);

impl Doi {
    /// Parses a bare `10.x/y`, `doi:10.x/y`, or a doi.org / dx.doi.org URL.
    pub fn parse(input: &str) -> Option<Doi> {
        let s = input
            .trim()
            .trim_start_matches(['(', '[', '<', '"', '\'', '`'])
            .trim_start();
        let s = strip_ci(s, "doi:")
            .or_else(|| strip_doi_host(s).map(|rest| rest.split(['?', '#']).next().unwrap_or("")))
            .unwrap_or(s);
        let decoded = percent_decode(s.trim());
        bare(&decoded)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The arXiv id behind an arXiv-assigned DOI (`10.48550/arXiv.2208.06046`).
    pub fn arxiv(&self) -> Option<ArxivId> {
        let id = ArxivId::parse(self.0.strip_prefix("10.48550/arxiv.")?).ok()?;
        Some(ArxivId {
            id: id.id,
            version: None,
        })
    }

    pub fn url(&self) -> String {
        format!("https://doi.org/{}", self.0)
    }

    /// File-name-safe form, e.g. `10.5555_sap-2026-13`.
    pub fn file_key(&self) -> String {
        self.0
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
}

impl fmt::Display for Doi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A reference that identifies a work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    Arxiv(ArxivId),
    Doi(Doi),
}

impl Ref {
    /// Parses user input: arXiv forms first, then DOI forms, SSRN and NBER URLs.
    /// Anything else (e.g. a landing page) has no id.
    pub fn parse(input: &str) -> Result<Ref, String> {
        if let Ok(id) = ArxivId::parse(input) {
            return Ok(Ref::Arxiv(id));
        }
        Doi::parse(input)
            .or_else(|| from_ssrn(input))
            .or_else(|| from_nber(input))
            .map(Ref::from_doi)
            .ok_or_else(|| {
                format!(
                    "no paper id in {input:?}: expected an arXiv id or URL, a DOI (10.x/y, \
                     doi:10.x/y, doi.org URL), an SSRN abstract URL, or an NBER paper URL; \
                     for other pages use --title"
                )
            })
    }

    /// The reference a note's `url` names: an arxiv.org abs URL, a doi.org URL,
    /// or an SSRN/NBER URL; `None` for other landing pages.
    pub fn from_note_url(url: &str) -> Option<Ref> {
        if let Some(id) = ArxivId::from_abs_url(url) {
            return Some(Ref::Arxiv(ArxivId {
                id: id.id,
                version: None,
            }));
        }
        let url = url.trim();
        strip_doi_host(strip_scheme(url))
            .and_then(|_| Doi::parse(url))
            .or_else(|| from_ssrn(url))
            .or_else(|| from_nber(url))
            .map(Ref::from_doi)
    }

    /// A DOI as a reference; arXiv DOIs become their arXiv id, so a paper cited
    /// by `10.48550/arXiv.X` and by `arXiv:X` is one work.
    pub fn from_doi(doi: Doi) -> Ref {
        match doi.arxiv() {
            Some(id) => Ref::Arxiv(id),
            None => Ref::Doi(doi),
        }
    }

    /// Index key shared by notes and lookups: `arxiv:<versionless id>` or `doi:<doi>`.
    pub fn key(&self) -> String {
        match self {
            Ref::Arxiv(id) => format!("arxiv:{}", id.id),
            Ref::Doi(doi) => format!("doi:{doi}"),
        }
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Arxiv(id) => write!(f, "arXiv:{id}"),
            Ref::Doi(doi) => write!(f, "doi:{doi}"),
        }
    }
}

fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn strip_scheme(s: &str) -> &str {
    strip_ci(s, "https://")
        .or_else(|| strip_ci(s, "http://"))
        .unwrap_or(s)
}

/// Path after a doi.org / dx.doi.org host (with or without scheme).
fn strip_doi_host(s: &str) -> Option<&str> {
    let s = strip_scheme(s);
    ["doi.org/", "dx.doi.org/", "www.doi.org/"]
        .iter()
        .find_map(|host| strip_ci(s, host))
}

/// Validates `10.<registrant>/<suffix>`, trims trailing punctuation, lowercases.
fn bare(s: &str) -> Option<Doi> {
    let mut s = s.trim_end_matches(['.', ',', ';', ':', '"', '\'', '`', '>', ']', '}']);
    // A closing parenthesis belongs to the DOI only when it is balanced there.
    while s.ends_with(')') && s.matches(')').count() > s.matches('(').count() {
        s = s[..s.len() - 1].trim_end_matches(['.', ',', ';', ':']);
    }
    let (prefix, suffix) = s.split_once('/')?;
    let registrant = prefix.strip_prefix("10.")?;
    let registrant_ok = registrant
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && (4..=9).contains(&registrant.split('.').next()?.len());
    let suffix_ok = !suffix.is_empty() && !suffix.chars().any(char::is_whitespace);
    (registrant_ok && suffix_ok).then(|| Doi(s.to_lowercase()))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_www(s: &str) -> &str {
    strip_ci(s, "www.").unwrap_or(s)
}

/// `papers.ssrn.com/sol3/papers.cfm?abstract_id=N` or `ssrn.com/abstract=N` → `10.2139/ssrn.N`.
fn from_ssrn(s: &str) -> Option<Doi> {
    let rest = strip_www(strip_scheme(s.trim()));
    let digits = if let Some(path) = strip_ci(rest, "papers.ssrn.com/") {
        let (_, query) = path.split_once('?')?;
        query.split('&').find_map(|kv| {
            kv.strip_prefix("abstract_id=")
                .or(kv.strip_prefix("abstractid="))
        })?
    } else {
        strip_ci(rest, "ssrn.com/abstract=")?
    };
    let digits: &str = digits.split(['&', '#', '/']).next()?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| Doi(format!("10.2139/ssrn.{digits}")))
}

/// `nber.org/papers/wN` (optionally `.pdf`) → `10.3386/wN`.
fn from_nber(s: &str) -> Option<Doi> {
    let rest = strip_www(strip_scheme(s.trim()));
    let path = strip_ci(rest, "nber.org/papers/")?;
    let path = path.split(['?', '#']).next()?.trim_end_matches('/');
    let path = path.strip_suffix(".pdf").unwrap_or(path);
    let digits = path.strip_prefix('w').or_else(|| path.strip_prefix('W'))?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| Doi(format!("10.3386/w{digits}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doi(input: &str) -> String {
        match Ref::parse(input).unwrap() {
            Ref::Doi(d) => d.to_string(),
            other => panic!("{input:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn parses_doi_forms() {
        let jfe = "10.5555/j.synth.2019.07.001";
        for input in [
            "10.5555/j.synth.2019.07.001",
            " 10.5555/J.SYNTH.2019.07.001 ",
            "doi:10.5555/j.synth.2019.07.001",
            "DOI:10.5555/j.synth.2019.07.001",
            "https://doi.org/10.5555/j.synth.2019.07.001",
            "http://dx.doi.org/10.5555/j.synth.2019.07.001",
            "doi.org/10.5555/j.synth.2019.07.001",
            "https://doi.org/10.5555%2Fj.synth.2019.07.001",
            "10.5555/j.synth.2019.07.001.",
            "(10.5555/j.synth.2019.07.001)",
            "https://doi.org/10.5555/j.synth.2019.07.001?x=1#y",
            "`10.5555/j.synth.2019.07.001`",
            "`10.5555/j.synth.2019.07.001`.",
        ] {
            assert_eq!(doi(input), jfe, "{input:?}");
        }
        assert_eq!(
            doi("10.5555/0304-405X(90)90011-X"),
            "10.5555/0304-405x(90)90011-x"
        );
        assert_eq!(doi("10.1002/(SICI)1097-0258"), "10.1002/(sici)1097-0258");
        assert_eq!(doi("10.5555/sap-2026-13;"), "10.5555/sap-2026-13");
        assert_eq!(doi("10.31235/osf.io/d5yx2"), "10.31235/osf.io/d5yx2");
    }

    #[test]
    fn maps_ssrn_and_nber_to_dois() {
        assert_eq!(
            doi("https://papers.ssrn.com/sol3/papers.cfm?abstract_id=9990001"),
            "10.2139/ssrn.9990001"
        );
        assert_eq!(
            doi("papers.ssrn.com/sol3/papers.cfm?foo=1&abstract_id=9990001&bar"),
            "10.2139/ssrn.9990001"
        );
        assert_eq!(
            doi("https://ssrn.com/abstract=9990001"),
            "10.2139/ssrn.9990001"
        );
        assert_eq!(
            doi("https://www.ssrn.com/abstract=9990001"),
            "10.2139/ssrn.9990001"
        );
        assert_eq!(doi("https://www.nber.org/papers/w99001"), "10.3386/w99001");
        assert_eq!(doi("http://nber.org/papers/w99001.pdf"), "10.3386/w99001");
        assert_eq!(doi("https://www.nber.org/papers/w99001/"), "10.3386/w99001");
    }

    #[test]
    fn arxiv_still_parsed_first() {
        for input in [
            "2208.06046v4",
            "arXiv:2208.06046",
            "https://arxiv.org/abs/2208.06046",
        ] {
            assert!(matches!(Ref::parse(input), Ok(Ref::Arxiv(_))), "{input}");
        }
        let r = Ref::parse("https://arxiv.org/abs/2208.06046v2").unwrap();
        assert_eq!(r.key(), "arxiv:2208.06046");
        assert_eq!(r.to_string(), "arXiv:2208.06046v2");
    }

    #[test]
    fn rejects_non_ids() {
        for bad in [
            "",
            "10.5555",
            "10.12/x",
            "10.5555/",
            "11.1016/x",
            "10.5555/a b",
            "https://www.example.org/conference/security23/presentation/doe",
            "https://www.example.org/2025/03/staff-working-paper-2025-12/",
            "https://papers.ssrn.com/sol3/papers.cfm?abstract_id=",
            "https://papers.ssrn.com/sol3/papers.cfm?abstract_id=12x",
            "https://www.nber.org/papers/x99001",
            "https://example.org/10.5555/j.synth.2019.07.001",
        ] {
            assert!(Ref::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn note_urls() {
        let key = |url: &str| Ref::from_note_url(url).map(|r| r.key());
        assert_eq!(
            key("https://arxiv.org/abs/2208.06046v3").as_deref(),
            Some("arxiv:2208.06046")
        );
        assert_eq!(
            key("https://doi.org/10.5555/rof.0004").as_deref(),
            Some("doi:10.5555/rof.0004")
        );
        assert_eq!(
            key("https://papers.ssrn.com/sol3/papers.cfm?abstract_id=9990001").as_deref(),
            Some("doi:10.2139/ssrn.9990001")
        );
        assert_eq!(
            key("https://www.nber.org/papers/w99001").as_deref(),
            Some("doi:10.3386/w99001")
        );
        assert_eq!(key("https://arxiv.org/pdf/2208.06046"), None);
        assert_eq!(key("https://www.example.org/publications/dp99001"), None);
        // A bare DOI is not a URL.
        assert_eq!(key("10.5555/rof.0004"), None);
        let d = Doi::parse("10.5555/sap-2026-13").unwrap();
        assert_eq!(d.file_key(), "10.5555_sap-2026-13");
        assert_eq!(d.url(), "https://doi.org/10.5555/sap-2026-13");
    }
}
