//! arXiv API client: batched lookups, search paging, and query building.

use std::collections::HashMap;

use jiff::civil::Date;

use crate::atom::{Feed, Paper, parse_feed};
use crate::http::{ATTEMPTS, Get, Http};
use crate::id::ArxivId;

const ENDPOINT: &str = "https://export.arxiv.org/api/query";
/// arXiv's OAI-PMH interface: the only machine-readable source of each
/// version's withdrawal flag (the Atom API keeps the withdrawn version's comment
/// and abstract).
pub const OAI_ENDPOINT: &str = "https://oaipmh.arxiv.org/oai";
const OAI: &str = "http://www.openarchives.org/OAI/2.0/";
const ARXIV_RAW: &str = "http://arxiv.org/OAI/arXivRaw/";
/// Attempts per request while splitting a batch whose response could not be
/// read, to isolate the id behind it.
const SPLIT_ATTEMPTS: u32 = 2;
pub const MAX_PER_REQUEST: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Sort {
    Submitted,
    Updated,
    Relevance,
}

impl Sort {
    fn api_name(self) -> &'static str {
        match self {
            Sort::Submitted => "submittedDate",
            Sort::Updated => "lastUpdatedDate",
            Sort::Relevance => "relevance",
        }
    }
}

pub struct Client<'a> {
    http: &'a Http,
    endpoint: &'a str,
}

/// A response the caller interprets: 200, or 400 (arXiv reports bad ids and
/// query syntax as a 400 whose body is a feed with `Error` entries).
struct Reply {
    status: u16,
    body: String,
}

impl<'a> Client<'a> {
    pub fn new(http: &'a Http) -> Self {
        Client {
            http,
            endpoint: ENDPOINT,
        }
    }

    /// A client for a test server's endpoint.
    #[cfg(test)]
    pub(crate) fn at(http: &'a Http, endpoint: &'a str) -> Self {
        Client { http, endpoint }
    }

    /// `cache`: id lookups may come from the response cache; searches never do.
    fn get(&self, url: &str, attempts: u32, cache: bool) -> Result<Reply, String> {
        let reply = self.http.get(&Get {
            attempts,
            cache,
            ..Get::api(url, &[200, 400])
        })?;
        Ok(Reply {
            status: reply.status,
            body: reply.text(),
        })
    }

