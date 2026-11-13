//! Search sources beyond arXiv: OpenAlex, DBLP, Semantic Scholar, and CiNii
//! Research, mapped to one result shape. Requests go through [`Http`], which
//! spaces them per host; searches are never cached.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{Get, Http, encode};
use crate::openalex;
use crate::refs::Doi;
use crate::searchlog::Found;
use crate::text::{squash, strip_markup};

pub const S2_KEY_VAR: &str = "S2_API_KEY";
pub const CINII_APPID_VAR: &str = "CINII_APPID";
const S2_HOST: &str = "api.semanticscholar.org";
const S2_SEARCH: &str = "https://api.semanticscholar.org/graph/v1/paper/search";
pub const S2_FIELDS: &str = "title,year,authors,venue,externalIds,url,publicationDate,abstract";
const DBLP_SEARCH: &str = "https://dblp.org/search/publ/api";
const DBLP_SPARQL: &str = "https://sparql.dblp.org/sparql";
const DBLP_SPARQL_HOST: &str = "sparql.dblp.org";
const DBLP_REC: &str = "https://dblp.org/rec/";
const CINII_SEARCH: &str = "https://cir.nii.ac.jp/opensearch/articles";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Source {
    Arxiv,
    Openalex,
    Dblp,
    S2,
    Cinii,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Arxiv => "arxiv",
            Source::Openalex => "openalex",
            Source::Dblp => "dblp",
            Source::S2 => "s2",
            Source::Cinii => "cinii",
        }
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    /// Source-prefixed id, e.g. `openalex:W1`, `dblp:conf/x/Y26`, `s2:<sha>`.
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    /// Versionless arXiv id.
    pub arxiv: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "abstract")]
    pub abstract_text: Option<String>,
}

impl Hit {
    pub fn found(&self) -> Found {
        Found {
            id: self.id.clone(),
            title: Some(self.title.clone()),
            year: self.year,
            doi: self.doi.clone(),
            arxiv: self.arxiv.clone(),
            openalex: self.id.strip_prefix("openalex:").map(str::to_owned),
            url: self.url.clone(),
        }
    }
}

/// Total matches as the source reports them (if it does) and the hits.
pub type Page = (Option<u64>, Vec<Hit>);

fn year_of(date: Option<&str>) -> Option<i64> {
    date?.get(..4)?.parse().ok()
}

fn doi_text(s: &str) -> Option<String> {
    Doi::parse(s).map(|d| d.as_str().to_owned())
}

/// The versionless id in a `10.48550/arXiv.<id>` DOI.
fn arxiv_from_doi(doi: &str) -> Option<String> {
    let rest = doi.strip_prefix("10.48550/arxiv.")?;
    crate::id::ArxivId::parse(rest).ok().map(|a| a.id)
}

impl From<openalex::Work> for Hit {
    fn from(w: openalex::Work) -> Self {
        let doi = w.doi.as_ref().map(|d| d.as_str().to_owned());
        Hit {
            id: format!("openalex:{}", w.id),
            arxiv: doi.as_deref().and_then(arxiv_from_doi),
            title: w.title,
            authors: w.authors,
            year: w.publication_year,
            venue: w.venue,
            url: doi.as_ref().map(|d| format!("https://doi.org/{d}")),
            doi,
            abstract_text: w.abstract_text,
        }
    }
}

pub fn openalex(http: &Http, query: &str, since: Option<&str>, max: usize) -> Result<Page, String> {
    let (total, works) = openalex::Client::new(http).search(query, since, max)?;
    Ok((total, works.into_iter().map(Hit::from).collect()))
}

// --- DBLP -----------------------------------------------------------------

#[derive(Deserialize)]
struct DblpReply {
    result: DblpResult,
}

#[derive(Deserialize)]
struct DblpResult {
    hits: DblpHits,
}

#[derive(Deserialize)]
struct DblpHits {
    #[serde(rename = "@total")]
    total: Option<String>,
    #[serde(default)]
    hit: Vec<DblpHit>,
}

#[derive(Deserialize)]
struct DblpHit {
    info: DblpInfo,
}

