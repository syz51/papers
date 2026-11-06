//! The search log: one `rf.search/1` JSON line per search or lookup, appended
//! to `[papers] search_log`, so a literature review can show what was searched,
//! when, and what came back.

use std::io::Write as _;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::seen::{Index, Query};

pub const SCHEMA: &str = "rf.search/1";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    /// Source-prefixed id: `arxiv:2601.00001v2`, `doi:10.1/x`, `openalex:W1`, …
    pub id: String,
    pub title: Option<String>,
    pub year: Option<i64>,
}

/// One result with the identifiers used to find it in the archive.
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub id: String,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub doi: Option<String>,
    pub arxiv: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub schema: &'static str,
    /// RFC 3339 UTC.
    pub at: String,
    /// `arxiv`, `openalex`, `crossref`, `dblp`, `s2`, or `cinii`.
    pub source: String,
    pub query: String,
    pub params: Value,
    pub results: Vec<Item>,
    /// Archive keys (registry or notes) among the results.
    pub seen: Vec<String>,
}

impl Entry {
    pub fn new(source: &str, query: &str, params: Value, found: &[Found], index: &Index) -> Entry {
        let mut seen: Vec<String> = Vec::new();
        for f in found {
            let hits = index.lookup(&Query {
                doi: f.doi.clone(),
                arxiv: f.arxiv.clone(),
                url: f.url.clone(),
                title: f.title.clone(),
            });
            for h in hits {
                if !seen.contains(&h.key) {
                    seen.push(h.key);
                }
            }
        }
        seen.sort_by(|a, b| crate::registry::key_order(a, b));
        Entry {
            schema: SCHEMA,
            at: crate::finding::now(),
            source: source.to_owned(),
            query: query.to_owned(),
            params,
            results: found
                .iter()
                .map(|f| Item {
                    id: f.id.clone(),
                    title: f.title.clone(),
                    year: f.year,
                })
                .collect(),
            seen,
        }
    }
}

/// Appends entries as JSON lines, creating the file and its directory.
pub fn append(path: &Path, entries: &[Entry]) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let err = |e: std::io::Error| format!("search log {}: {e}", path.display());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    let mut text = String::new();
    for e in entries {
        text += &serde_json::to_string(e).expect("search entries serialize");
        text.push('\n');
    }
    // One write in append mode, so concurrent runs do not interleave lines.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Reference;

    #[test]
    fn entries_name_seen_keys_and_append() {
        let mut known = Reference::new("12".into());
        known.ids.arxiv = Some("2601.00001".into());
        let mut other = Reference::new("3".into());
        other.title = Some("Synthetic Widgets".into());
        let mut index = Index::default();
        index.add(&known, "registry");
        index.add(&other, "registry");
        let found = [
            Found {
                id: "arxiv:2601.00001v2".into(),
                arxiv: Some("2601.00001".into()),
                title: Some("A".into()),
                year: Some(2026),
                ..Found::default()
            },
            Found {
                id: "openalex:W1".into(),
                title: Some("Synthetic widgets".into()),
                ..Found::default()
            },
            Found {
                id: "doi:10.1/none".into(),
                doi: Some("10.1/none".into()),
                ..Found::default()
            },
        ];
        let mut e = Entry::new(
            "arxiv",
            "widgets",
            serde_json::json!({"max": 3}),
            &found,
            &index,
        );
        assert_eq!(e.seen, ["3", "12"]);
        e.at = "2026-11-07T00:00:00Z".into();
        let line = serde_json::to_string(&e).unwrap();
        assert!(
            line.starts_with(r#"{"schema":"rf.search/1","at":"2026-11-07T00:00:00Z","source":"arxiv","query":"widgets","params":{"max":3},"results":[{"id":"arxiv:2601.00001v2","title":"A","year":2026}"#),
            "{line}"
        );
        let dir = std::env::temp_dir().join(format!("papers-log-test-{}", std::process::id()));
        let path = dir.join("nested/search.jsonl");
        append(&path, &[e]).unwrap();
        let e2 = Entry::new("openalex", "x", Value::Null, &[], &index);
        append(&path, &[e2]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
