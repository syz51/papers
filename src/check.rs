//! `registry check` rules over parsed records and parse issues.
//!
//! | Rule | Severity | Meaning |
//! | --- | --- | --- |
//! | `duplicate-doi`, `duplicate-arxiv`, `duplicate-url`, `duplicate-title` | warn; info when annotated | keys that share a DOI, an arXiv id (any version), a normalized URL, or a normalized title plus first-author surname |
//! | `duplicate-target-missing` | error | `duplicate_of` names a key the registry lacks |
//! | `duplicate-cycle` | error | following `duplicate_of` from a key comes back to it |
//! | `duplicate-chain` | warn | `duplicate_of` names a key that is itself a duplicate |
//! | `verified-mismatch` | warn | `verify --record` found the entry's metadata differs from its index, or not there |
//! | `malformed-doi` | warn | a `doi` that does not parse, or that ends in a character other than an ASCII letter, an ASCII digit or `)` |
//! | `no-identifier` | warn | no DOI, arXiv id, or URL |
//! | `malformed-date` | error (warn for reported dates) | an access or reported date that is not a calendar date |
//! | `invalid-value` | error | `type`, `depth` or `verified.status` outside the `rf.reference/1` vocabulary |
//! | `key-mismatch` | error | the heading number and the `[N]` entry number differ |
//! | plus the parser's `entry-unnumbered`, `entry-missing`, `duplicate-key`, `duplicate-annotation-conflict`, `ambiguous-id`, `authors-unparsed`, `frontmatter-invalid`, `depth-invalid`, `depth-unparsed`, `depth-missing` |

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::catalogue::Issue;
use crate::finding::{Finding, Severity};
use crate::id::ArxivId;
use crate::refs::{Doi, Ref};
use crate::registry::{DEPTHS, Reference, TYPES, VERIFIED_STATUSES, key_order};
use crate::text::{normalize_url, title_author_key};

/// Turns parse issues into findings located in their files.
pub fn issue_findings(issues: &[Issue]) -> Vec<Finding> {
    issues
        .iter()
        .map(|i| {
            Finding::new(i.rule, i.severity, i.key.clone(), i.message.clone())
                .evidence(&i.path, format!("line {}", i.line))
        })
        .collect()
}

/// An entry's DOI: its `doi` (unless an arXiv DOI), else a doi.org, SSRN, or NBER `url`.
fn entry_doi(r: &Reference) -> Option<String> {
    let from_url = || match Ref::from_note_url(r.ids.url.as_deref()?)? {
        Ref::Doi(doi) => Some(doi),
        Ref::Arxiv(_) => None,
    };
    let doi = r.ids.doi.as_deref().and_then(Doi::parse);
    match doi {
        Some(d) if d.arxiv().is_some() => from_url(),
        Some(d) => Some(d),
        None => from_url(),
    }
    .map(|d| d.to_string())
}

/// An entry's versionless arXiv id: its `arxiv`, else an arXiv DOI, else an arxiv.org `url`.
fn entry_arxiv(r: &Reference) -> Option<String> {
    let id = r.ids.arxiv.as_deref().and_then(|a| ArxivId::parse(a).ok());
    let from_doi = || Doi::parse(r.ids.doi.as_deref()?)?.arxiv();
    let from_url = || match Ref::from_note_url(r.ids.url.as_deref()?)? {
        Ref::Arxiv(id) => Some(id),
        Ref::Doi(_) => None,
    };
    id.or_else(from_doi).or_else(from_url).map(|a| a.id)
}

/// A duplicate rule: its name, what the keys share, and the shared value.
type DuplicateRule = (&'static str, &'static str, fn(&Reference) -> Option<String>);