    /// Looks up ids in batches; returns one result per input id, in input order.
    pub fn fetch(&self, ids: &[ArxivId]) -> Vec<Result<Paper, String>> {
        let mut unique: Vec<&ArxivId> = Vec::new();
        for id in ids {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        let mut found: HashMap<String, Paper> = HashMap::new();
        let mut failed: HashMap<String, String> = HashMap::new();
        // The API answers HTTP 500 for an old-style id with a non-latest version
        // (e.g. hep-th/9901001v1), failing every id in the batch: send those alone.
        let (alone, batched): (Vec<&ArxivId>, Vec<&ArxivId>) = unique
            .into_iter()
            .partition(|id| id.version.is_some() && id.id.contains('/'));
        let mut batches = batched.chunks(MAX_PER_REQUEST).chain(alone.chunks(1));
        for batch in batches.by_ref() {
            if let Err(message) = self.fetch_batch(batch, ATTEMPTS, &mut found, &mut failed) {
                // The API is refusing or failing requests after the retries:
                // stop instead of sending it more.
                for id in batches.flatten() {
                    failed.insert(
                        id.to_string(),
                        format!("not attempted: an earlier arXiv request failed ({message})"),
                    );
                }
                break;
            }
        }
        ids.iter()
            .map(|id| {
                let key = id.to_string();
                if let Some(paper) = found.get(&key) {
                    Ok(paper.clone())
                } else if let Some(message) = failed.get(&key) {
                    Err(message.clone())
                } else {
                    Err("not found: arXiv returned no entry for this id".to_owned())
                }
            })
            .collect()
    }

    /// One batch. A response that cannot be read as a feed is split in half to
    /// isolate the id behind it; a request that fails after its retries (429,
    /// 5xx, transport) fails the batch and returns `Err` so the caller stops.
    fn fetch_batch(
        &self,
        batch: &[&ArxivId],
        attempts: u32,
        found: &mut HashMap<String, Paper>,
        failed: &mut HashMap<String, String>,
    ) -> Result<(), String> {
        let mut pending: Vec<&ArxivId> = batch.to_vec();
        while !pending.is_empty() {
            let url = id_list_url(self.endpoint, &pending);
            let reply = match self.get(&url, attempts, true) {
                Ok(reply) => reply,
                Err(message) => {
                    for id in pending {
                        failed.insert(id.to_string(), message.clone());
                    }
                    return Err(message);
                }
            };
            let status = reply.status;
            let feed = match parse_feed(&reply.body) {
                Ok(feed) => feed,
                Err(message) if pending.len() > 1 => {
                    eprintln!(
                        "papers: unreadable response for a batch of {} ids ({message}); \
                         splitting it to isolate the id behind it",
                        pending.len()
                    );
                    let (left, right) = pending.split_at(pending.len() / 2);
                    if let Err(e) = self.fetch_batch(left, SPLIT_ATTEMPTS, found, failed) {
                        for id in right {
                            failed.insert(
                                id.to_string(),
                                format!("not attempted: an earlier arXiv request failed ({e})"),
                            );
                        }
                        return Err(e);
                    }
                    return self.fetch_batch(right, SPLIT_ATTEMPTS, found, failed);
                }
                Err(message) => {
                    for id in pending {
                        failed.insert(id.to_string(), message.clone());
                    }
                    return Ok(());
                }
            };
            // arXiv names a rejected id as "... for <id>"; a 400 rejects the whole batch.
            let rejected: Vec<(&ArxivId, &String)> = pending
                .iter()
                .filter_map(|id| {
                    let suffix = format!(" for {id}");
                    feed.errors
                        .iter()
                        .find(|m| m.ends_with(&suffix))
                        .map(|m| (*id, m))
                })
                .collect();
            for (id, message) in &rejected {
                failed.insert(id.to_string(), format!("arXiv error: {message}"));
            }
            if status == 400 {
                if rejected.is_empty() {
                    let message = describe_errors(&feed, &url);
                    for id in pending {
                        failed.insert(id.to_string(), message.clone());
                    }
                    return Ok(());
                }
                pending.retain(|id| !rejected.iter().any(|(r, _)| r == id));
                continue;
            }
            for paper in feed.papers {
                found.insert(format!("{}v{}", paper.id, paper.version), paper.clone());
                let latest = found
                    .get(&paper.id)
                    .is_none_or(|p| p.version < paper.version);
                if latest {
                    found.insert(paper.id.clone(), paper);
                }
            }
            return Ok(());
        }
        Ok(())
    }

    /// Runs a search, paging at ≤100 results; returns the total match count and up to `max` papers.
    pub fn search(&self, query: &str, sort: Sort, max: usize) -> Result<(u64, Vec<Paper>), String> {
        let mut papers = Vec::new();
        let mut total = None;
        let mut empty_pages = 0;
        while papers.len() < max {
            let page = (max - papers.len()).min(MAX_PER_REQUEST);
            let url = search_url(self.endpoint, query, sort, papers.len(), page);
            let reply = self.get(&url, ATTEMPTS, false)?;
            let feed = parse_feed(&reply.body)?;
            if reply.status == 400 || !feed.errors.is_empty() {
                return Err(describe_errors(&feed, &url));
            }
            let reported = feed.total.unwrap_or(0);
            total = Some(total.unwrap_or(reported));
            let got = feed.papers.len();
            papers.extend(feed.papers);
            if (papers.len() as u64) >= reported {
                break;
            }
            if got == 0 {
                // The API occasionally returns an empty page mid-result; retry it a couple of times.
                empty_pages += 1;
                if empty_pages > 2 {
                    eprintln!(
                        "papers: repeated empty pages at offset {}; stopping early",
                        papers.len()
                    );
                    break;
                }
                eprintln!("papers: empty page at offset {}; retrying", papers.len());
            }
        }
        Ok((total.unwrap_or(0), papers))
    }
}

/// One version in arXiv's version history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub version: u32,
    /// Submission date, `YYYY-MM-DD` (UTC).
    pub date: String,
    /// arXiv withdrew this version: its source flag is `I` or it has no content.
    pub withdrawn: bool,
}

/// The version history of a versionless arXiv id; `Ok(None)` when arXiv has no
/// such record.
pub fn versions(http: &Http, endpoint: &str, id: &str) -> Result<Option<Vec<Version>>, String> {
    let url = format!(
        "{endpoint}?verb=GetRecord&identifier=oai:arXiv.org:{}&metadataPrefix=arXivRaw",
        encode(id)
    );
    let reply = http.get(&Get::api(&url, &[200]))?;
    parse_versions(&reply.text()).map_err(|e| format!("{e} ({url})"))
}

