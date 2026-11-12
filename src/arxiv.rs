//! arXiv API client: batched lookups, search paging, and query building.
//!
//! When the export API rate-limits papers, id lookups fall back to arXiv's
//! OAI-PMH interface, one record per request, so a run can still finish.

use std::collections::HashMap;

use jiff::civil::Date;

use crate::atom::{Feed, Paper, parse_feed};
use crate::http::{ATTEMPTS, Get, Http, is_rate_limited};
use crate::id::ArxivId;
use crate::text::squash;

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
    /// OAI-PMH endpoint for lookups while the export API rate-limits papers.
    oai: &'a str,
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
            oai: OAI_ENDPOINT,
        }
    }

    /// This client with another OAI-PMH endpoint for its fallback.
    pub fn with_oai(self, oai: &'a str) -> Self {
        Client { oai, ..self }
    }

    /// A client for a test server's endpoint; it serves the fallback too.
    #[cfg(test)]
    pub(crate) fn at(http: &'a Http, endpoint: &'a str) -> Self {
        Client {
            http,
            endpoint,
            oai: endpoint,
        }
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
                if is_rate_limited(&message) {
                    let rest: Vec<&ArxivId> =
                        batch.iter().chain(batches.flatten()).copied().collect();
                    self.fetch_oai(&rest, &message, &mut found, &mut failed);
                    break;
                }
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

    /// Looks up `ids` one by one through OAI-PMH after the export API
    /// rate-limited papers with `cause`. Gives the latest version only, so a
    /// pinned older version stays failed. Stops at the first failed request.
    fn fetch_oai(
        &self,
        ids: &[&ArxivId],
        cause: &str,
        found: &mut HashMap<String, Paper>,
        failed: &mut HashMap<String, String>,
    ) {
        eprintln!(
            "papers: arXiv's export API is rate-limiting papers; looking up {} id(s) \
             through OAI-PMH instead, one request each (about {}s)",
            ids.len(),
            ids.len() * 3
        );
        // A versioned and a versionless id share one record.
        let mut records: HashMap<&str, Option<Paper>> = HashMap::new();
        for (i, id) in ids.iter().enumerate() {
            let fetched = match records.get(id.id.as_str()) {
                Some(paper) => Ok(paper.clone()),
                None => record(self.http, self.oai, &id.id).map(|r| r.map(|r| r.paper)),
            };
            if let Ok(paper) = &fetched {
                records.insert(&id.id, paper.clone());
            }
            let paper = match fetched {
                Ok(Some(paper)) => paper,
                Ok(None) => {
                    failed.insert(
                        id.to_string(),
                        "not found: arXiv OAI-PMH has no record for this id".to_owned(),
                    );
                    continue;
                }
                Err(e) => {
                    let message = format!("{cause}; OAI-PMH fallback failed too: {e}");
                    for id in &ids[i..] {
                        failed.insert(id.to_string(), message.clone());
                    }
                    return;
                }
            };
            if id.version.is_some_and(|v| v != paper.version) {
                failed.insert(
                    id.to_string(),
                    format!(
                        "{cause}; the OAI-PMH fallback gives only the latest version, v{}",
                        paper.version
                    ),
                );
                continue;
            }
            failed.remove(&id.to_string());
            found.insert(format!("{}v{}", paper.id, paper.version), paper.clone());
            found.insert(paper.id.clone(), paper);
        }
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

/// A versionless arXiv id's `arXivRaw` record: its version history and its
/// latest version's metadata.
#[derive(Debug, Clone)]
struct Record {
    versions: Vec<Version>,
    /// The latest version, as the Atom API would give it.
    paper: Paper,
}

/// The version history of a versionless arXiv id; `Ok(None)` when arXiv has no
/// such record.
pub fn versions(http: &Http, endpoint: &str, id: &str) -> Result<Option<Vec<Version>>, String> {
    Ok(record(http, endpoint, id)?.map(|r| r.versions))
}

/// The `arXivRaw` record of a versionless arXiv id; `Ok(None)` when arXiv has
/// no such record. [`versions`] asks for the same URL, so it is cached.
fn record(http: &Http, endpoint: &str, id: &str) -> Result<Option<Record>, String> {
    let url = format!(
        "{endpoint}?verb=GetRecord&identifier=oai:arXiv.org:{}&metadataPrefix=arXivRaw",
        encode(id)
    );
    let reply = http.get(&Get::api(&url, &[200]))?;
    parse_record(&reply.text()).map_err(|e| format!("{e} ({url})"))
}

/// Parses an OAI-PMH `GetRecord` reply in the `arXivRaw` format.
#[cfg(test)]
pub fn parse_versions(xml: &str) -> Result<Option<Vec<Version>>, String> {
    Ok(parse_record(xml)?.map(|r| r.versions))
}

/// Parses an OAI-PMH `GetRecord` reply in the `arXivRaw` format.
fn parse_record(xml: &str) -> Result<Option<Record>, String> {
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
    let versions = raw
        .children()
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
        .collect::<Result<Vec<_>, String>>()?;
    let (Some(first), Some(latest)) = (versions.first(), versions.last()) else {
        return Err("arXivRaw record has no versions".to_owned());
    };
    let id = ArxivId::parse(&child(raw, "id"))
        .map_err(|e| format!("arXivRaw id {:?}: {e}", child(raw, "id")))?;
    let optional = |name: &str| Some(squash(&child(raw, name))).filter(|t| !t.is_empty());
    let categories: Vec<String> = child(raw, "categories")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let paper = Paper {
        version: latest.version,
        title: squash(&child(raw, "title")),
        authors: parse_authors(&child(raw, "authors")),
        published: first.date.clone(),
        updated: latest.date.clone(),
        primary_category: categories.first().cloned().unwrap_or_default(),
        categories,
        journal_ref: optional("journal-ref"),
        doi: optional("doi"),
        comment: optional("comments"),
        abstract_text: squash(&child(raw, "abstract")),
        abs_url: id.abs_url(),
        id: id.id,
    };
    Ok(Some(Record { versions, paper }))
}

/// Splits arXivRaw's author string ("A. Lee, B. Kim and C. M\\\"uller (MIT)")
/// into names: affiliations in parentheses are dropped and TeX accents decoded.
fn parse_authors(text: &str) -> Vec<String> {
    let mut plain = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let plain = squash(&decode_tex(&plain));
    plain
        .split(',')
        .flat_map(|part| part.split(" and "))
        .map(|name| name.trim().trim_start_matches("and ").trim())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Decodes the TeX accents and letters arXiv metadata uses into Unicode (NFC),
/// then drops leftover braces: `M\\\"{u}ller` → `Müller`.
fn decode_tex(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\\' {
            if chars[i] != '{' && chars[i] != '}' {
                out.push(chars[i]);
            }
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied().unwrap_or_default();
        let mark = match next {
            '"' => Some('\u{308}'),
            '\'' => Some('\u{301}'),
            '`' => Some('\u{300}'),
            '^' => Some('\u{302}'),
            '~' => Some('\u{303}'),
            '=' => Some('\u{304}'),
            '.' => Some('\u{307}'),
            'c' => Some('\u{327}'),
            'v' => Some('\u{30C}'),
            'u' => Some('\u{306}'),
            'H' => Some('\u{30B}'),
            'r' => Some('\u{30A}'),
            'k' => Some('\u{328}'),
            _ => None,
        };
        // A letter accent (`\c`, `\v`, …) needs a brace or space after it, else
        // it is the start of a command such as `\v` in `\vartheta`.
        let letter_accent = next.is_ascii_alphabetic();
        let after = chars.get(i + 2).copied().unwrap_or_default();
        if let Some(mark) = mark
            && (!letter_accent || after == '{' || after == ' ')
        {
            // The accented letter: the next non-brace, non-space character.
            let mut j = i + 2;
            while j < chars.len() && (chars[j] == '{' || chars[j] == ' ') {
                j += 1;
            }
            if let Some(&base) = chars.get(j) {
                let base = match base {
                    _ if base == '\\' && chars.get(j + 1) == Some(&'i') => {
                        j += 1;
                        'i'
                    }
                    other => other,
                };
                out.push(base);
                out.push(mark);
                i = j + 1;
                continue;
            }
        }
        // Letters written as commands: `\ss`, `\o`, `\aa`, `\l`, `\i`, ….
        let word: String = chars[i + 1..]
            .iter()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        let letter = match word.as_str() {
            "ss" => Some("ß"),
            "o" => Some("ø"),
            "O" => Some("Ø"),
            "aa" => Some("å"),
            "AA" => Some("Å"),
            "ae" => Some("æ"),
            "AE" => Some("Æ"),
            "oe" => Some("œ"),
            "OE" => Some("Œ"),
            "l" => Some("ł"),
            "L" => Some("Ł"),
            "i" => Some("ı"),
            _ => None,
        };
        match letter {
            Some(letter) => {
                out += letter;
                i += 1 + word.len();
                // The space that ends a control word belongs to it.
                if chars.get(i) == Some(&' ') && chars.get(i + 1).is_some_and(|c| *c == '}') {
                    i += 1;
                }
            }
            None => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out.nfc().collect()
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
    fn oai_records_give_the_latest_version() {
        let xml = include_str!("../tests/fixtures/arxiv-oai-withdrawn.xml");
        let paper = parse_record(xml).unwrap().unwrap().paper;
        assert_eq!(
            (paper.id.as_str(), paper.version, paper.published.as_str()),
            ("2601.99977", 2, "2026-01-06")
        );
        assert_eq!(paper.updated, "2026-02-12");
        assert_eq!(paper.title, "Synthetic Widgets Rewrite Their Own Queries");
        assert_eq!(paper.authors, ["Ena Morrow", "Fitz Calder"]);
        assert_eq!(
            (paper.primary_category.as_str(), paper.categories.len()),
            ("cs.IR", 1)
        );
        assert_eq!(
            paper.comment.as_deref(),
            Some("The paper is about to undergo significant revisions")
        );
        assert_eq!(paper.abs_url, "https://arxiv.org/abs/2601.99977");
        assert_eq!(paper.doi, None);
    }

    #[test]
    fn oai_author_strings_become_names() {
        assert_eq!(
            parse_authors(
                r#"J. M\"uller, A. Sch{\"o}n (Inst. of X (Y)), B. Ko\c{c} and C. {\AA}berg,
                 and D. Garc\'{\i}a, E. Wa\l{}\k{e}sa, F. \v Simek, G. \vartheta"#
            ),
            [
                "J. Müller",
                "A. Schön",
                "B. Koç",
                "C. Åberg",
                "D. García",
                "E. Wałęsa",
                "F. Šimek",
                "G. \\vartheta"
            ]
        );
    }

    #[test]
    fn rate_limited_lookups_fall_back_to_oai() {
        use crate::http::{response, serve};
        let xml = [("Content-Type", "text/xml")].map(|(k, v)| format!("{k}: {v}"));
        let xml: Vec<&str> = xml.iter().map(String::as_str).collect();
        let (api, api_server) = serve(vec![response(
            "429 Too Many Requests",
            &["Retry-After: 120"],
            "Rate exceeded.",
        )]);
        let (oai, oai_server) = serve(vec![
            response(
                "200 OK",
                &xml,
                include_str!("../tests/fixtures/arxiv-oai-withdrawn.xml"),
            ),
            response(
                "200 OK",
                &xml,
                include_str!("../tests/fixtures/arxiv-oai-error.xml"),
            ),
        ]);
        let http = Http::new();
        let (endpoint, oai) = (format!("{api}/api/query"), format!("{oai}/oai"));
        let client = Client::at(&http, &endpoint).with_oai(&oai);
        let ids: Vec<ArxivId> = ["2601.99977", "2601.99977v1", "2601.99999"]
            .iter()
            .map(|id| ArxivId::parse(id).unwrap())
            .collect();
        let results = client.fetch(&ids);
        assert_eq!(api_server.join().unwrap().len(), 1);
        let heads = oai_server.join().unwrap();
        assert!(
            heads[0].contains("identifier=oai:arXiv.org:2601.99977&metadataPrefix=arXivRaw"),
            "{}",
            heads[0]
        );
        let paper = results[0].as_ref().unwrap();
        assert_eq!((paper.version, paper.updated.as_str()), (2, "2026-02-12"));
        let pinned = results[1].as_ref().unwrap_err();
        assert!(pinned.contains("only the latest version, v2"), "{pinned}");
        assert!(pinned.contains("is rate-limiting papers"), "{pinned}");
        let missing = results[2].as_ref().unwrap_err();
        assert!(missing.starts_with("not found"), "{missing}");
    }

    #[test]
    fn failing_requests_are_not_split_and_stop_the_lookup() {
        let busy = || crate::http::response("503 Service Unavailable", &["Retry-After: 0"], "");
        // Exactly one request per attempt; a split would send more.
        let (base, server) = crate::http::serve((0..ATTEMPTS).map(|_| busy()).collect());
        let http = Http::new();
        let endpoint = format!("{base}/api/query");
        let client = Client::at(&http, &endpoint);
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