/// Runs the record rules.
pub fn check(records: &[Reference]) -> Vec<Finding> {
    let mut out = Vec::new();
    let keys: BTreeSet<&str> = records.iter().map(|r| r.key.as_str()).collect();
    let rules: [DuplicateRule; 4] = [
        ("duplicate-doi", "DOI", entry_doi),
        ("duplicate-arxiv", "arXiv id", entry_arxiv),
        ("duplicate-url", "URL", |r| {
            // URLs that name an arXiv paper or a DOI are covered by those rules.
            let url = r.ids.url.as_deref()?;
            Ref::from_note_url(url)
                .is_none()
                .then(|| normalize_url(url))?
        }),
        ("duplicate-title", "title and first author", |r| {
            title_author_key(
                r.title.as_deref()?,
                r.authors
                    .as_ref()
                    .and_then(|a| a.first())
                    .map(String::as_str),
            )
        }),
    ];
    for (rule, what, value) in rules {
        let mut groups: BTreeMap<String, Vec<&Reference>> = BTreeMap::new();
        for r in records {
            if let Some(v) = value(r) {
                groups.entry(v).or_default().push(r);
            }
        }
        let mut found: Vec<Finding> = groups
            .into_iter()
            .filter(|(_, g)| g.len() > 1)
            .map(|(v, g)| duplicate_finding(rule, what, &v, &g))
            .collect();
        found.sort_by(|a, b| key_order(&a.subject, &b.subject));
        out.extend(found);
    }
    out.extend(duplicate_chains(records));
    for r in records {
        if let Some(target) = &r.duplicate_of
            && !keys.contains(target.as_str())
        {
            out.push(with_source(
                Finding::new(
                    "duplicate-target-missing",
                    Severity::Error,
                    r.key.clone(),
                    format!("duplicate_of names key {target}, which the registry does not have"),
                ),
                r,
            ));
        }
        if r.ids.doi.is_none() && r.ids.arxiv.is_none() && r.ids.url.is_none() {
            out.push(with_source(
                Finding::new(
                    "no-identifier",
                    Severity::Warn,
                    r.key.clone(),
                    "the entry has no DOI, arXiv id, or URL".to_owned(),
                ),
                r,
            ));
        }
        for (field, value, allowed) in [
            ("type", &r.kind, &TYPES[..]),
            ("depth", &r.depth, &DEPTHS[..]),
        ] {
            if let Some(v) = value
                && !allowed.contains(&v.as_str())
            {
                out.push(with_source(
                    Finding::new(
                        "invalid-value",
                        Severity::Error,
                        r.key.clone(),
                        format!("{field} {v:?} is not one of {}", allowed.join(", ")),
                    ),
                    r,
                ));
            }
        }
        if let Some(v) = &r.verified {
            let finding = if !VERIFIED_STATUSES.contains(&v.status.as_str()) {
                Some(Finding::new(
                    "invalid-value",
                    Severity::Error,
                    r.key.clone(),
                    format!(
                        "verified.status {:?} is not one of {}",
                        v.status,
                        VERIFIED_STATUSES.join(", ")
                    ),
                ))
            } else if !valid_iso_date(&v.at) {
                Some(Finding::new(
                    "malformed-date",
                    Severity::Error,
                    r.key.clone(),
                    format!("verified.at {:?} is not a YYYY-MM-DD calendar date", v.at),
                ))
            } else if v.status != "match" {
                Some(Finding::new(
                    "verified-mismatch",
                    Severity::Warn,
                    r.key.clone(),
                    format!(
                        "papers verify recorded {} against {} on {}; check the entry, then run papers verify --keys {} --record",
                        v.status, v.against, v.at, r.key
                    ),
                ))
            } else {
                None
            };
            out.extend(finding.map(|f| with_source(f, r)));
        }
        if let Some(doi) = &r.ids.doi
            && (Doi::parse(doi).is_none()
                || !doi.ends_with(|c: char| c.is_ascii_alphanumeric() || c == ')'))
        {
            out.push(with_source(
                Finding::new(
                    "malformed-doi",
                    Severity::Warn,
                    r.key.clone(),
                    format!("doi {doi:?} is not a DOI, or ends in a stray character"),
                ),
                r,
            ));
        }
        if let Some(date) = &r.accessed
            && !valid_iso_date(date)
        {
            out.push(with_source(
                Finding::new(
                    "malformed-date",
                    Severity::Error,
                    r.key.clone(),
                    format!("accessed {date:?} is not a YYYY-MM-DD calendar date"),
                ),
                r,
            ));
        }
    }
    out
}