/// Parses an OAI-PMH `GetRecord` reply in the `arXivRaw` format.
pub fn parse_versions(xml: &str) -> Result<Option<Vec<Version>>, String> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| format!("invalid OAI-PMH XML from arXiv: {e}"))?;
    let root = doc.root_element();
    if !root.has_tag_name((OAI, "OAI-PMH")) {
        return Err("arXiv OAI-PMH response is not an OAI-PMH document".to_owned());
    }
    if let Some(error) = root.children().find(|n| n.has_tag_name((OAI, "error"))) {
        let code = error.attribute("code").unwrap_or_default();
        if code == "idDoesNotExist" {
            return Ok(None);
        }
        return Err(format!(
            "arXiv OAI-PMH error {code}: {}",
            error.text().unwrap_or_default().trim()
        ));
    }
    let raw = root
        .descendants()
        .find(|n| n.has_tag_name((ARXIV_RAW, "arXivRaw")))
        .ok_or("arXiv OAI-PMH reply has no arXivRaw record")?;
    let child = |n: roxmltree::Node, name: &str| {
        n.children()
            .find(|c| c.has_tag_name((ARXIV_RAW, name)))
            .and_then(|c| c.text())
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    raw.children()
        .filter(|n| n.has_tag_name((ARXIV_RAW, "version")))
        .map(|v| {
            let label = v.attribute("version").unwrap_or_default();
            let version = label
                .strip_prefix('v')
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("arXivRaw version {label:?} is not vN"))?;
            let date_text = child(v, "date");
            let date = jiff::fmt::rfc2822::parse(&date_text)
                .map_err(|e| format!("arXivRaw {label} date {date_text:?}: {e}"))?
                .with_time_zone(jiff::tz::TimeZone::UTC)
                .date()
                .to_string();
            Ok(Version {
                version,
                date,
                withdrawn: child(v, "source_type").contains('I') || child(v, "size") == "0kb",
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Some)
}

fn describe_errors(feed: &Feed, url: &str) -> String {
    if feed.errors.is_empty() {
        format!("HTTP 400 from {url}")
    } else {
        format!("arXiv error: {} ({url})", feed.errors.join("; "))
    }
}

/// Percent-encodes a query-string value, keeping characters arXiv uses literally.
fn encode(s: &str) -> String {
    crate::http::encode(s, b":,/")
}

fn id_list_url(endpoint: &str, ids: &[&ArxivId]) -> String {
    let list: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    format!(
        "{endpoint}?id_list={}&max_results={}",
        encode(&list.join(",")),
        ids.len()
    )
}

fn search_url(endpoint: &str, query: &str, sort: Sort, start: usize, max: usize) -> String {
    format!(
        "{endpoint}?search_query={}&start={start}&max_results={max}&sortBy={}&sortOrder=descending",
        encode(query),
        sort.api_name()
    )
}

/// True when the query already uses arXiv field prefixes or boolean operators.
fn is_raw(query: &str) -> bool {
    const FIELDS: [&str; 5] = ["ti:", "abs:", "au:", "cat:", "all:"];
    query
        .split(|c: char| c.is_whitespace() || c == '(')
        .any(|token| {
            matches!(token, "AND" | "OR" | "ANDNOT") || FIELDS.iter().any(|f| token.starts_with(f))
        })
}

/// Builds the `search_query` value: raw syntax passes through, plain words become
/// `all:<word>` joined by AND; categories are OR-ed and a date floor is AND-ed on.
pub fn build_query(query: &str, categories: &[String], since: Option<Date>) -> String {
    let base = if is_raw(query) {
        query.trim().to_owned()
    } else {
        query
            .split_whitespace()
            .map(|w| format!("all:{w}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let mut parts = Vec::new();
    if !base.is_empty() {
        parts.push(base);
    }
    if !categories.is_empty() {
        let cats: Vec<String> = categories.iter().map(|c| format!("cat:{c}")).collect();
        parts.push(cats.join(" OR "));
    }
    if let Some(d) = since {
        parts.push(format!(
            "submittedDate:[{}0000 TO 999912312359]",
            d.strftime("%Y%m%d")
        ));
    }
    if parts.len() == 1 {
        parts.pop().unwrap_or_default()
    } else {
        parts
            .iter()
            .map(|p| format!("({p})"))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oai_versions_mark_withdrawals() {
        let xml = include_str!("../tests/fixtures/arxiv-oai-withdrawn.xml");
        let versions = parse_versions(xml).unwrap().unwrap();
        assert_eq!(
            versions,
            [
                Version {
                    version: 1,
                    date: "2026-01-06".into(),
                    withdrawn: false
                },
                Version {
                    version: 2,
                    date: "2026-02-12".into(),
                    withdrawn: true
                },
            ]
        );
        let missing = include_str!("../tests/fixtures/arxiv-oai-error.xml");
        assert_eq!(parse_versions(missing).unwrap(), None);
        let bad = missing.replace("idDoesNotExist", "badArgument");
        assert!(parse_versions(&bad).unwrap_err().contains("badArgument"));
        assert!(parse_versions("<feed/>").is_err());
    }

    #[test]
    fn failing_requests_are_not_split_and_stop_the_lookup() {
        let busy = || crate::http::response("503 Service Unavailable", &["Retry-After: 0"], "");
        // Exactly one request per attempt; a split would send more.
        let (base, server) = crate::http::serve((0..ATTEMPTS).map(|_| busy()).collect());
        let http = Http::new();
        let endpoint = format!("{base}/api/query");
        let client = Client {
            http: &http,
            endpoint: &endpoint,
        };
        let ids: Vec<ArxivId> = ["2601.00001", "2601.00002", "hep-th/9901999v1"]
            .iter()
            .map(|id| ArxivId::parse(id).unwrap())
            .collect();
        let results = client.fetch(&ids);
        let heads = server.join().unwrap();
        assert_eq!(heads.len(), ATTEMPTS as usize);
        assert!(
            heads[0].contains("id_list=2601.00001,2601.00002"),
            "{}",
            heads[0]
        );
        for r in &results[..2] {
            assert!(r.as_ref().unwrap_err().starts_with("HTTP 503"), "{r:?}");
        }
        let skipped = results[2].as_ref().unwrap_err();
        assert!(skipped.starts_with("not attempted"), "{skipped}");
    }

    #[test]
    fn plain_words_become_all_fields() {
        assert_eq!(
            build_query("funding rate  perpetual", &[], None),
            "all:funding AND all:rate AND all:perpetual"
        );
    }

    #[test]
    fn raw_syntax_passes_through() {
        for raw in [
            "ti:\"loss-versus-rebalancing\"",
            "au:Ellingwood",
            "abs:arbitrage ANDNOT cat:cs.CR",
            "funding OR basis",
            "(cat:q-fin.TR)",
        ] {
            assert_eq!(build_query(raw, &[], None), raw);
        }
        // Lowercase "and" and words merely containing a prefix are plain words.
        assert_eq!(
            build_query("supply and demand", &[], None),
            "all:supply AND all:and AND all:demand"
        );
        assert_eq!(build_query("stati:c", &[], None), "all:stati:c");
    }

    #[test]
    fn categories_and_since_are_anded() {
        let cats = ["q-fin.TR".to_owned(), "q-fin.MF".to_owned()];
        let since = Date::new(2026, 1, 1).ok();
        assert_eq!(
            build_query("funding rate", &cats, since),
            "(all:funding AND all:rate) AND (cat:q-fin.TR OR cat:q-fin.MF) AND \
             (submittedDate:[202601010000 TO 999912312359])"
        );
        assert_eq!(build_query("", &cats[..1], None), "cat:q-fin.TR");
        assert_eq!(
            build_query("ti:lvr", &[], since),
            "(ti:lvr) AND (submittedDate:[202601010000 TO 999912312359])"
        );
    }

    #[test]
    fn urls_are_encoded() {
        assert_eq!(
            search_url(
                ENDPOINT,
                "ti:\"a b\" AND cat:q-fin.TR",
                Sort::Relevance,
                100,
                50
            ),
            "https://export.arxiv.org/api/query?search_query=ti:%22a%20b%22%20AND%20cat:q-fin.TR\
             &start=100&max_results=50&sortBy=relevance&sortOrder=descending"
        );
        let a = ArxivId::parse("2208.06046v4").unwrap();
        let b = ArxivId::parse("hep-th/9901001").unwrap();
        assert_eq!(
            id_list_url(ENDPOINT, &[&a, &b]),
            "https://export.arxiv.org/api/query?id_list=2208.06046v4,hep-th/9901001&max_results=2"
        );
    }
}