#[derive(Deserialize)]
struct DblpInfo {
    key: String,
    title: Option<String>,
    authors: Option<DblpAuthors>,
    venue: Option<Value>,
    year: Option<String>,
    doi: Option<String>,
    ee: Option<Value>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct DblpAuthors {
    author: Value,
}

/// A JSON string or the `text` of an object, or each of an array of those.
fn texts(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Object(o) => o
            .get("text")
            .and_then(Value::as_str)
            .map(|s| vec![s.to_owned()])
            .unwrap_or_default(),
        Value::Array(a) => a.iter().flat_map(texts).collect(),
        _ => Vec::new(),
    }
}

/// DBLP disambiguates homonyms with a four-digit suffix ("Jane Doe 0002").
fn dblp_name(name: &str) -> String {
    match name.rsplit_once(' ') {
        Some((base, n)) if n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit()) => base.to_owned(),
        _ => name.to_owned(),
    }
}

pub fn parse_dblp(json: &str) -> Result<Page, String> {
    let reply: DblpReply =
        serde_json::from_str(json).map_err(|e| format!("invalid DBLP JSON: {e}"))?;
    let hits = reply.result.hits;
    let total = hits.total.and_then(|t| t.parse().ok());
    let out = hits
        .hit
        .into_iter()
        .map(|h| {
            let i = h.info;
            let ee = i.ee.as_ref().map(texts).unwrap_or_default();
            let arxiv = ee
                .iter()
                .find_map(|u| crate::id::ArxivId::from_abs_url(u))
                .map(|a| a.id);
            Hit {
                id: format!("dblp:{}", i.key),
                title: squash(i.title.as_deref().unwrap_or_default().trim_end_matches('.')),
                authors: i
                    .authors
                    .map(|a| texts(&a.author).iter().map(|n| dblp_name(n)).collect())
                    .unwrap_or_default(),
                year: i.year.and_then(|y| y.parse().ok()),
                venue: i.venue.as_ref().map(texts).map(|v| v.join(", ")),
                doi: i.doi.as_deref().and_then(doi_text),
                arxiv,
                url: ee.into_iter().next().or(i.url),
                abstract_text: None,
            }
        })
        .collect();
    Ok((total, out))
}

/// Which DBLP service answered a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DblpVia {
    /// The search API (`dblp.org/search/publ/api`), ranked by relevance.
    Api,
    /// The SPARQL endpoint, after the search API answered with a bot check.
    Sparql,
}

impl DblpVia {
    /// What the search log and the result line say about a fallback search.
    pub const SPARQL_NOTE: &str = "the dblp.org search API answered with a bot check, \
         so the search ran on sparql.dblp.org: every word must be in the title, \
         newest first, no author lists or total";
}

pub fn dblp(http: &Http, query: &str, max: usize) -> Result<(Page, DblpVia), String> {
    dblp_at(http, DBLP_SEARCH, DBLP_SPARQL, query, max)
}

/// The DBLP search with the services at `api` and `sparql`: tests pass a local server.
fn dblp_at(
    http: &Http,
    api: &str,
    sparql: &str,
    query: &str,
    max: usize,
) -> Result<(Page, DblpVia), String> {
    let url = format!("{api}?q={}&format=json&h={max}", encode(query.trim(), b""));
    let reply = http.get(&Get {
        cache: false,
        ..Get::api(&url, &[200, 202])
    })?;
    if reply.status != 202 && !reply.content_type.contains("html") {
        return Ok((parse_dblp(&reply.text())?, DblpVia::Api));
    }
    let challenge = format!(
        "dblp.org answered with an HTML page (HTTP {}), likely a bot-check challenge",
        reply.status
    );
    dblp_sparql(http, sparql, query, max)
        .map(|page| (page, DblpVia::Sparql))
        .map_err(|e| {
            format!(
                "{challenge}; papers does not solve challenges. The fallback title query to \
                 {DBLP_SPARQL_HOST} failed too: {e}. Try again later or use another --source"
            )
        })
}

/// `text` as the body of a double-quoted SPARQL string literal.
fn sparql_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

