//! Parsing of the arXiv API's Atom feed.

use roxmltree::{Document, Node};
use serde::Serialize;

use crate::id::parse_bare;
use crate::text::squash;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const ARXIV: &str = "http://arxiv.org/schemas/atom";
const OPENSEARCH: &str = "http://a9.com/-/spec/opensearch/1.1/";

#[derive(Debug, Clone, Serialize)]
pub struct Paper {
    /// Versionless id.
    pub id: String,
    pub version: u32,
    pub title: String,
    pub authors: Vec<String>,
    /// v1 submission date, `YYYY-MM-DD`.
    pub published: String,
    /// This version's date, `YYYY-MM-DD`.
    pub updated: String,
    pub primary_category: String,
    pub categories: Vec<String>,
    pub journal_ref: Option<String>,
    pub doi: Option<String>,
    pub comment: Option<String>,
    #[serde(rename = "abstract")]
    pub abstract_text: String,
    /// Versionless landing page.
    pub abs_url: String,
}

#[derive(Debug, Default)]
pub struct Feed {
    pub total: Option<u64>,
    pub papers: Vec<Paper>,
    /// Messages of the API's `Error` entries (e.g. "incorrect id format for x").
    pub errors: Vec<String>,
}

pub fn parse_feed(xml: &str) -> Result<Feed, String> {
    let doc = Document::parse(xml).map_err(|e| format!("invalid Atom XML from arXiv: {e}"))?;
    let root = doc.root_element();
    if !root.has_tag_name((ATOM, "feed")) {
        return Err("arXiv response is not an Atom feed".to_owned());
    }
    let mut feed = Feed {
        total: child_text(root, OPENSEARCH, "totalResults").and_then(|t| t.trim().parse().ok()),
        ..Feed::default()
    };
    for entry in root.children().filter(|n| n.has_tag_name((ATOM, "entry"))) {
        let entry_id = child_text(entry, ATOM, "id").unwrap_or_default();
        if entry_id.contains("arxiv.org/api/errors") {
            let message = child_text(entry, ATOM, "summary").map(squash);
            feed.errors
                .push(message.unwrap_or_else(|| entry_id.to_owned()));
        } else {
            feed.papers.push(parse_entry(entry, entry_id)?);
        }
    }
    Ok(feed)
}

fn parse_entry(entry: Node, entry_id: &str) -> Result<Paper, String> {
    let missing = |field: &str| format!("arXiv entry {entry_id:?} has no {field}");
    let (_, versioned) = entry_id
        .split_once("/abs/")
        .ok_or_else(|| missing("abs id"))?;
    let parsed = parse_bare(versioned).ok_or_else(|| missing("valid id"))?;
    let version = parsed.version.ok_or_else(|| missing("version"))?;
    let date = |name: &str| {
        child_text(entry, ATOM, name)
            .and_then(|t| t.trim().get(..10))
            .map(str::to_owned)
            .ok_or_else(|| missing(name))
    };
    let optional = |name: &str| {
        child_text(entry, ARXIV, name)
            .map(squash)
            .filter(|t| !t.is_empty())
    };
    Ok(Paper {
        version,
        title: child_text(entry, ATOM, "title")
            .map(squash)
            .ok_or_else(|| missing("title"))?,
        authors: entry
            .children()
            .filter(|n| n.has_tag_name((ATOM, "author")))
            .filter_map(|a| child_text(a, ATOM, "name").map(squash))
            .collect(),
        published: date("published")?,
        updated: date("updated")?,
        primary_category: entry
            .children()
            .find(|n| n.has_tag_name((ARXIV, "primary_category")))
            .and_then(|n| n.attribute("term"))
            .ok_or_else(|| missing("primary category"))?
            .to_owned(),
        categories: entry
            .children()
            .filter(|n| n.has_tag_name((ATOM, "category")))
            .filter_map(|n| n.attribute("term").map(str::to_owned))
            .collect(),
        journal_ref: optional("journal_ref"),
        doi: optional("doi"),
        comment: optional("comment"),
        abstract_text: child_text(entry, ATOM, "summary")
            .map(squash)
            .unwrap_or_default(),
        abs_url: parsed.abs_url(),
        id: parsed.id,
    })
}

fn child_text<'a>(node: Node<'a, '_>, ns: &str, name: &str) -> Option<&'a str> {
    node.children()
        .find(|n| n.has_tag_name((ns, name)))
        .map(|n| n.text().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic feeds in the arXiv API's shape (see tests/fixtures/README.md).
    const ENTRIES: &str = include_str!("../tests/fixtures/arxiv-entries.xml");
    const ERROR: &str = include_str!("../tests/fixtures/arxiv-error.xml");
    const EMPTY: &str = include_str!("../tests/fixtures/arxiv-empty.xml");

    #[test]
    fn parses_entries() {
        let feed = parse_feed(ENTRIES).unwrap();
        assert_eq!(feed.total, Some(3));
        assert!(feed.errors.is_empty());
        let ids: Vec<_> = feed.papers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["2601.99901", "2601.99902", "hep-th/9901999"]);

        let first = &feed.papers[0];
        assert_eq!(first.version, 4);
        assert_eq!(first.title, "Rebalancing Costs of a Synthetic Market Maker");
        assert_eq!(
            first.authors,
            [
                "Mira Ellingwood",
                "Niles O. Barrow",
                "Opal Strand",
                "Pell Anders Kemp"
            ]
        );
        assert_eq!(first.published, "2026-01-12");
        assert_eq!(first.updated, "2026-03-15");
        assert_eq!(first.primary_category, "q-fin.MF");
        assert_eq!(first.categories.len(), 5);
        assert_eq!(first.comment.as_deref(), Some("12 pages, 3 figures"));
        assert_eq!(first.journal_ref, None);
        assert_eq!(first.abs_url, "https://arxiv.org/abs/2601.99901");
        // Line breaks in the summary are squashed.
        assert!(first.abstract_text.starts_with(
            "Synthetic market makers quote prices from a fixed curve. We measure what a passive"
        ));

        let old = &feed.papers[2];
        assert_eq!(old.version, 3);
        assert_eq!(
            old.journal_ref.as_deref(),
            Some("Synth. J. Phys. 1:1-8,1999")
        );
        assert_eq!(old.doi.as_deref(), Some("10.5555/SJP.1.1"));
        // Leading whitespace in the summary is normalized away.
        assert!(old.abstract_text.starts_with("We explicitly give"));
    }

    #[test]
    fn parses_error_entry() {
        let feed = parse_feed(ERROR).unwrap();
        assert!(feed.papers.is_empty());
        assert_eq!(feed.errors, ["incorrect id format for not-an-id"]);
    }

    #[test]
    fn missing_ids_yield_an_empty_feed() {
        let feed = parse_feed(EMPTY).unwrap();
        assert_eq!(feed.total, Some(0));
        assert!(feed.papers.is_empty() && feed.errors.is_empty());
    }

    #[test]
    fn rejects_non_feed() {
        assert!(parse_feed("<html/>").is_err());
        assert!(parse_feed("not xml").is_err());
    }
}
