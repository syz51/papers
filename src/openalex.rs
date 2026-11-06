//! OpenAlex API (`api.openalex.org`): title search, abstracts, and open-access locations.
//!
//! Works without a key (a small daily budget shared per IP address; single-work
//! lookups are free, searches cost credits). An optional key, from
//! `OPENALEX_API_KEY` or else (on macOS) the login Keychain item
//! `papers-openalex-api-key`, is sent as a bearer token to api.openalex.org only.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::http::{Get, Http, encode};
use crate::refs::Doi;
use crate::text::strip_markup;

const HOST: &str = "api.openalex.org";
const ENDPOINT: &str = "https://api.openalex.org/works";
pub const KEY_VAR: &str = "OPENALEX_API_KEY";
/// macOS Keychain service name holding the key when `KEY_VAR` is unset.
pub const KEYCHAIN_SERVICE: &str = "papers-openalex-api-key";

/// The optional API key: `KEY_VAR`, else the Keychain item; looked up once per process.
fn api_key() -> Option<&'static str> {
    static KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        std::env::var(KEY_VAR)
            .ok()
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty())
            .or_else(keychain_key)
    })
    .as_deref()
}

#[cfg(target_os = "macos")]
fn keychain_key() -> Option<String> {
    /// `security` exit status for errSecItemNotFound: no key stored, which is fine.
    const NOT_FOUND: i32 = 44;
    let output = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_owned()).filter(|k| !k.is_empty())
        }
        Ok(o) if o.status.code() == Some(NOT_FOUND) => None,
        Ok(o) => {
            eprintln!(
                "papers: could not read Keychain item {KEYCHAIN_SERVICE} (security exited {}); \
                 continuing without an OpenAlex key",
                o.status
            );
            None
        }
        Err(e) => {
            eprintln!(
                "papers: could not run /usr/bin/security ({e}); continuing without an OpenAlex key"
            );
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_key() -> Option<String> {
    None
}

#[derive(Debug, Deserialize)]
struct RawWork {
    id: String,
    doi: Option<String>,
    title: Option<String>,
    display_name: Option<String>,
    publication_year: Option<i64>,
    publication_date: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    authorships: Vec<Authorship>,
    primary_location: Option<RawLocation>,
    #[serde(default)]
    locations: Vec<RawLocation>,
    open_access: Option<OpenAccess>,
    abstract_inverted_index: Option<BTreeMap<String, Vec<usize>>>,
    #[serde(default)]
    referenced_works: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Authorship {
    author: Option<AuthorRef>,
    raw_author_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorRef {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLocation {
    #[serde(default)]
    is_oa: bool,
    pdf_url: Option<String>,
    landing_page_url: Option<String>,
    version: Option<String>,
    source: Option<Source>,
}

#[derive(Debug, Deserialize)]
struct Source {
    display_name: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAccess {
    oa_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchPage {
    #[serde(default)]
    results: Vec<RawWork>,
    meta: Option<SearchMeta>,
}

#[derive(Debug, Deserialize)]
struct SearchMeta {
    count: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub pdf_url: Option<String>,
    pub landing_page_url: Option<String>,
    pub version: Option<String>,
    pub source: Option<String>,
}

/// An OpenAlex work.
#[derive(Debug, Clone, Serialize)]
pub struct Work {
    /// Short id, e.g. `W9000000001`.
    pub id: String,
    pub doi: Option<Doi>,
    pub title: String,
    pub authors: Vec<String>,
    pub publication_year: Option<i64>,
    pub publication_date: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Primary location's source (journal, repository, or publisher series).
    pub venue: Option<String>,
    /// True when the primary source is a journal and the work an article.
    pub journal_article: bool,
    pub oa_status: Option<String>,
    /// Locations OpenAlex marks open access, in OpenAlex's order.
    pub oa_locations: Vec<Location>,
    #[serde(rename = "abstract")]
    pub abstract_text: Option<String>,
    /// Short ids of the works this one cites, in OpenAlex's order.
    #[serde(skip)]
    pub referenced_works: Vec<String>,
}

/// `W123` from `https://openalex.org/W123` or `W123`.
pub fn short_id(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

impl From<RawWork> for Work {
    fn from(w: RawWork) -> Self {
        let venue_source = w.primary_location.as_ref().and_then(|l| l.source.as_ref());
        let journal_article = w.kind.as_deref() == Some("article")
            && venue_source.and_then(|s| s.kind.as_deref()) == Some("journal");
        Work {
            id: short_id(&w.id).to_owned(),
            referenced_works: w
                .referenced_works
                .iter()
                .map(|r| short_id(r).to_owned())
                .collect(),
            doi: w.doi.as_deref().and_then(Doi::parse),
            title: w
                .title
                .or(w.display_name)
                .map(|t| strip_markup(&t))
                .unwrap_or_default(),
            authors: w
                .authorships
                .into_iter()
                .filter_map(|a| {
                    a.author
                        .and_then(|r| r.display_name)
                        .or(a.raw_author_name)
                        .map(|n| crate::text::squash(&n))
                })
                .collect(),
            publication_year: w.publication_year,
            publication_date: w.publication_date,
            kind: w.kind,
            venue: venue_source.and_then(|s| s.display_name.clone()),
            journal_article,
            oa_status: w.open_access.and_then(|o| o.oa_status),
            oa_locations: w
                .locations
                .into_iter()
                .filter(|l| l.is_oa)
                .map(|l| Location {
                    pdf_url: l.pdf_url,
                    landing_page_url: l.landing_page_url,
                    version: l.version,
                    source: l.source.and_then(|s| s.display_name),
                })
                .collect(),
            abstract_text: w
                .abstract_inverted_index
                .as_ref()
                .map(reconstruct_abstract)
                .filter(|a| !a.is_empty()),
        }
    }
}

/// Rebuilds abstract text from OpenAlex's `{word: [positions]}` index.
pub fn reconstruct_abstract(index: &BTreeMap<String, Vec<usize>>) -> String {
    let mut slots: Vec<(usize, &str)> = index
        .iter()
        .flat_map(|(word, positions)| positions.iter().map(move |&p| (p, word.as_str())))
        .collect();
    slots.sort_unstable();
    let words: Vec<&str> = slots.into_iter().map(|(_, w)| w).collect();
    words.join(" ")
}

pub fn parse_work(json: &str) -> Result<Work, String> {
    serde_json::from_str::<RawWork>(json)
        .map(Work::from)
        .map_err(|e| format!("invalid OpenAlex JSON: {e}"))
}

pub fn parse_search(json: &str) -> Result<Vec<Work>, String> {
    serde_json::from_str::<SearchPage>(json)
        .map(|p| p.results.into_iter().map(Work::from).collect())
        .map_err(|e| format!("invalid OpenAlex JSON: {e}"))
}

pub struct Client<'a> {
    http: &'a Http,
    headers: Vec<(&'static str, String)>,
}

impl<'a> Client<'a> {
    /// Uses the optional API key from the environment or the Keychain.
    pub fn new(http: &'a Http) -> Self {
        let headers = api_key()
            .map(|k| vec![("authorization", format!("Bearer {k}"))])
            .unwrap_or_default();
        Client { http, headers }
    }

    fn get(&self, url: &str) -> Result<Option<String>, String> {
        self.get_with(url, true)
    }

    /// `cache`: lookups may come from the response cache; searches never do.
    fn get_with(&self, url: &str, cache: bool) -> Result<Option<String>, String> {
        let result = self.http.get(&Get {
            headers: &self.headers,
            headers_host: HOST,
            cache,
            ..Get::api(url, &[200, 404])
        });
        match result {
            Ok(reply) if reply.status == 404 => Ok(None),
            Ok(reply) => Ok(Some(reply.text())),
            Err(e) => Err(self.explain(e)),
        }
    }

    /// Adds what to do about a key to budget and authorization failures.
    fn explain(&self, error: String) -> String {
        let keyed = !self.headers.is_empty();
        if error.contains("HTTP 401") || error.contains("HTTP 403") {
            if keyed {
                format!(
                    "{error} (OpenAlex rejected the API key from {KEY_VAR} or the \
                     Keychain item {KEYCHAIN_SERVICE}; check the key)"
                )
            } else {
                format!(
                    "{error} (OpenAlex requires an API key for this request; set {KEY_VAR} \
                     or store it in the Keychain item {KEYCHAIN_SERVICE})"
                )
            }
        } else if error.contains("HTTP 429") && !keyed {
            format!(
                "{error} (OpenAlex's keyless budget is shared per IP address; set {KEY_VAR} \
                 or the Keychain item {KEYCHAIN_SERVICE} to a free key from \
                 https://openalex.org/settings/api)"
            )
        } else {
            error
        }
    }

    /// Looks up a work by DOI; `Ok(None)` when OpenAlex has no record.
    pub fn by_doi(&self, doi: &Doi) -> Result<Option<Work>, String> {
        let url = format!("{ENDPOINT}/doi:{}", encode(doi.as_str(), b"/"));
        self.get(&url)?.map(|json| parse_work(&json)).transpose()
    }

    /// Full-text search, best matches first; returns the total match count and
    /// up to `max` works. `since` is `YYYY-MM-DD`. Never cached.
    pub fn search(
        &self,
        query: &str,
        since: Option<&str>,
        max: usize,
    ) -> Result<(Option<u64>, Vec<Work>), String> {
        let mut url = format!("{ENDPOINT}?search={}", encode(query.trim(), b""));
        if let Some(date) = since {
            url += &format!("&filter=from_publication_date:{date}");
        }
        self.paged(&url, max, false)
    }

    /// Works matching a filter such as `cites:W1` or `openalex:W1|W2`.
    pub fn list(&self, filter: &str, max: usize) -> Result<(Option<u64>, Vec<Work>), String> {
        let url = format!("{ENDPOINT}?filter={}", encode(filter, b":|"));
        self.paged(&url, max, true)
    }

    /// Pages through a list endpoint at up to 100 works per request.
    fn paged(
        &self,
        base: &str,
        max: usize,
        cache: bool,
    ) -> Result<(Option<u64>, Vec<Work>), String> {
        paged_with(base, max, |url| self.get_with(url, cache))
    }

    /// Looks up a work by its OpenAlex id (`W123`); `Ok(None)` when unknown.
    pub fn by_id(&self, id: &str) -> Result<Option<Work>, String> {
        let url = format!("{ENDPOINT}/{}", encode(short_id(id), b""));
        self.get(&url)?.map(|json| parse_work(&json)).transpose()
    }

    /// Searches works by title text; best matches first.
    pub fn search_title(&self, title: &str, per_page: usize) -> Result<Vec<Work>, String> {
        let url = format!(
            "{ENDPOINT}?search={}&per_page={per_page}",
            encode(title.trim(), b"")
        );
        match self.get(&url)? {
            Some(json) => parse_search(&json),
            None => Ok(Vec::new()),
        }
    }
}

/// The paging loop behind [`Client::paged`]. `per_page` stays fixed for the
/// whole run, since OpenAlex's `page=N` offset is `(N - 1) * per_page`.
fn paged_with(
    base: &str,
    max: usize,
    mut fetch: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<(Option<u64>, Vec<Work>), String> {
    let per_page = max.clamp(1, 100);
    let mut works = Vec::new();
    let mut total = None;
    let mut page = 1;
    while works.len() < max {
        let url = format!("{base}&per_page={per_page}&page={page}");
        let Some(json) = fetch(&url)? else {
            break;
        };
        let parsed: SearchPage =
            serde_json::from_str(&json).map_err(|e| format!("invalid OpenAlex JSON: {e}"))?;
        total = total.or(parsed.meta.and_then(|m| m.count));
        let got = parsed.results.len();
        works.extend(parsed.results.into_iter().map(Work::from));
        if got < per_page {
            break;
        }
        page += 1;
    }
    works.truncate(max);
    Ok((total, works))
}

#[cfg(test)]
mod tests {
    #[test]
    fn pages_keep_one_page_size() {
        // A synthetic index of 250 works, W1..W250, served by page and per_page.
        let serve = |url: &str| -> Result<Option<String>, String> {
            let arg = |name: &str| -> usize {
                let at =
                    url.find(&format!("&{name}=")).expect("parameter present") + name.len() + 2;
                url[at..].split('&').next().unwrap().parse().unwrap()
            };
            let (per_page, page) = (arg("per_page"), arg("page"));
            let results: Vec<String> = ((page - 1) * per_page + 1..=(page * per_page).min(250))
                .map(|n| format!(r#"{{"id":"https://openalex.org/W{n}","title":"Work {n}"}}"#))
                .collect();
            Ok(Some(format!(
                r#"{{"meta":{{"count":250}},"results":[{}]}}"#,
                results.join(",")
            )))
        };
        let (total, works) =
            paged_with("https://api.openalex.org/works?filter=x", 150, serve).unwrap();
        assert_eq!(total, Some(250));
        let ids: Vec<String> = works.iter().map(|w| w.id.clone()).collect();
        let want: Vec<String> = (1..=150).map(|n| format!("W{n}")).collect();
        assert_eq!(ids, want);
    }

    use super::*;

    // Synthetic replies in the OpenAlex API's shape (see tests/fixtures/README.md).
    const REPORT: &str = include_str!("../tests/fixtures/openalex-report.json");
    const JOURNAL: &str = include_str!("../tests/fixtures/openalex-journal.json");
    const SEARCH: &str = include_str!("../tests/fixtures/openalex-search.json");

    #[test]
    fn reconstructs_abstract_from_inverted_index() {
        let index: BTreeMap<String, Vec<usize>> =
            serde_json::from_str(r#"{"the": [0, 3], "cat": [1], "saw": [2], "dog.": [4]}"#)
                .unwrap();
        assert_eq!(reconstruct_abstract(&index), "the cat saw the dog.");
        let w = parse_work(REPORT).unwrap();
        let abs = w.abstract_text.unwrap();
        assert!(
            abs == "We study decentralized lending on a synthetic protocol, the largest toy \
                    lending venue.",
            "{abs}"
        );
    }

    #[test]
    fn parses_work_with_oa_locations() {
        let w = parse_work(JOURNAL).unwrap();
        assert_eq!(w.id, "W9000000001");
        assert_eq!(
            w.doi.as_ref().map(Doi::as_str),
            Some("10.5555/synth.2020.0001")
        );
        assert_eq!(w.title, "Widget Arbitrage Across Synthetic Exchanges");
        assert_eq!(w.authors, ["Ada Quill", "Bram Tolliver"]);
        assert_eq!(w.referenced_works, ["W9000000101", "W9000000102"]);
        assert_eq!(w.publication_year, Some(2018));
        assert_eq!(w.venue.as_deref(), Some("Journal of Synthetic Finance"));
        assert!(w.journal_article);
        assert_eq!(w.oa_status.as_deref(), Some("green"));
        // Some publishers withhold abstracts.
        assert_eq!(w.abstract_text, None);
        // Only locations marked open access are kept.
        assert_eq!(w.oa_locations.len(), 1);
        assert_eq!(
            w.oa_locations[0].pdf_url.as_deref(),
            Some("https://repository.example.edu/eprint/42/widget-arbitrage.pdf")
        );
        let report = parse_work(REPORT).unwrap();
        assert!(!report.journal_article);
        assert_eq!(
            report.venue.as_deref(),
            Some("Example Central Bank Research")
        );
    }

    #[test]
    fn parses_search_results() {
        let results = parse_search(SEARCH).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0].title,
            "Network-Based Detection of Synthetic Wash Trading"
        );
        assert_eq!(
            results[0].doi.as_ref().map(Doi::as_str),
            Some("10.5555/ssrn.0000111")
        );
        assert_eq!(results[0].authors.len(), 4);
        assert_eq!(results[0].authors[3], "K. Wendmark");
        assert_eq!(results[1].doi, None);
        // A missing title falls back to the display name, without markup.
        assert_eq!(results[2].title, "Order-Book Spoofing in Simulated Venues");
    }
}
