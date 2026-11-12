//! `registry abstracts`: fills the `abstract` field of registry entries from
//! arXiv and OpenAlex by id. It sends only the entries' public ids and
//! downloads no PDF.

use crate::arxiv;
use crate::config::Config;
use crate::id::ArxivId;
use crate::registry::{self, Reference};
use crate::text;

/// An abstract and where it is from (`arxiv:2601.99901v4`, `openalex:W9000000001`).
pub type Found = (String, String);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Keys that got an abstract (with `dry_run`, that would get one).
    pub arxiv: Vec<String>,
    pub openalex: Vec<String>,
    /// Keys whose source has the work without an abstract, or not at all.
    pub without: Vec<String>,
    /// `key: message` for lookups that failed.
    pub failed: Vec<String>,
    /// Entries without an arXiv id, DOI or OpenAlex id.
    pub no_id: usize,
    /// Entries that had an abstract before.
    pub already: usize,
}

/// OpenAlex lookups stop after this many failures in a row.
const MAX_FAILURES: usize = 3;

/// Fills abstracts for the entries with `keys` (all entries when empty) that
/// have none, at most `limit` of them. arXiv answers for entries with an arXiv
/// id, in the recorded version; `openalex` answers for the others that have a
/// DOI or an OpenAlex id. Only `abstract` and `abstract_source` change, under
/// the registry's lock; with `dry_run`, nothing is written.
pub fn backfill(
    config: &Config,
    arxiv: &arxiv::Client,
    openalex: &dyn Fn(&Reference) -> Result<Option<Found>, String>,
    keys: &[String],
    limit: Option<usize>,
    dry_run: bool,
) -> Result<Outcome, String> {
    let records = registry::load(&config.registry)?.unwrap_or_default();
    if let Some(missing) = keys.iter().find(|k| !records.iter().any(|r| &r.key == *k)) {
        return Err(format!(
            "{}: no entry with key {missing}",
            config.display(&config.registry)
        ));
    }
    let mut out = Outcome::default();
    let mut wanted: Vec<&Reference> = Vec::new();
    for r in &records {
        if !keys.is_empty() && !keys.contains(&r.key) {
            continue;
        }
        if r.abstract_text().is_some() {
            out.already += 1;
        } else if r.ids.arxiv.is_none() && r.ids.doi.is_none() && r.ids.openalex.is_none() {
            out.no_id += 1;
        } else if limit.is_none_or(|l| wanted.len() < l) {
            wanted.push(r);
        }
    }
    let mut found: Vec<(String, Found)> = Vec::new();
    // arXiv, in batches.
    let mut asked: Vec<(&Reference, ArxivId)> = Vec::new();
    let mut rest: Vec<&Reference> = Vec::new();
    for r in wanted {
        let Some(id) = &r.ids.arxiv else {
            rest.push(r);
            continue;
        };
        let version = r.version.as_deref().filter(|v| {
            v.strip_prefix('v')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
        match ArxivId::parse(&format!("{id}{}", version.unwrap_or(""))) {
            Ok(id) => asked.push((r, id)),
            Err(e) => out.failed.push(format!("{}: {e}", r.key)),
        }
    }
    let ids: Vec<ArxivId> = asked.iter().map(|(_, id)| id.clone()).collect();
    for ((r, _), paper) in asked.iter().zip(arxiv.fetch(&ids)) {
        match paper {
            Ok(p) => {
                let text = text::squash(&p.abstract_text);
                if text.is_empty() {
                    out.without.push(r.key.clone());
                } else {
                    out.arxiv.push(r.key.clone());
                    let source = format!("arxiv:{}v{}", p.id, p.version);
                    found.push((r.key.clone(), (text, source)));
                }
            }
            Err(e) => out.failed.push(format!("{}: {e}", r.key)),
        }
    }
    // OpenAlex, one work per request.
    let mut failures = 0;
    for r in rest {
        if failures >= MAX_FAILURES {
            out.failed.push(format!(
                "{}: not attempted: earlier OpenAlex requests failed",
                r.key
            ));
            continue;
        }
        match openalex(r) {
            Ok(Some((text, source))) => {
                failures = 0;
                let text = text::squash(&text);
                if text.is_empty() {
                    out.without.push(r.key.clone());
                } else {
                    out.openalex.push(r.key.clone());
                    found.push((r.key.clone(), (text, source)));
                }
            }
            Ok(None) => {
                failures = 0;
                out.without.push(r.key.clone());
            }
            Err(e) => {
                failures += 1;
                out.failed.push(format!("{}: {e}", r.key));
            }
        }
    }
    if !dry_run && !found.is_empty() {
        registry::update(&config.registry, |records| {
            for (key, (text, source)) in found {
                // The registry as it is now: an entry may have left or been filled.
                if let Some(r) = records
                    .iter_mut()
                    .find(|r| r.key == key && r.abstract_text().is_none())
                {
                    r.set_abstract(text, source);
                }
            }
            Ok(())
        })?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{self, Http};

    #[test]
    fn adds_the_abstract_by_arxiv_id_and_changes_nothing_else() {
        let dir = std::env::temp_dir().join(format!("papers-abstracts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("research.toml"), "[papers]\n").unwrap();
        // Unknown fields and the entry order must survive.
        let before = [
            r#"{"schema":"rf.reference/1","key":"3","type":"paper","title":"No Ids","authors":null,"year":null,"venue":null,"ids":{"doi":null,"arxiv":null,"dblp":null,"openalex":null,"url":null},"version":null,"accessed":null,"depth":null,"texts":[],"duplicate_of":null,"source":null,"verified":null}"#,
            r#"{"schema":"rf.reference/1","key":"87","type":"paper","title":"A Title","authors":["Ada Example"],"year":2026,"venue":null,"ids":{"doi":null,"arxiv":"2601.99901","dblp":null,"openalex":null,"url":null,"other":"kept"},"version":"v4","accessed":"2026-11-01","depth":"full","texts":[],"duplicate_of":null,"source":null,"verified":null,"custom":{"kept":true}}"#,
            r#"{"schema":"rf.reference/1","key":"90","type":"paper","title":"Has One","authors":null,"year":null,"venue":null,"ids":{"doi":"10.1234/synthetic.90","arxiv":null,"dblp":null,"openalex":null,"url":null},"version":null,"accessed":null,"depth":null,"texts":[],"duplicate_of":null,"source":null,"verified":null,"abstract":"Old words.","abstract_source":"openalex:W9000000090"}"#,
            r#"{"schema":"rf.reference/1","key":"91","type":"paper","title":"By DOI","authors":null,"year":null,"venue":null,"ids":{"doi":"10.1234/synthetic.91","arxiv":null,"dblp":null,"openalex":null,"url":null},"version":null,"accessed":null,"depth":null,"texts":[],"duplicate_of":null,"source":null,"verified":null}"#,
        ];
        std::fs::write(dir.join("references.jsonl"), before.join("\n") + "\n").unwrap();
        let config = Config::load(Some(&dir.join("research.toml")), &dir).unwrap();
        let feed = http::response(
            "200 OK",
            &["Content-Type: application/atom+xml"],
            include_str!("../tests/fixtures/arxiv-entries.xml"),
        );
        let (base, server) = http::serve(vec![feed.clone(), feed]);
        let endpoint = format!("{base}/api/query");
        let http = Http::new();
        let arxiv = arxiv::Client::at(&http, &endpoint);
        let openalex = |r: &Reference| -> Result<Option<Found>, String> {
            assert_eq!(r.key, "91");
            Ok(Some((
                " Words  from\nOpenAlex. ".into(),
                "openalex:W9000000091".into(),
            )))
        };
        let read = || std::fs::read_to_string(dir.join("references.jsonl")).unwrap();
        // A dry run names the keys and writes nothing.
        let dry = backfill(&config, &arxiv, &openalex, &[], None, true).unwrap();
        let unchanged = read();
        let got = backfill(&config, &arxiv, &openalex, &[], None, false).unwrap();
        let after = read();
        let requests = server.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(unchanged, before.join("\n") + "\n");
        assert_eq!(dry, got);
        assert_eq!(
            got,
            Outcome {
                arxiv: vec!["87".into()],
                openalex: vec!["91".into()],
                without: vec![],
                failed: vec![],
                no_id: 1,
                already: 1,
            }
        );
        // Only the public id is sent.
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0].contains("id_list=2601.99901v4"),
            "{}",
            requests[0]
        );
        assert!(!requests[0].to_lowercase().contains("mailto"));
        let value = |line: &str| serde_json::from_str::<serde_json::Value>(line).unwrap();
        let lines: Vec<&str> = after.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], before[0]);
        assert_eq!(lines[2], before[2]);
        for (line, old, source) in [
            (lines[1], before[1], "arxiv:2601.99901v4"),
            (lines[3], before[3], "openalex:W9000000091"),
        ] {
            let mut new = value(line);
            let object = new.as_object_mut().unwrap();
            let text = object.remove("abstract").unwrap();
            assert_eq!(object.remove("abstract_source").unwrap(), source);
            assert_eq!(new, value(old));
            let text = text.as_str().unwrap();
            assert!(!text.is_empty() && text == text::squash(text), "{text:?}");
        }
        assert!(lines[3].contains(r#""abstract":"Words from OpenAlex.""#));
    }
}