/// `duplicate_of` links followed from each key: a cycle once per cycle, at
/// its least key, and a chain at each key whose target is itself a
/// duplicate, naming the key the chain ends at. Missing targets are left to
/// `duplicate-target-missing`.
fn duplicate_chains(records: &[Reference]) -> Vec<Finding> {
    let by_key: BTreeMap<&str, &Reference> = records.iter().map(|r| (r.key.as_str(), r)).collect();
    let next = |k: &str| by_key.get(k).and_then(|r| r.duplicate_of.as_deref());
    let mut out = Vec::new();
    let mut cycles: BTreeSet<Vec<&str>> = BTreeSet::new();
    let mut members: Vec<&Reference> = records.iter().collect();
    members.sort_by(|a, b| key_order(&a.key, &b.key));
    for r in members {
        let Some(target) = r.duplicate_of.as_deref() else {
            continue;
        };
        // The path from `r` until it ends, leaves the registry, or repeats.
        let mut path = vec![r.key.as_str()];
        let mut at = target;
        let repeat = loop {
            if let Some(i) = path.iter().position(|k| *k == at) {
                break Some(i);
            }
            if !by_key.contains_key(at) {
                break None;
            }
            path.push(at);
            match next(at) {
                Some(n) => at = n,
                None => break None,
            }
        };
        match repeat {
            Some(i) => {
                let mut cycle = path[i..].to_vec();
                let least = (0..cycle.len())
                    .min_by(|a, b| key_order(cycle[*a], cycle[*b]))
                    .unwrap_or(0);
                cycle.rotate_left(least);
                if i == 0 && cycles.insert(cycle.clone()) {
                    let mut shown = cycle.clone();
                    shown.push(cycle[0]);
                    out.push(with_source(
                        Finding::new(
                            "duplicate-cycle",
                            Severity::Error,
                            cycle[0].to_owned(),
                            format!(
                                "duplicate_of links form a cycle: {}; one key must be the survivor, with no duplicate_of",
                                shown.join(" → ")
                            ),
                        )
                        .data(json!({ "keys": cycle })),
                        by_key[cycle[0]],
                    ));
                }
            }
            // Only a chain that ends at a surviving entry: the end is named.
            None if path.len() > 2 && by_key.contains_key(at) => out.push(with_source(
                Finding::new(
                    "duplicate-chain",
                    Severity::Warn,
                    r.key.clone(),
                    format!(
                        "duplicate_of names {target}, which is itself a duplicate: {}; point it at {at}",
                        path.join(" → ")
                    ),
                ),
                r,
            )),
            None => {}
        }
    }
    out
}

fn valid_iso_date(s: &str) -> bool {
    s.len() == 10 && s.parse::<jiff::civil::Date>().is_ok()
}

fn with_source(f: Finding, r: &Reference) -> Finding {
    match &r.source {
        Some(s) => f.evidence(&s.path, format!("line {}", s.line)),
        None => f,
    }
}

