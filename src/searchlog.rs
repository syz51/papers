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
    pub openalex: Option<String>,
    pub url: Option<String>,
}

impl Found {
    /// The identifiers to look this result up by in the archive.
    pub fn query(&self) -> Query {
        Query {
            doi: self.doi.clone(),
            arxiv: self.arxiv.clone(),
            openalex: self.openalex.clone(),
            url: self.url.clone(),
            title: self.title.clone(),
        }
    }
}

/// The archive keys among `index` that hold `found`, in key order.
pub fn seen_keys(index: &Index, found: &Found) -> Vec<String> {
    let mut keys: Vec<String> = index
        .lookup(&found.query())
        .into_iter()
        .map(|h| h.key)
        .collect();
    keys.dedup();
    keys
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
            for key in seen_keys(index, f) {
                if !seen.contains(&key) {
                    seen.push(key);
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

/// The earlier runs of one search, from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Earlier {
    /// When the last run was logged, RFC 3339 UTC.
    pub at: String,
    /// The results the last run logged.
    pub results: usize,
    /// How many runs the log holds.
    pub runs: usize,
}

/// A query's comparison form: accents folded, lower case, single spaces.
fn normal_query(query: &str) -> String {
    crate::text::squash(&crate::text::fold(query).to_lowercase())
}

/// The logged `search` runs with the same source and normalized query; `None`
/// when there are none or no log. Lines that are not log entries are skipped,
/// as are `fetch`, `refs` and `cited-by` entries, which record a `command`.
pub fn earlier(path: &Path, source: &str, query: &str) -> Result<Option<Earlier>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("search log {}: {e}", path.display())),
    };
    let wanted = normal_query(query);
    let mut last: Option<Earlier> = None;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let field = |name: &str| entry.get(name).and_then(Value::as_str);
        if field("schema") != Some(SCHEMA)
            || field("source") != Some(source)
            || entry.pointer("/params/command").is_some()
            || field("query").map(normal_query).as_deref() != Some(wanted.as_str())
        {
            continue;
        }
        let Some(at) = field("at") else { continue };
        let results = entry
            .get("results")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let runs = last.as_ref().map_or(0, |l| l.runs) + 1;
        // The log is appended in time order, but a merged log may not be.
        if last.as_ref().is_none_or(|l| l.at.as_str() <= at) {
            last = Some(Earlier {
                at: at.to_owned(),
                results,
                runs,
            });
        } else if let Some(l) = &mut last {
            l.runs = runs;
        }
    }
    Ok(last)
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

    #[test]
    fn earlier_finds_the_last_run_of_the_same_query_and_source() {
        let dir = std::env::temp_dir().join(format!("papers-log-earlier-{}", std::process::id()));
        let path = dir.join("search.jsonl");
        assert_eq!(earlier(&path, "arxiv", "widgets"), Ok(None));
        std::fs::create_dir_all(&dir).unwrap();
        let line = |at: &str, source: &str, query: &str, params: &str, n: usize| {
            let results = vec![r#"{"id":"x","title":null,"year":null}"#; n].join(",");
            format!(
                r#"{{"schema":"rf.search/1","at":"{at}","source":"{source}","query":"{query}","params":{params},"results":[{results}],"seen":[]}}"#
            )
        };
        let log = [
            line(
                "2026-11-01T00:00:00Z",
                "arxiv",
                "Synthetic  Widgets",
                "{}",
                3,
            ),
            "not json".to_owned(),
            line(
                "2026-11-03T00:00:00Z",
                "arxiv",
                "synthetic widgets",
                "{}",
                2,
            ),
            line(
                "2026-11-02T00:00:00Z",
                "arxiv",
                "synthétic widgets",
                "{}",
                5,
            ),
            line(
                "2026-11-04T00:00:00Z",
                "openalex",
                "synthetic widgets",
                "{}",
                7,
            ),
            line(
                "2026-11-05T00:00:00Z",
                "arxiv",
                "synthetic widgets",
                r#"{"command":"fetch"}"#,
                1,
            ),
            line("2026-11-06T00:00:00Z", "arxiv", "synthetic widget", "{}", 1),
        ]
        .join("\n");
        std::fs::write(&path, log).unwrap();
        assert_eq!(
            earlier(&path, "arxiv", " synthetic WIDGETS "),
            Ok(Some(Earlier {
                at: "2026-11-03T00:00:00Z".into(),
                results: 2,
                runs: 3,
            }))
        );
        assert_eq!(earlier(&path, "dblp", "synthetic widgets"), Ok(None));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