/// The title filter of [`dblp_sparql_query`]. Each whitespace-separated word,
/// lowercased, must be a substring of the lowercased title. Of dblp's search
/// syntax, `a|b` matches either part; quotes and a trailing `$` (exact word)
/// are dropped, so a phrase becomes its words and an exact word a substring.
fn dblp_title_filter(query: &str) -> Result<String, String> {
    let groups: Vec<String> = query
        .split_whitespace()
        .filter_map(|word| {
            let parts: Vec<String> = word
                .split('|')
                .map(|p| {
                    p.trim_matches('"')
                        .trim_end_matches('$')
                        .trim_end_matches('"')
                })
                .filter(|p| !p.is_empty())
                .map(|p| format!("CONTAINS(?lt, \"{}\")", sparql_literal(&p.to_lowercase())))
                .collect();
            match parts.len() {
                0 => None,
                1 => parts.into_iter().next(),
                _ => Some(format!("({})", parts.join(" || "))),
            }
        })
        .collect();
    if groups.is_empty() {
        return Err(format!("{query:?} has no title words for the SPARQL query"));
    }
    Ok(groups.join(" && "))
}

/// A title search over DBLP's RDF with [`dblp_title_filter`]. Newest first, at
/// most `max` rows. Author lists are left out: ordered authors need the
/// signature join, which made the endpoint time out on a test query.
pub fn dblp_sparql_query(query: &str, max: usize) -> Result<String, String> {
    let filter = dblp_title_filter(query)?;
    Ok(format!(
        "PREFIX dblp: <https://dblp.org/rdf/schema#>\n\
         SELECT ?p (SAMPLE(?t0) AS ?t) (SAMPLE(?y0) AS ?y) (SAMPLE(?v0) AS ?v) (SAMPLE(?d0) AS ?d) WHERE {{\n\
         \x20 ?p dblp:title ?t0 . BIND(LCASE(STR(?t0)) AS ?lt)\n\
         \x20 FILTER({filter})\n\
         \x20 OPTIONAL {{ ?p dblp:yearOfPublication ?y0 }}\n\
         \x20 OPTIONAL {{ ?p dblp:publishedIn ?v0 }}\n\
         \x20 OPTIONAL {{ ?p dblp:doi ?d0 }}\n\
         }} GROUP BY ?p ORDER BY DESC(?y) ?p LIMIT {max}\n"
    ))
}

/// One SPARQL request: no retry, so a bot check or overload is not pressed.
fn dblp_sparql(http: &Http, endpoint: &str, query: &str, max: usize) -> Result<Page, String> {
    let encoded = encode(&dblp_sparql_query(query, max)?, b"");
    let url = format!("{endpoint}?query={encoded}");
    let host = crate::http::host(&url)?;
    let headers = [("accept", "application/sparql-results+json".to_owned())];
    let reply = http.get(&Get {
        attempts: 1,
        retry_429: false,
        headers: &headers,
        headers_host: &host,
        cache: false,
        // QLever reports a query timeout as HTTP 429 with a JSON `exception`.
        ..Get::api(&url, &[200, 400, 429])
    });
    // The URL holds the whole query; errors name the endpoint instead, and a
    // redirected URL that still carries the query shows `<query>`.
    let reply = reply.map_err(|e| e.replace(&url, endpoint).replace(&encoded, "<query>"))?;
    let text = reply.text();
    if reply.status != 200 {
        let reason = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("exception")?.as_str().map(squash))
            .unwrap_or_else(|| "no reason given".to_owned());
        return Err(format!("HTTP {} from {host}: {reason}", reply.status));
    }
    if !reply.content_type.contains("json") {
        return Err(format!(
            "{host} answered with {:?} (HTTP 200), not SPARQL results; likely a bot-check \
             challenge too",
            reply.content_type
        ));
    }
    parse_dblp_sparql(&text)
}

#[derive(Deserialize)]
struct SparqlReply {
    results: SparqlResults,
}

#[derive(Deserialize)]
struct SparqlResults {
    bindings: Vec<std::collections::HashMap<String, SparqlTerm>>,
}

#[derive(Deserialize)]
struct SparqlTerm {
    value: String,
}