/// One finding per group of keys sharing a value. A group whose members all
/// point (via `duplicate_of`) at one surviving member is already annotated: info.
fn duplicate_finding(rule: &'static str, what: &str, value: &str, group: &[&Reference]) -> Finding {
    let mut members: Vec<&Reference> = group.to_vec();
    members.sort_by(|a, b| key_order(&a.key, &b.key));
    let keys: Vec<&str> = members.iter().map(|r| r.key.as_str()).collect();
    let survivors: Vec<&&Reference> = members
        .iter()
        .filter(|r| r.duplicate_of.is_none())
        .collect();
    let annotated = survivors.len() == 1
        && members
            .iter()
            .filter(|r| r.duplicate_of.is_some())
            .all(|r| r.duplicate_of.as_deref() == Some(survivors[0].key.as_str()));
    let (severity, message) = if annotated {
        (
            Severity::Info,
            format!(
                "keys {} share {what} {value}; annotated as duplicates of {}",
                keys.join(", "),
                survivors[0].key
            ),
        )
    } else {
        (
            Severity::Warn,
            format!("keys {} share {what} {value}", keys.join(", ")),
        )
    };
    let mut f = Finding::new(rule, severity, keys[0].to_owned(), message).data(json!({
        "keys": keys,
        "value": value,
        "annotated": annotated,
    }));
    for r in &members {
        if let Some(s) = &r.source {
            f = f.evidence(&s.path, format!("line {} (key {})", s.line, r.key));
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Source;

    fn rec(key: &str) -> Reference {
        let mut r = Reference::new(key.to_owned());
        r.source = Some(Source {
            path: "refs.md".into(),
            line: key.parse().unwrap_or(1) * 10,
        });
        r
    }

    fn rules(findings: &[Finding]) -> Vec<(&str, Severity, &str)> {
        findings
            .iter()
            .map(|f| (f.rule, f.severity, f.subject.as_str()))
            .collect()
    }

    #[test]
    fn ids_in_urls_and_arxiv_dois_are_compared() {
        let mut x = rec("1");
        x.ids.arxiv = Some("2601.00002".into());
        let mut y = rec("2");
        y.ids.url = Some("https://arxiv.org/abs/2601.00002v2".into());
        let mut z = rec("3");
        z.ids.doi = Some("10.48550/arXiv.2601.00002".into());
        let mut p = rec("4");
        p.ids.doi = Some("10.5555/w9".into());
        let mut q = rec("5");
        q.ids.url = Some("https://doi.org/10.5555/W9".into());
        let found = check(&[x, y, z, p, q]);
        let subjects = |rule: &str| -> Vec<&str> {
            found
                .iter()
                .filter(|f| f.rule == rule)
                .map(|f| f.subject.as_str())
                .collect()
        };
        assert_eq!(subjects("duplicate-arxiv").len(), 1, "{found:?}");
        assert!(
            found
                .iter()
                .any(|f| f.rule == "duplicate-arxiv" && f.message.contains("1, 2, 3")),
            "{found:?}"
        );
        assert_eq!(subjects("duplicate-doi"), ["4"], "{found:?}");
        assert!(subjects("duplicate-url").is_empty(), "{found:?}");
    }

    #[test]
    fn flags_shared_identifiers() {
        let mut a = rec("1");
        a.ids.doi = Some("10.1234/ABC".into());
        a.ids.url = Some("https://www.example.org/paper/".into());
        let mut b = rec("2");
        b.ids.doi = Some("10.1234/abc".into());
        b.ids.url = Some("http://example.org/paper#top".into());
        let mut c = rec("3");
        c.ids.arxiv = Some("2601.00001".into());
        c.version = Some("v1".into());
        c.title = Some("Widgets, Revisited".into());
        c.authors = Some(vec!["Ann Lee".into()]);
        let mut d = rec("4");
        d.ids.arxiv = Some("2601.00001".into());
        d.version = Some("v3".into());
        d.title = Some("widgets revisited".into());
        d.authors = Some(vec!["A. Lee".into()]);
        let mut e = rec("5");
        e.ids.url = Some("https://arxiv.org/abs/2601.00001".into());
        let f = rec("6");
        let found = check(&[a, b, c, d, e, f]);
        let got = rules(&found);
        assert!(
            got.contains(&("duplicate-doi", Severity::Warn, "1")),
            "{got:?}"
        );
        assert!(
            got.contains(&("duplicate-url", Severity::Warn, "1")),
            "{got:?}"
        );
        assert!(
            got.contains(&("duplicate-arxiv", Severity::Warn, "3")),
            "{got:?}"
        );
        assert!(
            got.contains(&("duplicate-title", Severity::Warn, "3")),
            "{got:?}"
        );
        assert!(
            got.contains(&("no-identifier", Severity::Warn, "6")),
            "{got:?}"
        );
        // arXiv landing pages are left to the arXiv rule.
        assert_eq!(got.iter().filter(|g| g.0 == "duplicate-url").count(), 1);
        let doi = found.iter().find(|f| f.rule == "duplicate-doi").unwrap();
        assert_eq!(doi.data.as_ref().unwrap()["keys"], json!(["1", "2"]));
        assert_eq!(doi.evidence.len(), 2);
    }

    #[test]
    fn annotated_duplicates_are_info() {
        let mut a = rec("177");
        a.ids.arxiv = Some("2601.00001".into());
        let mut b = rec("679");
        b.ids.arxiv = Some("2601.00001".into());
        b.duplicate_of = Some("177".into());
        let mut c = rec("700");
        c.duplicate_of = Some("999".into());
        c.ids.url = Some("https://example.org/x".into());
        c.accessed = Some("2026-02-30".into());
        c.depth = Some("skimmed".into());
        let got = check(&[a, b, c]);
        let got = rules(&got);
        assert!(
            got.contains(&("duplicate-arxiv", Severity::Info, "177")),
            "{got:?}"
        );
        assert!(got.contains(&("duplicate-target-missing", Severity::Error, "700")));
        assert!(got.contains(&("malformed-date", Severity::Error, "700")));
        assert!(got.contains(&("invalid-value", Severity::Error, "700")));
    }

    #[test]
    fn stray_doi_ends_are_reported() {
        let mut a = rec("1");
        a.ids.doi = Some("10.18653/v1/2026.acl-long.923`".into());
        let mut b = rec("2");
        b.ids.doi = Some("not a doi".into());
        let mut c = rec("3");
        c.ids.doi = Some("10.5555/0304-405x(90)90011-x".into());
        let mut d = rec("4");
        d.ids.doi = Some("10.1002/(sici)1097-0258(1996)".into());
        let got = check(&[a, b, c, d]);
        let got: Vec<_> = rules(&got)
            .into_iter()
            .filter(|g| g.0 == "malformed-doi")
            .collect();
        assert_eq!(
            got,
            [
                ("malformed-doi", Severity::Warn, "1"),
                ("malformed-doi", Severity::Warn, "2")
            ]
        );
    }

    #[test]
    fn duplicate_cycles_and_chains_are_reported() {
        let link = |key: &str, to: Option<&str>| {
            let mut r = rec(key);
            r.ids.url = Some(format!("https://example.org/{key}"));
            r.duplicate_of = to.map(str::to_owned);
            r
        };
        let records = [
            link("1", Some("2")),
            link("2", Some("3")),
            link("3", Some("1")),
            link("4", Some("1")),
            link("5", Some("5")),
            link("6", None),
            link("7", Some("8")),
            link("8", Some("6")),
            link("9", Some("6")),
        ];
        let found = check(&records);
        let got = rules(&found);
        let cycles: Vec<_> = got.iter().filter(|g| g.0 == "duplicate-cycle").collect();
        assert_eq!(
            cycles,
            [
                &("duplicate-cycle", Severity::Error, "1"),
                &("duplicate-cycle", Severity::Error, "5")
            ],
            "each cycle once, at its least key; 4 leads into one"
        );
        let cycle = found.iter().find(|f| f.rule == "duplicate-cycle").unwrap();
        assert!(cycle.message.contains("1 → 2 → 3 → 1"), "{}", cycle.message);
        let chains: Vec<_> = found
            .iter()
            .filter(|f| f.rule == "duplicate-chain")
            .collect();
        assert_eq!(chains.len(), 1, "{got:?}");
        assert_eq!(chains[0].subject, "7");
        assert!(
            chains[0].message.contains("7 → 8 → 6; point it at 6"),
            "{}",
            chains[0].message
        );
    }

    #[test]
    fn recorded_verification_is_checked() {
        let verified = |key: &str, at: &str, status: &str| {
            let mut r = rec(key);
            r.ids.url = Some(format!("https://example.org/{key}"));
            r.verified = Some(crate::registry::Verified {
                at: at.into(),
                against: "crossref".into(),
                status: status.into(),
            });
            r
        };
        let records = [
            verified("1", "2026-11-12", "match"),
            verified("2", "2026-11-12", "mismatch"),
            verified("3", "2026-11-12", "not-found"),
            verified("4", "2026-11-12", "checked"),
            verified("5", "12 November", "match"),
        ];
        let found = check(&records);
        let got = rules(&found);
        assert_eq!(
            got,
            [
                ("verified-mismatch", Severity::Warn, "2"),
                ("verified-mismatch", Severity::Warn, "3"),
                ("invalid-value", Severity::Error, "4"),
                ("malformed-date", Severity::Error, "5"),
            ]
        );
    }
}
