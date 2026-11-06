//! Citation graph: the works a paper cites (`refs`) and the works citing it
//! (`cited-by`), from OpenAlex (`referenced_works`, the `cites:` filter) with
//! Semantic Scholar as the fallback.

use serde::Deserialize;

use crate::http::{Get, Http, encode};
use crate::openalex;
use crate::refs::Ref;
use crate::sources::{self, Hit, Page};

/// OpenAlex accepts at most 100 values in one OR filter.
const OR_LIMIT: usize = 100;
const S2_PAPER: &str = "https://api.semanticscholar.org/graph/v1/paper";
const S2_HOST: &str = "api.semanticscholar.org";
const S2_GRAPH_FIELDS: &str = "title,year,authors,venue,externalIds,url";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Refs,
    CitedBy,
}

impl Direction {
    pub fn command(self) -> &'static str {
        match self {
            Direction::Refs => "refs",
            Direction::CitedBy => "cited-by",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GraphSource {
    /// OpenAlex, then Semantic Scholar when OpenAlex has no record or no links.
    Auto,
    Openalex,
    S2,
}

/// A work named on the command line.
#[derive(Debug, Clone, PartialEq)]
pub enum Work {
    Ref(Ref),
    /// OpenAlex short id, `W123`.
    OpenAlex(String),
}

impl Work {
    pub fn parse(input: &str) -> Result<Work, String> {
        let short = openalex::short_id(input.trim());
        let is_w = short.len() > 1
            && short.starts_with(['W', 'w'])
            && short[1..].bytes().all(|b| b.is_ascii_digit())
            && (short.len() == input.trim().len() || input.contains("openalex.org/"));
        if is_w {
            return Ok(Work::OpenAlex(format!("W{}", &short[1..])));
        }
        Ref::parse(input)
            .map(Work::Ref)
            .map_err(|e| format!("{e}; or an OpenAlex work id (W123)"))
    }

    /// The Semantic Scholar paper id form (`DOI:…`, `ARXIV:…`), if any.
    fn s2_id(&self) -> Option<String> {
        match self {
            Work::Ref(Ref::Arxiv(id)) => Some(format!("ARXIV:{}", id.id)),
            Work::Ref(Ref::Doi(doi)) => Some(format!("DOI:{doi}")),
            Work::OpenAlex(_) => None,
        }
    }
}

/// The links found and where they came from.
pub struct Links {
    pub source: &'static str,
    /// The work as the source names it (`W123`, `ARXIV:…`).
    pub work: String,
    pub page: Page,
}

pub fn links(
    http: &Http,
    work: &Work,
    direction: Direction,
    source: GraphSource,
    max: usize,
) -> Result<Links, String> {
    let mut s2_id = work.s2_id();
    if source != GraphSource::S2 {
        let client = openalex::Client::new(http);
        let found = match work {
            Work::OpenAlex(id) => client.by_id(id)?,
            Work::Ref(Ref::Doi(doi)) => client.by_doi(doi)?,
            Work::Ref(Ref::Arxiv(id)) => {
                let doi = crate::refs::Doi::parse(&format!("10.48550/arXiv.{}", id.id))
                    .expect("arXiv DOIs parse");
                client.by_doi(&doi)?
            }
        };
        match found {
            Some(w) => {
                if s2_id.is_none() {
                    s2_id = w.doi.as_ref().map(|d| format!("DOI:{d}"));
                }
                let page = openalex_links(&client, &w, direction, max)?;
                if !page.1.is_empty() || source == GraphSource::Openalex {
                    return Ok(Links {
                        source: "openalex",
                        work: w.id,
                        page,
                    });
                }
                eprintln!(
                    "papers: OpenAlex lists no {} for {}; trying Semantic Scholar",
                    what(direction),
                    w.id
                );
            }
            None if source == GraphSource::Openalex => {
                return Err("OpenAlex has no record of this work".to_owned());
            }
            None => {
                eprintln!("papers: OpenAlex has no record of this work; trying Semantic Scholar")
            }
        }
    }
    let id = s2_id.ok_or("Semantic Scholar needs a DOI or arXiv id for this work")?;
    let page = s2_links(http, &id, direction, max)?;
    Ok(Links {
        source: "s2",
        work: id,
        page,
    })
}

fn what(direction: Direction) -> &'static str {
    match direction {
        Direction::Refs => "references",
        Direction::CitedBy => "citing works",
    }
}

fn openalex_links(
    client: &openalex::Client,
    w: &openalex::Work,
    direction: Direction,
    max: usize,
) -> Result<Page, String> {
    match direction {
        Direction::CitedBy => {
            let (total, works) = client.list(&format!("cites:{}", w.id), max)?;
            Ok((total, works.into_iter().map(Hit::from).collect()))
        }
        Direction::Refs => {
            let wanted: Vec<&String> = w.referenced_works.iter().take(max).collect();
            let mut works: Vec<openalex::Work> = Vec::new();
            for chunk in wanted.chunks(OR_LIMIT) {
                let ids: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();
                let (_, got) = client.list(&format!("openalex:{}", ids.join("|")), chunk.len())?;
                works.extend(got);
            }
            // Keep OpenAlex's reference order; the filter returns its own order.
            works.sort_by_key(|x| wanted.iter().position(|id| **id == x.id));
            Ok((
                Some(w.referenced_works.len() as u64),
                works.into_iter().map(Hit::from).collect(),
            ))
        }
    }
}

#[derive(Deserialize)]
struct S2LinkPage {
    #[serde(default)]
    data: Vec<S2Link>,
    next: Option<u64>,
}

#[derive(Deserialize)]
struct S2Link {
    #[serde(rename = "citedPaper")]
    cited: Option<serde_json::Value>,
    #[serde(rename = "citingPaper")]
    citing: Option<serde_json::Value>,
}

/// Parses one page of `/references` or `/citations`; links to papers Semantic
/// Scholar could not resolve (no paper id) are dropped.
pub fn parse_s2_links(json: &str) -> Result<(Vec<Hit>, Option<u64>), String> {
    let page: S2LinkPage =
        serde_json::from_str(json).map_err(|e| format!("invalid Semantic Scholar JSON: {e}"))?;
    let mut hits = Vec::new();
    for link in page.data {
        let Some(paper) = link.cited.or(link.citing) else {
            continue;
        };
        if paper.get("paperId").is_none_or(serde_json::Value::is_null) {
            continue;
        }
        let one = serde_json::json!({"total": null, "data": [paper]}).to_string();
        hits.extend(sources::parse_s2(&one)?.1);
    }
    Ok((hits, page.next))
}

fn s2_links(http: &Http, id: &str, direction: Direction, max: usize) -> Result<Page, String> {
    let endpoint = match direction {
        Direction::Refs => "references",
        Direction::CitedBy => "citations",
    };
    let headers = sources::s2_headers();
    let mut hits = Vec::new();
    let mut offset = 0u64;
    while hits.len() < max {
        let limit = (max - hits.len()).min(100);
        let url = format!(
            "{S2_PAPER}/{}/{endpoint}?fields={S2_GRAPH_FIELDS}&offset={offset}&limit={limit}",
            encode(id, b":/")
        );
        let reply = http.get(&Get {
            headers: &headers,
            headers_host: S2_HOST,
            ..Get::api(&url, &[200, 404])
        })?;
        if reply.status == 404 {
            return Err(format!("Semantic Scholar has no paper {id}"));
        }
        let (got, next) = parse_s2_links(&reply.text())?;
        hits.extend(got);
        match next {
            Some(n) if n > offset => offset = n,
            _ => break,
        }
    }
    hits.truncate(max);
    Ok((None, hits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_work_arguments() {
        assert_eq!(Work::parse("W123").unwrap(), Work::OpenAlex("W123".into()));
        assert_eq!(
            Work::parse("https://openalex.org/W42").unwrap(),
            Work::OpenAlex("W42".into())
        );
        assert!(matches!(
            Work::parse("2601.00001"),
            Ok(Work::Ref(Ref::Arxiv(_)))
        ));
        assert!(matches!(
            Work::parse("10.1234/w1"),
            Ok(Work::Ref(Ref::Doi(_)))
        ));
        assert_eq!(
            Work::parse("arXiv:2601.00001v2")
                .unwrap()
                .s2_id()
                .as_deref(),
            Some("ARXIV:2601.00001")
        );
        assert!(Work::parse("Wonder").is_err());
    }

    #[test]
    fn parses_semantic_scholar_links() {
        let json = r#"{"offset":0,"next":3,"data":[
          {"citedPaper":{"paperId":"a1","title":"Synthetic Widgets","year":2024,"authors":[{"name":"Ann Lee"}],"externalIds":{"DOI":"10.1234/W"}}},
          {"citedPaper":{"paperId":null,"title":"Unresolved reference"}},
          {"citingPaper":{"paperId":"b2","title":"Gadgets","year":2026,"authors":[],"externalIds":{"ArXiv":"2601.00001"}}}]}"#;
        let (hits, next) = parse_s2_links(json).unwrap();
        assert_eq!(next, Some(3));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "s2:a1");
        assert_eq!(hits[0].doi.as_deref(), Some("10.1234/w"));
        assert_eq!(hits[1].arxiv.as_deref(), Some("2601.00001"));
    }
}