/// Hits from a `sparql-results+json` reply to [`dblp_sparql_query`]. The ids
/// are the `dblp:<key>` of the API path; SPARQL gives no total.
pub fn parse_dblp_sparql(json: &str) -> Result<Page, String> {
    let reply: SparqlReply =
        serde_json::from_str(json).map_err(|e| format!("invalid DBLP SPARQL JSON: {e}"))?;
    let hits = reply
        .results
        .bindings
        .into_iter()
        .map(|row| {
            let get = |k: &str| row.get(k).map(|t| t.value.as_str());
            let record = get("p").ok_or("a DBLP SPARQL row has no record")?;
            let key = record
                .strip_prefix(DBLP_REC)
                .ok_or_else(|| format!("DBLP SPARQL record {record:?} is not under {DBLP_REC}"))?;
            let doi = get("d").and_then(doi_text);
            Ok(Hit {
                id: format!("dblp:{key}"),
                title: squash(get("t").unwrap_or_default().trim_end_matches('.')),
                authors: Vec::new(),
                year: get("y").and_then(|y| y.parse().ok()),
                venue: get("v").map(squash).filter(|v| !v.is_empty()),
                arxiv: doi.as_deref().and_then(arxiv_from_doi),
                url: Some(
                    doi.as_ref()
                        .map_or_else(|| record.to_owned(), |d| format!("https://doi.org/{d}")),
                ),
                doi,
                abstract_text: None,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok((None, hits))
}

// --- Semantic Scholar ------------------------------------------------------

#[derive(Deserialize)]
struct S2Page {
    total: Option<u64>,
    #[serde(default)]
    data: Vec<S2Paper>,
}

#[derive(Deserialize)]
struct S2Paper {
    #[serde(rename = "paperId")]
    paper_id: String,
    title: Option<String>,
    year: Option<i64>,
    #[serde(default)]
    authors: Vec<S2Author>,
    venue: Option<String>,
    #[serde(rename = "externalIds")]
    external_ids: Option<std::collections::BTreeMap<String, Value>>,
    url: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
}

#[derive(Deserialize)]
struct S2Author {
    name: Option<String>,
}

impl From<S2Paper> for Hit {
    fn from(p: S2Paper) -> Self {
        let ext = p.external_ids.unwrap_or_default();
        let get = |k: &str| ext.get(k).and_then(Value::as_str).map(str::to_owned);
        Hit {
            id: format!("s2:{}", p.paper_id),
            title: squash(p.title.as_deref().unwrap_or_default()),
            authors: p.authors.into_iter().filter_map(|a| a.name).collect(),
            year: p.year,
            venue: p.venue.filter(|v| !v.is_empty()),
            doi: get("DOI").as_deref().and_then(doi_text),
            arxiv: get("ArXiv"),
            url: p.url,
            abstract_text: p.abstract_text,
        }
    }
}

pub fn parse_s2(json: &str) -> Result<Page, String> {
    let page: S2Page =
        serde_json::from_str(json).map_err(|e| format!("invalid Semantic Scholar JSON: {e}"))?;
    Ok((page.total, page.data.into_iter().map(Hit::from).collect()))
}

/// The optional API key header, sent only to the Semantic Scholar API host.
pub fn s2_headers() -> Vec<(&'static str, String)> {
    std::env::var(S2_KEY_VAR)
        .ok()
        .filter(|k| !k.trim().is_empty())
        .map(|k| vec![("x-api-key", k.trim().to_owned())])
        .unwrap_or_default()
}

/// Relevance search, paging at 100; `since_year` filters on the reported year
/// after the fact (the year is all every result carries).
pub fn s2(http: &Http, query: &str, since_year: Option<i64>, max: usize) -> Result<Page, String> {
    let headers = s2_headers();
    let mut out = Vec::new();
    let mut total = None;
    let mut offset = 0;
    while out.len() < max {
        let limit = (max - offset).min(100);
        if limit == 0 {
            break;
        }
        let url = format!(
            "{S2_SEARCH}?query={}&offset={offset}&limit={limit}&fields={S2_FIELDS}",
            encode(query.trim(), b"")
        );
        let reply = http
            .get(&Get {
                headers: &headers,
                headers_host: S2_HOST,
                cache: false,
                ..Get::api(&url, &[200])
            })
            .map_err(|e| {
                if e.contains("HTTP 429") && headers.is_empty() {
                    format!(
                        "{e} (unauthenticated Semantic Scholar requests share one rate limit; \
                         set {S2_KEY_VAR} to a key from https://www.semanticscholar.org/product/api)"
                    )
                } else {
                    e
                }
            })?;
        let (t, hits) = parse_s2(&reply.text())?;
        total = total.or(t);
        let got = hits.len();
        out.extend(hits);
        offset += got;
        if got < limit {
            break;
        }
    }
    if let Some(y) = since_year {
        out.retain(|h| h.year.is_some_and(|hy| hy >= y));
    }
    out.truncate(max);
    Ok((total, out))
}

// --- CiNii Research ----------------------------------------------------------

#[derive(Deserialize)]
struct CiniiChannel {
    #[serde(rename = "opensearch:totalResults")]
    total: Option<u64>,
    #[serde(default)]
    items: Vec<CiniiItem>,
}

#[derive(Deserialize)]
struct CiniiItem {
    #[serde(rename = "@id")]
    id: String,
    title: Option<String>,
    #[serde(rename = "dc:creator", default)]
    creators: Vec<String>,
    #[serde(rename = "prism:publicationName")]
    publication: Option<String>,
    #[serde(rename = "prism:publicationDate")]
    date: Option<String>,
    #[serde(rename = "dc:identifier", default)]
    identifiers: Vec<CiniiIdentifier>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct CiniiIdentifier {
    #[serde(rename = "@type")]
    kind: Option<String>,
    #[serde(rename = "@value")]
    value: Option<String>,
}

/// CiNii writes names "Family, Given" (romanized) or as one string (Japanese).
fn cinii_name(name: &str) -> String {
    match name.split_once(", ") {
        Some((family, given)) if !given.contains(',') => format!("{given} {family}"),
        _ => squash(name),
    }
}

pub fn parse_cinii(json: &str) -> Result<Page, String> {
    let channel: CiniiChannel =
        serde_json::from_str(json).map_err(|e| format!("invalid CiNii JSON: {e}"))?;
    let hits = channel
        .items
        .into_iter()
        .map(|i| {
            let doi = i
                .identifiers
                .iter()
                .find(|x| x.kind.as_deref() == Some("cir:DOI"))
                .and_then(|x| x.value.as_deref())
                .and_then(doi_text);
            Hit {
                id: format!("cinii:{}", i.id.rsplit('/').next().unwrap_or(&i.id)),
                title: squash(i.title.as_deref().unwrap_or_default()),
                authors: i.creators.iter().map(|n| cinii_name(n)).collect(),
                year: year_of(i.date.as_deref()),
                venue: i.publication,
                arxiv: doi.as_deref().and_then(arxiv_from_doi),
                doi,
                url: Some(i.id),
                abstract_text: i.description.map(|d| strip_markup(&d)),
            }
        })
        .collect();
    Ok((channel.total, hits))
}

/// Article search. CiNii's Web API terms require a registered application id.
pub fn cinii(
    http: &Http,
    query: &str,
    since_year: Option<i64>,
    max: usize,
) -> Result<Page, String> {
    let appid = std::env::var(CINII_APPID_VAR)
        .ok()
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            format!(
                "CiNii Research requires an application id: register one at \
                 https://support.nii.ac.jp/en/cinii/api/developer and set {CINII_APPID_VAR}"
            )
        })?;
    let mut url = format!(
        "{CINII_SEARCH}?q={}&count={}&format=json&appid={}",
        encode(query.trim(), b""),
        max.min(200),
        encode(&appid, b"")
    );
    if let Some(y) = since_year {
        url += &format!("&from={y}");
    }
    // The appid travels in the URL; keep it out of error messages.
    let redact = |e: String| e.replace(&encode(&appid, b""), "<appid>");
    let reply = http
        .get(&Get {
            cache: false,
            ..Get::api(&url, &[200])
        })
        .map_err(redact)?;
    parse_cinii(&reply.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dblp_hits() {
        let json = r#"{"result":{"hits":{"@total":"2","@sent":"2","hit":[
          {"@score":"5","info":{"authors":{"author":[{"@pid":"1","text":"Ann Lee 0002"},{"@pid":"2","text":"Bo Chen"}]},
           "title":"Synthetic Widgets for Retrieval.","venue":"SYNTH","year":"2025","type":"Conference and Workshop Papers",
           "key":"conf/synth/LeeC25","doi":"10.1234/SYNTH.2025.1","ee":"https://doi.org/10.1234/synth.2025.1","url":"https://dblp.org/rec/conf/synth/LeeC25"}},
          {"@score":"4","info":{"authors":{"author":{"@pid":"3","text":"Cy Dee"}},"title":"Gadgets.","venue":["CoRR","abs/2601.00001"],
           "year":"2026","key":"journals/corr/abs-2601-00001","ee":["https://arxiv.org/abs/2601.00001"]}}]}}}"#;
        let (total, hits) = parse_dblp(json).unwrap();
        assert_eq!(total, Some(2));
        assert_eq!(hits[0].id, "dblp:conf/synth/LeeC25");
        assert_eq!(hits[0].title, "Synthetic Widgets for Retrieval");
        assert_eq!(hits[0].authors, ["Ann Lee", "Bo Chen"]);
        assert_eq!(hits[0].doi.as_deref(), Some("10.1234/synth.2025.1"));
        assert_eq!((hits[1].authors.len(), hits[1].year), (1, Some(2026)));
        assert_eq!(hits[1].venue.as_deref(), Some("CoRR, abs/2601.00001"));
        assert_eq!(hits[1].arxiv.as_deref(), Some("2601.00001"));
        assert!(parse_dblp("<html>").is_err());
    }

    #[test]
    fn dblp_sparql_query_matches_every_word_and_escapes_input() {
        let q = dblp_sparql_query("Foot\"note  Financial\\ x\")}", 7).unwrap();
        assert!(
            q.contains(
                r#"FILTER(CONTAINS(?lt, "foot\"note") && CONTAINS(?lt, "financial\\") && CONTAINS(?lt, "x\")}"))"#
            ),
            "{q}"
        );
        assert!(q.contains("BIND(LCASE(STR(?t0)) AS ?lt)"), "{q}");
        assert!(q.ends_with("LIMIT 7\n"), "{q}");
        // Every quote in the query is either a delimiter or escaped.
        let filter = q
            .split("FILTER(")
            .nth(1)
            .unwrap()
            .split('\n')
            .next()
            .unwrap();
        let bare = filter.replace("\\\\", "").replace("\\\"", "");
        assert_eq!(bare.matches('"').count(), 6, "{filter}");
        assert_eq!(sparql_literal("a\nb\rc"), "a\\nb\\rc");
    }

    #[test]
    fn dblp_sparql_filter_reads_dblp_or_exact_and_phrase_syntax() {
        assert_eq!(
            dblp_title_filter("Graph|network \"deep learning\" embed$ |").unwrap(),
            r#"(CONTAINS(?lt, "graph") || CONTAINS(?lt, "network")) && CONTAINS(?lt, "deep") && CONTAINS(?lt, "learning") && CONTAINS(?lt, "embed")"#
        );
        assert!(dblp_title_filter("\" $ |").is_err());
    }

    #[test]
    fn parses_dblp_sparql_rows_with_missing_fields() {
        let json = r#"{"head":{"vars":["p","t","y","v","d"]},"results":{"bindings":[
          {"p":{"type":"uri","value":"https://dblp.org/rec/journals/ngc/PanLZ25"},
           "t":{"type":"literal","value":"Synthetic  Footnotes."},
           "y":{"datatype":"http://www.w3.org/2001/XMLSchema#gYear","type":"literal","value":"2025"},
           "v":{"type":"literal","value":"New Gener. Comput."},
           "d":{"type":"uri","value":"https://doi.org/10.1007/S00354-025-00301-9"}},
          {"p":{"type":"uri","value":"https://dblp.org/rec/journals/corr/abs-2601-00001"},
           "t":{"type":"literal","value":"Gadgets"},
           "d":{"type":"uri","value":"https://doi.org/10.48550/arXiv.2601.00001"}},
          {"p":{"type":"uri","value":"https://dblp.org/rec/conf/x/Y26"},"t":{"type":"literal","value":"Bare"}}]}}"#;
        let (total, hits) = parse_dblp_sparql(json).unwrap();
        assert_eq!(total, None);
        assert_eq!(hits[0].id, "dblp:journals/ngc/PanLZ25");
        assert_eq!(hits[0].title, "Synthetic Footnotes");
        assert_eq!(hits[0].year, Some(2025));
        assert_eq!(hits[0].venue.as_deref(), Some("New Gener. Comput."));
        assert_eq!(hits[0].doi.as_deref(), Some("10.1007/s00354-025-00301-9"));
        assert_eq!(
            hits[0].url.as_deref(),
            Some("https://doi.org/10.1007/s00354-025-00301-9")
        );
        assert_eq!(hits[1].arxiv.as_deref(), Some("2601.00001"));
        assert_eq!((hits[1].year, hits[1].venue.as_deref()), (None, None));
        let bare = &hits[2];
        assert_eq!(
            (bare.year, bare.venue.as_deref(), bare.doi.as_deref()),
            (None, None, None)
        );
        assert_eq!(bare.url.as_deref(), Some("https://dblp.org/rec/conf/x/Y26"));
        assert!(bare.authors.is_empty());
        assert!(parse_dblp_sparql("<html>").is_err());
        let foreign = r#"{"results":{"bindings":[{"p":{"value":"https://example.org/x"}}]}}"#;
        assert!(parse_dblp_sparql(foreign).is_err());
    }

    #[test]
    fn dblp_falls_back_to_one_sparql_query_after_a_bot_check() {
        use crate::http::{response, serve};
        let challenge = response(
            "202 Accepted",
            &["Content-Type: text/html"],
            "<html>bot</html>",
        );
        let rows = r#"{"head":{"vars":["p","t"]},"results":{"bindings":[
          {"p":{"value":"https://dblp.org/rec/conf/synth/LeeC25"},"t":{"value":"Synthetic Widgets."},
           "y":{"value":"2025"}}]}}"#;
        let answer = response(
            "200 OK",
            &["Content-Type: application/sparql-results+json"],
            rows,
        );
        let (base, server) = serve(vec![challenge, answer]);
        let ((total, hits), via) = dblp_at(
            &Http::new(),
            &format!("{base}/search/publ/api"),
            &format!("{base}/sparql"),
            "synthetic widgets",
            5,
        )
        .unwrap();
        let heads = server.join().unwrap();
        assert_eq!(via, DblpVia::Sparql);
        assert_eq!(total, None);
        assert_eq!(hits[0].id, "dblp:conf/synth/LeeC25");
        assert_eq!(
            (hits[0].title.as_str(), hits[0].year),
            ("Synthetic Widgets", Some(2025))
        );
        assert_eq!(heads.len(), 2);
        assert!(
            heads[0].starts_with("GET /search/publ/api?q=synthetic%20widgets"),
            "{}",
            heads[0]
        );
        let sparql = heads[1].to_lowercase();
        assert!(sparql.starts_with("get /sparql?query=prefix"), "{sparql}");
        assert!(
            sparql.contains("contains%28%3flt%2c%20%22widgets%22%29"),
            "{sparql}"
        );
        assert!(
            sparql.contains("accept: application/sparql-results+json"),
            "{sparql}"
        );
        assert!(
            sparql.contains(&format!("user-agent: {}", crate::http::USER_AGENT)),
            "{sparql}"
        );
    }

    #[test]
    fn dblp_sparql_failure_names_both_attempts_and_is_not_retried() {
        use crate::http::{response, serve};
        let challenge = response(
            "200 OK",
            &["Content-Type: text/html; charset=utf-8"],
            "<html/>",
        );
        let timeout = response(
            "429 Too Many Requests",
            &["Content-Type: application/json"],
            r#"{"exception":"Operation timed out.","query":"PREFIX dblp: …"}"#,
        );
        let (base, server) = serve(vec![challenge, timeout]);
        let err = dblp_at(
            &Http::new(),
            &format!("{base}/search/publ/api"),
            &format!("{base}/sparql"),
            "widgets",
            5,
        )
        .unwrap_err();
        assert_eq!(server.join().unwrap().len(), 2);
        assert!(
            err.contains("HTML page (HTTP 200), likely a bot-check"),
            "{err}"
        );
        assert!(
            err.contains("fallback title query to sparql.dblp.org failed too"),
            "{err}"
        );
        assert!(
            err.contains("HTTP 429 from 127.0.0.1: Operation timed out."),
            "{err}"
        );
        assert!(!err.contains("PREFIX"), "{err}");
    }

    #[test]
    fn dblp_sparql_html_reply_is_reported_as_a_challenge() {
        use crate::http::{response, serve};
        let challenge = || response("202 Accepted", &["Content-Type: text/html"], "<html/>");
        let (base, server) = serve(vec![
            challenge(),
            response("200 OK", &["Content-Type: text/html"], "<html/>"),
        ]);
        let err = dblp_at(
            &Http::new(),
            &format!("{base}/search/publ/api"),
            &format!("{base}/sparql"),
            "widgets",
            5,
        )
        .unwrap_err();
        server.join().unwrap();
        assert!(
            err.contains("127.0.0.1 answered with \"text/html\" (HTTP 200), not SPARQL results"),
            "{err}"
        );
    }

    #[test]
    fn dblp_sparql_transport_errors_name_the_endpoint_not_the_query() {
        use crate::http::{response, serve};
        let challenge = response("202 Accepted", &["Content-Type: text/html"], "<html/>");
        let busy = response("503 Service Unavailable", &[], "");
        let (base, server) = serve(vec![challenge, busy]);
        let err = dblp_at(
            &Http::new(),
            &format!("{base}/search/publ/api"),
            &format!("{base}/sparql"),
            "widgets",
            5,
        )
        .unwrap_err();
        assert_eq!(server.join().unwrap().len(), 2);
        assert!(
            err.contains(&format!("HTTP 503 from {base}/sparql. Try again")),
            "{err}"
        );
        assert!(!err.contains("query=") && !err.contains("gave up"), "{err}");
    }

    #[test]
    fn parses_semantic_scholar_hits() {
        let json = r#"{"total":42,"offset":0,"next":2,"data":[
          {"paperId":"abc123","title":"Synthetic  Widgets","year":2024,"authors":[{"authorId":"1","name":"Ann Lee"}],
           "venue":"","externalIds":{"DOI":"10.1234/X","ArXiv":"2401.00001","CorpusId":7},"url":"https://www.semanticscholar.org/paper/abc123"},
          {"paperId":"def456","title":"Gadgets","year":null,"authors":[],"venue":"SYNTH","externalIds":null}]}"#;
        let (total, hits) = parse_s2(json).unwrap();
        assert_eq!(total, Some(42));
        assert_eq!(hits[0].id, "s2:abc123");
        assert_eq!(hits[0].title, "Synthetic Widgets");
        assert_eq!(hits[0].venue, None);
        assert_eq!(hits[0].doi.as_deref(), Some("10.1234/x"));
        assert_eq!(hits[0].arxiv.as_deref(), Some("2401.00001"));
        assert_eq!((hits[1].year, hits[1].doi.as_deref()), (None, None));
    }

    #[test]
    fn parses_cinii_items() {
        let json = r#"{"@id":"https://cir.nii.ac.jp/opensearch/articles?q=x","opensearch:totalResults":9,"items":[
          {"@id":"https://cir.nii.ac.jp/crid/100","title":"Synthetic Widgets","dc:creator":["Lee, Ann","陳 博"],
           "prism:publicationName":"Synthetic Journal","prism:publicationDate":"2024-01-01","description":"<jats:p>About widgets.</jats:p>",
           "dc:identifier":[{"@type":"cir:DOI","@value":"10.48550/arxiv.2401.00001"},{"@type":"cir:URI","@value":"https://example.org/x.pdf"}]},
          {"@id":"https://cir.nii.ac.jp/crid/200","title":"Gadgets"}]}"#;
        let (total, hits) = parse_cinii(json).unwrap();
        assert_eq!(total, Some(9));
        assert_eq!(hits[0].id, "cinii:100");
        assert_eq!(hits[0].authors, ["Ann Lee", "陳 博"]);
        assert_eq!(hits[0].year, Some(2024));
        assert_eq!(hits[0].arxiv.as_deref(), Some("2401.00001"));
        assert_eq!(hits[0].abstract_text.as_deref(), Some("About widgets."));
        assert_eq!((hits[1].year, hits[1].authors.len()), (None, 0));
    }

    #[test]
    fn strips_dblp_homonym_numbers_only() {
        assert_eq!(dblp_name("Ann Lee 0002"), "Ann Lee");
        assert_eq!(dblp_name("Agent 47"), "Agent 47");
        assert_eq!(dblp_name("Bo Chen"), "Bo Chen");
        assert_eq!(dblp_name("R2D2"), "R2D2");
    }
}
