//! `papers verify`: compares registry entries or paper notes with arXiv,
//! Crossref, and OpenAlex, and reports differences as `rf.finding/1`.
//!
//! | Rule | Severity | Meaning |
//! | --- | --- | --- |
//! | `verify-title` | error | no source title equals ours (case, accents, punctuation ignored) |
//! | `verify-first-author` | error | our first author lacks the source's first surname |
//! | `verify-author-count` | error | author counts differ (skipped for "et al." lists) |
//! | `verify-year` | error | notes: not the arXiv v1 year, or after the earliest Crossref/OpenAlex date; registry: outside the years the source reports |
//! | `verify-venue` | warn | our venue does not name the source's journal (journal articles only) |
//! | `verify-withdrawn` | warn | arXiv withdrew a version (OAI-PMH version history), or the arXiv comment says the paper is withdrawn |
//! | `verify-retracted` | warn | Crossref lists a retraction, withdrawal, or removal notice for the DOI (`updated-by`) |
//! | `verify-notice` | warn | the DOI is itself a notice (`update-to`), such as a retraction; cite the work it changes |
//! | `verify-updated` | info | Crossref lists another notice for the DOI, such as a correction or expression of concern |
//! | `verify-not-found` | error (warn for title searches) | the source has no record for our identifier |
//! | `verify-failed` | error | a lookup failed (network or API error), including the arXiv version history; the run exits 2 |
//! | `verify-manual` | info | no index lists the work; a note's `checked` date records a hand check |
//! | `verify-skipped` | info | nothing to look up: a non-paper without DOI or arXiv id, or a duplicate |
//! | `frontmatter-invalid` | error | a note's frontmatter does not parse |

use serde_json::json;

use crate::arxiv::{self, Client};
use crate::atom::Paper;
use crate::crossref::{Direction, Work};
use crate::finding::{Finding, Severity};
use crate::http::Http;
use crate::id::ArxivId;
use crate::notes::{Note, title_matches};
use crate::refs::{Doi, Ref};
use crate::registry::Reference;
use crate::text::{self, surname, venue_key};
use crate::work::Record;
use crate::{crossref, openalex};

/// Title-search results scanned for an exact title match.
const SEARCH_RESULTS: usize = 5;

/// What one entry claims, from either a registry record or a note.
#[derive(Debug, Clone)]
pub struct Subject {
    pub key: String,
    /// The title first, then earlier titles that also count as a match.
    pub titles: Vec<String>,
    pub authors: Option<Vec<String>>,
    pub authors_truncated: bool,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub kind: Option<String>,
    pub doi: Option<Doi>,
    /// Versionless arXiv id.
    pub arxiv: Option<String>,
    /// The version the registry pins, such as `v1`.
    pub version: Option<String>,
    /// Other spellings of the first author that a source may use.
    pub author_variants: Vec<String>,
    /// A note's hand-check date.
    pub checked: Option<String>,
    pub duplicate_of: Option<String>,
    /// Why the entry cannot be read (bad frontmatter).
    pub invalid: Option<String>,
    pub evidence: Option<(String, String)>,
    /// Notes record the first public version's year; registry years may be
    /// any year the source reports.
    pub strict_year: bool,
}

impl Subject {
    pub fn from_reference(r: &Reference) -> Subject {
        Subject {
            key: r.key.clone(),
            titles: r.title.iter().cloned().collect(),
            authors: r.authors.clone(),
            authors_truncated: r.authors_truncated,
            year: r.year,
            venue: r.venue.clone(),
            kind: r.kind.clone(),
            doi: r.ids.doi.as_deref().and_then(Doi::parse),
            // An arXiv DOI is checked against arXiv itself.
            arxiv: r.ids.arxiv.clone().or_else(|| {
                let doi = Doi::parse(r.ids.doi.as_deref()?)?;
                Some(doi.arxiv()?.id)
            }),
            version: r.version.clone(),
            author_variants: r.author_variants.clone(),
            checked: None,
            duplicate_of: r.duplicate_of.clone(),
            invalid: None,
            evidence: r
                .source
                .as_ref()
                .map(|s| (s.path.clone(), format!("line {}", s.line))),
            strict_year: false,
        }
    }

    pub fn from_note(note: &Note, path: &str) -> Subject {
        let mut s = Subject {
            key: note.key(),
            titles: Vec::new(),
            authors: Some(Vec::new()),
            authors_truncated: false,
            year: None,
            venue: None,
            kind: Some("paper".to_owned()),
            doi: None,
            arxiv: None,
            version: None,
            author_variants: vec![],
            checked: None,
            duplicate_of: None,
            invalid: None,
            evidence: Some((path.to_owned(), "frontmatter".to_owned())),
            strict_year: true,
        };
        let meta = match &note.meta {
            Ok(meta) => meta,
            Err(e) => {
                s.invalid = Some(e.clone());
                return s;
            }
        };
        s.titles = meta
            .title
            .iter()
            .chain(&meta.former_titles)
            .cloned()
            .collect();
        s.authors = Some(meta.authors.clone());
        s.year = meta.year;
        s.venue = meta.venue.clone();
        s.author_variants = meta.author_variants.clone();
        s.checked = meta.checked.clone();
        match note.reference() {
            Some(Ref::Arxiv(id)) => s.arxiv = Some(id.id),
            Some(Ref::Doi(doi)) => s.doi = Some(doi),
            None => {}
        }
        s
    }

    fn has_title(&self, candidate: &str) -> bool {
        self.titles.iter().any(|t| title_matches(t, candidate))
    }

    fn title(&self) -> &str {
        self.titles.first().map_or("", String::as_str)
    }
}

/// One difference between an entry and a source.
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub rule: &'static str,
    pub severity: Severity,
    pub message: String,
}

fn diff(rule: &'static str, message: String) -> Diff {
    let severity = if rule == "verify-venue" {
        Severity::Warn
    } else {
        Severity::Error
    };
    Diff {
        rule,
        severity,
        message,
    }
}

/// Title and author checks shared by every source.
fn compare_people(
    s: &Subject,
    src: &str,
    titles: &[String],
    authors: &[String],
    family: Option<&str>,
) -> Vec<Diff> {
    let mut diffs = Vec::new();
    if !s.titles.is_empty() && !titles.iter().any(|t| s.has_title(t)) {
        diffs.push(diff(
            "verify-title",
            format!("title: ours {:?} vs {src} {:?}", s.title(), titles[0]),
        ));
    }
    let Some(ours) = &s.authors else {
        return diffs;
    };
    let first = ours.first().map_or("", String::as_str);
    // Variants stand in for a misspelt index name, never for a missing author.
    let variants: &[String] = if ours.is_empty() {
        &[]
    } else {
        &s.author_variants
    };
    if let Some(family) = family
        && !std::iter::once(first)
            .chain(variants.iter().map(String::as_str))
            .any(|name| text::has_surname(name, family))
    {
        let tried = if variants.is_empty() {
            String::new()
        } else {
            format!(" or author_variants {variants:?}")
        };
        diffs.push(diff(
            "verify-first-author",
            format!(
                "first author: ours {first:?}{tried} lacks {src} surname {family:?} ({})",
                authors.first().map_or("", String::as_str)
            ),
        ));
    }
    if !s.authors_truncated && ours.len() != authors.len() {
        diffs.push(diff(
            "verify-author-count",
            format!(
                "authors: ours {} vs {src} {} ({})",
                ours.len(),
                authors.len(),
                authors.join(", ")
            ),
        ));
    }
    diffs
}

fn year_text(year: Option<i64>) -> String {
    year.map_or_else(|| "missing".to_owned(), |y| y.to_string())
}

/// Differences from an arXiv record. Notes must give the v1 year; registry
/// years may be any year from v1 to the latest version.
pub fn compare_paper(s: &Subject, p: &Paper) -> Vec<Diff> {
    let family = p.authors.first().map(|a| surname(a));
    let mut diffs = compare_people(
        s,
        "arXiv",
        std::slice::from_ref(&p.title),
        &p.authors,
        family,
    );
    let v1: Option<i64> = p.published.get(..4).and_then(|y| y.parse().ok());
    let latest: Option<i64> = p.updated.get(..4).and_then(|y| y.parse().ok());
    let wrong = match (s.year, s.strict_year) {
        (None, strict) => strict,
        (Some(y), true) => Some(y) != v1,
        (Some(y), false) => v1.is_some_and(|v| y < v) || latest.is_some_and(|l| y > l),
    };
    if wrong {
        let expected = if s.strict_year {
            format!("arXiv v1 {} ({})", year_text(v1), p.published)
        } else {
            format!(
                "arXiv versions {} to {} ({} to {})",
                year_text(v1),
                year_text(latest),
                p.published,
                p.updated
            )
        };
        diffs.push(diff(
            "verify-year",
            format!("year: ours {} vs {expected}", year_text(s.year)),
        ));
    }
    diffs
}

/// Differences from a Crossref or OpenAlex record. Notes may give a year before
/// the source's earliest (the first public version can predate publication);
/// registry years must fall within the years the source reports.
pub fn compare_record(s: &Subject, rec: &Record) -> Vec<Diff> {
    let src = rec.source;
    let mut diffs = compare_people(
        s,
        src,
        &rec.titles,
        &rec.authors,
        rec.first_surname.as_deref(),
    );
    let wrong = match (s.year, rec.earliest_year, s.strict_year) {
        (_, None, _) => false,
        (None, Some(_), strict) => strict,
        (Some(y), Some(earliest), true) => y > earliest,
        (Some(y), Some(earliest), false) => y < earliest || rec.latest_year.is_some_and(|l| y > l),
    };
    if wrong {
        let relation = if s.strict_year {
            format!("is after {src}'s earliest {}", year_text(rec.earliest_year))
        } else {
            format!(
                "is outside {src}'s {} to {}",
                year_text(rec.earliest_year),
                year_text(rec.latest_year)
            )
        };
        diffs.push(diff(
            "verify-year",
            format!(
                "year: ours {} {relation} ({})",
                year_text(s.year),
                rec.year_basis
            ),
        ));
    }
    if let Some(journal) = &rec.journal {
        let venue = s.venue.as_deref().unwrap_or_default();
        if !venue_key(venue).contains(&venue_key(journal)) {
            diffs.push(diff(
                "verify-venue",
                format!("venue: ours {venue:?} does not name {src} journal {journal:?}"),
            ));
        }
    }
    diffs
}

/// Withdrawn versions of an arXiv paper. Only a paper with more than one
/// version can have one (withdrawing adds a version), so a v1-only paper costs
/// no lookup.
fn withdrawals(http: &Http, oai: &str, s: &Subject, p: &Paper) -> Vec<Diff> {
    let pinned = s
        .version
        .as_deref()
        .map(|v| format!("; ours pins {v}"))
        .unwrap_or_default();
    let mut notices = Vec::new();
    if p.version > 1 {
        match arxiv::versions(http, oai, &p.id) {
            Ok(versions) => {
                for v in versions.iter().flatten().filter(|v| v.withdrawn) {
                    notices.push(Diff {
                        rule: "verify-withdrawn",
                        severity: Severity::Warn,
                        message: format!("arXiv v{} withdrawn {}{pinned}", v.version, v.date),
                    });
                }
            }
            Err(message) => notices.push(Diff {
                rule: "verify-failed",
                severity: Severity::Error,
                message: format!("arXiv version history: {message}"),
            }),
        }
    }
    let comment = p.comment.as_deref().unwrap_or_default();
    if notices.is_empty() && comment.to_lowercase().contains("withdrawn") {
        notices.push(Diff {
            rule: "verify-withdrawn",
            severity: Severity::Warn,
            message: format!("arXiv v{} comment: {comment:?}{pinned}", p.version),
        });
    }
    notices
}

/// Retraction and correction notices Crossref links to a work.
fn update_notices(w: &Work) -> Vec<Diff> {
    w.updates
        .iter()
        .map(|u| {
            let doi = u
                .doi
                .as_deref()
                .map_or_else(String::new, |d| format!(" doi:{d}"));
            let when = u
                .date
                .as_deref()
                .map_or_else(String::new, |d| format!(" {d}"));
            let source = u
                .source
                .as_deref()
                .map_or_else(String::new, |s| format!(" (source {s})"));
            let kind = u.kind.replace('_', " ");
            let (rule, severity, message) = match u.direction {
                Direction::UpdateTo => (
                    "verify-notice",
                    Severity::Warn,
                    format!("this DOI is a {kind} notice{when} for{doi}; cite that work{source}"),
                ),
                Direction::UpdatedBy if u.is_retraction() => (
                    "verify-retracted",
                    Severity::Warn,
                    format!("{kind}{when} by notice{doi}{source}"),
                ),
                Direction::UpdatedBy => (
                    "verify-updated",
                    Severity::Info,
                    format!("{kind}{when} by notice{doi}{source}"),
                ),
            };
            Diff {
                rule,
                severity,
                message,
            }
        })
        .collect()
}

/// The result of checking one entry.
#[derive(Debug)]
pub enum Outcome {
    Checked {
        against: &'static str,
        used: String,
        /// Metadata differences: they decide `match` or `mismatch`.
        diffs: Vec<Diff>,
        /// Withdrawal, retraction, and correction notices, and a failed
        /// version-history lookup. They leave the metadata status alone.
        notices: Vec<Diff>,
    },
    NotFound {
        against: &'static str,
        used: String,
        message: String,
        by_title: bool,
    },
    Failed {
        against: &'static str,
        used: String,
        message: String,
    },
    Manual(String),
    Skipped(String),
    Invalid(String),
}

impl Outcome {
    /// Counting label for the summary.
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Checked { diffs, .. } if diffs.is_empty() => "match",
            Outcome::Checked { .. } => "mismatch",
            Outcome::NotFound { .. } => "not found",
            Outcome::Failed { .. } => "failed",
            Outcome::Manual(_) => "manual",
            Outcome::Skipped(_) => "skipped",
            Outcome::Invalid(_) => "invalid",
        }
    }

    /// A lookup failed, so the run exits 2.
    pub fn failed(&self) -> bool {
        match self {
            Outcome::Failed { .. } => true,
            Outcome::Checked { notices, .. } => notices.iter().any(|n| n.rule == "verify-failed"),
            _ => false,
        }
    }

    /// `(against, status)` for the registry's `verified` field.
    pub fn verified(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Outcome::Checked { against, diffs, .. } => Some((
                against,
                if diffs.is_empty() {
                    "match"
                } else {
                    "mismatch"
                },
            )),
            Outcome::NotFound { against, .. } => Some((against, "not-found")),
            _ => None,
        }
    }

    pub fn findings(&self, s: &Subject) -> Vec<Finding> {
        let one = |rule, severity, message: String, data| {
            let mut f = Finding::new(rule, severity, s.key.clone(), message);
            if let Some((path, locator)) = &s.evidence {
                f = f.evidence(path, locator.clone());
            }
            match data {
                Some(d) => f.data(d),
                None => f,
            }
        };
        match self {
            Outcome::Checked {
                against,
                used,
                diffs,
                notices,
            } => diffs
                .iter()
                .chain(notices)
                .map(|d| {
                    one(
                        d.rule,
                        d.severity,
                        format!("{} ({against} {used})", d.message),
                        Some(json!({"against": against, "used": used})),
                    )
                })
                .collect(),
            Outcome::NotFound {
                against,
                used,
                message,
                by_title,
            } => vec![one(
                "verify-not-found",
                if *by_title {
                    Severity::Warn
                } else {
                    Severity::Error
                },
                format!("{message} ({against} {used})"),
                Some(json!({"against": against, "used": used})),
            )],
            Outcome::Failed {
                against,
                used,
                message,
            } => vec![one(
                "verify-failed",
                Severity::Error,
                format!("{message} ({against} {used})"),
                Some(json!({"against": against, "used": used})),
            )],
            Outcome::Manual(m) => vec![one("verify-manual", Severity::Info, m.clone(), None)],
            Outcome::Skipped(m) => vec![one("verify-skipped", Severity::Info, m.clone(), None)],
            Outcome::Invalid(m) => {
                vec![one("frontmatter-invalid", Severity::Error, m.clone(), None)]
            }
        }
    }
}

/// Checks every subject; arXiv ids are looked up in batches first.
pub fn run(http: &Http, subjects: &[Subject]) -> Vec<Outcome> {
    run_with(http, arxiv::OAI_ENDPOINT, subjects)
}

/// [`run`] with arXiv's OAI-PMH endpoint given, for tests.
fn run_with(http: &Http, oai: &str, subjects: &[Subject]) -> Vec<Outcome> {
    let wanted = arxiv_wanted(subjects);
    let mut found = if wanted.is_empty() {
        Vec::new()
    } else {
        Client::new(http).fetch(&wanted)
    }
    .into_iter();
    let total = subjects.len();
    subjects
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i > 0 && i % 25 == 0 {
                eprintln!("papers: verify {i}/{total}");
            }
            check(http, oai, s, &mut found)
        })
        .collect()
}

/// The arXiv ids [`check`] will consume, in subject order.
fn arxiv_wanted(subjects: &[Subject]) -> Vec<ArxivId> {
    subjects
        .iter()
        .filter(|s| precheck(s).is_none())
        .filter_map(|s| s.arxiv.as_deref())
        .filter_map(|id| ArxivId::parse(id).ok())
        .collect()
}

/// The outcome for a subject that needs no lookup. Without a title or authors
/// there is nothing to compare: a lookup could only "match" whatever record the
/// identifier resolves to, even a wrong one.
fn precheck(s: &Subject) -> Option<Outcome> {
    if let Some(e) = &s.invalid {
        return Some(Outcome::Invalid(e.clone()));
    }
    if let Some(target) = &s.duplicate_of {
        return Some(Outcome::Skipped(format!(
            "duplicate of {target}; verify that entry"
        )));
    }
    if s.titles.is_empty() && s.authors.as_ref().is_none_or(Vec::is_empty) {
        return Some(Outcome::Skipped(
            "no title or authors to compare; cannot verify".to_owned(),
        ));
    }
    None
}

fn check(
    http: &Http,
    oai: &str,
    s: &Subject,
    arxiv_results: &mut impl Iterator<Item = Result<Paper, String>>,
) -> Outcome {
    if let Some(outcome) = precheck(s) {
        return outcome;
    }
    if let Some(id) = s.arxiv.as_deref().filter(|id| ArxivId::parse(id).is_ok()) {
        let used = id.to_owned();
        return match arxiv_results.next().expect("one result per arXiv id") {
            Ok(paper) => Outcome::Checked {
                against: "arXiv",
                used,
                diffs: compare_paper(s, &paper),
                notices: withdrawals(http, oai, s, &paper),
            },
            Err(message) if message.starts_with("not found") => Outcome::NotFound {
                against: "arXiv",
                used,
                message,
                by_title: false,
            },
            Err(message) => Outcome::Failed {
                against: "arXiv",
                used,
                message,
            },
        };
    }
    if let Some(doi) = &s.doi {
        return check_doi(http, s, doi, "");
    }
    if let Some(date) = &s.checked {
        return Outcome::Manual(format!(
            "no id to check against; metadata confirmed by hand on {date}"
        ));
    }
    match s.kind.as_deref() {
        None | Some("paper") if !s.titles.is_empty() => check_by_title(http, s),
        None | Some("paper") => Outcome::Skipped("no DOI, arXiv id, or title".to_owned()),
        Some(kind) => Outcome::Skipped(format!(
            "type {kind} with no DOI or arXiv id; nothing to look up"
        )),
    }
}

fn check_doi(http: &Http, s: &Subject, doi: &Doi, via: &str) -> Outcome {
    let used = format!("{via}doi:{doi}");
    match crossref::fetch(http, doi) {
        Ok(Some(w)) => {
            return Outcome::Checked {
                against: "Crossref",
                diffs: compare_record(s, &(&w).into()),
                notices: update_notices(&w),
                used,
            };
        }
        Ok(None) => {}
        Err(message) => {
            return Outcome::Failed {
                against: "Crossref",
                used,
                message,
            };
        }
    }
    match openalex::Client::new(http).by_doi(doi) {
        Ok(Some(w)) => Outcome::Checked {
            against: "OpenAlex",
            diffs: compare_record(s, &(&w).into()),
            notices: vec![],
            used,
        },
        Ok(None) => Outcome::NotFound {
            against: "OpenAlex",
            used,
            message: "DOI not found at Crossref or OpenAlex".to_owned(),
            by_title: false,
        },
        Err(message) => Outcome::Failed {
            against: "OpenAlex",
            used,
            message,
        },
    }
}

/// Finds an entry with no id by exact (normalized) title on OpenAlex.
fn check_by_title(http: &Http, s: &Subject) -> Outcome {
    let used = "title search".to_owned();
    let results = match openalex::Client::new(http).search_title(s.title(), SEARCH_RESULTS) {
        Ok(results) => results,
        Err(message) => {
            return Outcome::Failed {
                against: "OpenAlex",
                used,
                message,
            };
        }
    };
    let Some(hit) = results.iter().find(|w| s.has_title(&w.title)) else {
        let closest = results
            .first()
            .map(|w| {
                format!(
                    "; closest: {:?} ({}, similarity {:.2})",
                    w.title,
                    w.id,
                    text::similarity(s.title(), &w.title)
                )
            })
            .unwrap_or_default();
        return Outcome::NotFound {
            against: "OpenAlex",
            used,
            message: format!("no OpenAlex work with this exact title{closest}"),
            by_title: true,
        };
    };
    match &hit.doi {
        Some(doi) => check_doi(http, s, doi, &format!("title search → {} → ", hit.id)),
        None => Outcome::Checked {
            against: "OpenAlex",
            diffs: compare_record(s, &hit.into()),
            notices: vec![],
            used: format!("title search → {}", hit.id),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(title: &str, authors: &[&str], year: i64, strict: bool) -> Subject {
        Subject {
            key: "k".into(),
            titles: vec![title.into()],
            authors: Some(authors.iter().map(|a| (*a).to_owned()).collect()),
            authors_truncated: false,
            year: Some(year),
            venue: None,
            kind: Some("paper".into()),
            doi: None,
            arxiv: None,
            version: None,
            author_variants: vec![],
            checked: None,
            duplicate_of: None,
            invalid: None,
            evidence: None,
            strict_year: strict,
        }
    }

    fn paper(title: &str, authors: &[&str]) -> Paper {
        Paper {
            id: "2208.06046".into(),
            version: 6,
            title: title.into(),
            authors: authors.iter().map(|a| (*a).to_owned()).collect(),
            published: "2022-08-11".into(),
            updated: "2026-08-20".into(),
            primary_category: "q-fin.MF".into(),
            categories: vec!["q-fin.MF".into()],
            journal_ref: None,
            doi: None,
            comment: None,
            abstract_text: String::new(),
            abs_url: "https://arxiv.org/abs/2208.06046".into(),
        }
    }

    fn rules(diffs: &[Diff]) -> Vec<&str> {
        diffs.iter().map(|d| d.rule).collect()
    }

    const AUTHORS: [&str; 4] = [
        "Mira Ellingwood",
        "Niles O. Barrow",
        "Opal Strand",
        "Pell Anders Kemp",
    ];

    #[test]
    fn arxiv_comparison_reports_each_difference() {
        let p = paper(
            "Synthetic Market Making and Rebalancing-Versus-Holding",
            &AUTHORS,
        );
        let good = subject(
            "Synthetic market making and rebalancing versus holding",
            &["M. Ellingwood", "N. Barrow", "O. Strand", "P. A. Kemp"],
            2022,
            true,
        );
        assert!(compare_paper(&good, &p).is_empty());
        let bad = subject("Synthetic Market Making", &["Niles Barrow"], 2026, true);
        let diffs = compare_paper(&bad, &p);
        assert_eq!(
            rules(&diffs),
            [
                "verify-title",
                "verify-first-author",
                "verify-author-count",
                "verify-year"
            ]
        );
        assert!(
            diffs[3].message.contains("ours 2026 vs arXiv v1 2022"),
            "{diffs:?}"
        );
    }

    #[test]
    fn first_author_needs_the_whole_surname() {
        let p = paper("Synthetic Widgets", &["Wei Li"]);
        let wrong = subject("Synthetic Widgets", &["William Chen"], 2022, true);
        assert_eq!(rules(&compare_paper(&wrong, &p)), ["verify-first-author"]);
        let wrong = subject("Synthetic Widgets", &["Lina Tanaka"], 2022, true);
        assert_eq!(rules(&compare_paper(&wrong, &p)), ["verify-first-author"]);
        for ours in ["Wei Li", "W. Li", "Li, Wei", "Li Wei"] {
            let s = subject("Synthetic Widgets", &[ours], 2022, true);
            assert!(compare_paper(&s, &p).is_empty(), "{ours}");
        }
    }

    #[test]
    fn entries_without_title_or_authors_are_not_verified() {
        let mut bare = subject("", &[], 2022, false);
        bare.titles.clear();
        bare.authors = None;
        bare.arxiv = Some("2601.00001".into());
        let outcome = check(&Http::new(), "", &bare, &mut std::iter::empty());
        assert_eq!(outcome.label(), "skipped");
        assert!(outcome.verified().is_none());
        let mut titled = subject("Synthetic Widgets", &[], 2022, false);
        titled.authors = None;
        titled.arxiv = Some("2601.00002".into());
        let ids: Vec<String> = arxiv_wanted(&[bare, titled])
            .iter()
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(ids, ["2601.00002"], "skipped entries take no batch slot");
    }

    #[test]
    fn registry_years_may_be_any_version_year() {
        let p = paper(
            "Synthetic Market Making and Rebalancing-Versus-Holding",
            &AUTHORS,
        );
        let mut s = subject(&p.title, &AUTHORS, 2024, false);
        assert!(compare_paper(&s, &p).is_empty());
        s.year = Some(2021);
        assert_eq!(rules(&compare_paper(&s, &p)), ["verify-year"]);
        s.year = None;
        assert!(
            compare_paper(&s, &p).is_empty(),
            "an unknown year is not a difference"
        );
        s.year = Some(2022);
        s.authors = Some(vec!["Mira Ellingwood".into()]);
        s.authors_truncated = true;
        assert!(
            compare_paper(&s, &p).is_empty(),
            "et al. lists skip the count"
        );
        s.authors = None;
        assert!(
            compare_paper(&s, &p).is_empty(),
            "unknown authors are not checked"
        );
    }

    #[test]
    fn former_titles_also_match() {
        let p = paper(
            "Funding Payments Steadied Toy Perpetual Futures",
            &["Rue Talbot"],
        );
        let mut s = subject(
            "Perpetual Futures and Basis Risk",
            &["Rue Talbot"],
            2022,
            true,
        );
        assert_eq!(rules(&compare_paper(&s, &p)), ["verify-title"]);
        s.titles
            .push("Funding Payments Steadied: Toy Perpetual Futures".into());
        assert!(compare_paper(&s, &p).is_empty());
    }

    #[test]
    fn author_variants_accept_a_misspelt_index_name() {
        let p = paper("Synthetic Widgets", &["Ann Leeb", "Bo Chan"]);
        let mut s = subject("Synthetic Widgets", &["Ann Leebo", "Bo Chan"], 2022, true);
        assert_eq!(rules(&compare_paper(&s, &p)), ["verify-first-author"]);
        s.author_variants.push("Ann Leeb".into());
        assert!(compare_paper(&s, &p).is_empty());
        s.author_variants = vec!["Ann Lee".into()];
        let diffs = compare_paper(&s, &p);
        assert_eq!(rules(&diffs), ["verify-first-author"]);
        assert!(diffs[0].message.contains("author_variants"), "{diffs:?}");
        // A variant never stands in for a missing author list.
        s.authors = Some(vec![]);
        s.author_variants = vec!["Ann Leeb".into()];
        assert!(rules(&compare_paper(&s, &p)).contains(&"verify-first-author"));
    }

    #[test]
    fn author_variants_reach_verify_from_notes_and_the_registry() {
        let note = Note {
            path: "research/papers/2026-leebo-widgets.md".into(),
            meta: crate::notes::parse_meta(
                "---\ntitle: Synthetic Widgets\nauthors: [Ann Leebo]\nauthor_variants: [Ann Leeb]\nurl: https://arxiv.org/abs/2601.00001\n---\n",
            ),
        };
        let from_note = Subject::from_note(&note, "research/papers/2026-leebo-widgets.md");
        assert_eq!(from_note.author_variants, ["Ann Leeb"]);
        let reference =
            note.to_reference("research/papers/2026-leebo-widgets.md", &Default::default());
        let line = serde_json::to_string(&reference).unwrap();
        let back: Reference = serde_json::from_str(&line).unwrap();
        assert_eq!(Subject::from_reference(&back).author_variants, ["Ann Leeb"]);
        let mut plain = reference.clone();
        plain.author_variants.clear();
        assert!(
            !serde_json::to_string(&plain)
                .unwrap()
                .contains("author_variants")
        );
    }

    #[test]
    fn scalar_list_fields_parse_and_verify() {
        let note = Note {
            path: "research/papers/2026-leebo-widgets.md".into(),
            meta: crate::notes::parse_meta(
                "---\ntitle: Synthetic Widgets\nformer_titles: Widgets, Synthetically\nauthors: [Ann Leebo, Bo Chan]\nyear: 2022\nauthor_variants: Ann Leeb\nurl: https://arxiv.org/abs/2601.00001\n---\n",
            ),
        };
        let s = Subject::from_note(&note, "research/papers/2026-leebo-widgets.md");
        assert_eq!(s.author_variants, ["Ann Leeb"]);
        assert!(s.titles.iter().any(|t| t == "Widgets, Synthetically"));
        let p = paper("Synthetic Widgets", &["Ann Leeb", "Bo Chan"]);
        assert!(
            compare_paper(&s, &p).is_empty(),
            "{:?}",
            compare_paper(&s, &p)
        );
        // An empty field is no variants, not an invalid note.
        let empty = crate::notes::parse_meta("---\ntitle: T\nauthor_variants:\n---\n").unwrap();
        assert!(empty.author_variants.is_empty());
    }

    fn record(journal: Option<&str>, years: Option<(i64, i64)>) -> Record {
        Record {
            source: "Crossref",
            titles: vec![
                "Patience pays slowly".into(),
                "Patience pays slowly: limits to arbitrage".into(),
            ],
            authors: vec!["Gale Northcott".into(), "Hale Brennick".into()],
            first_surname: Some("Northcott".into()),
            earliest_year: years.map(|y| y.0),
            latest_year: years.map(|y| y.1),
            year_basis: "published-online 2024-02-16".into(),
            journal: journal.map(str::to_owned),
        }
    }

    fn trust(year: i64, venue: &str, strict: bool) -> Subject {
        Subject {
            venue: Some(venue.into()),
            ..subject(
                "Patience pays slowly: Limits to arbitrage",
                &["Gale Northcott", "Hale Brennick"],
                year,
                strict,
            )
        }
    }

    #[test]
    fn note_year_may_precede_the_source() {
        let rec = record(None, Some((2024, 2024)));
        assert!(compare_record(&trust(2018, "x", true), &rec).is_empty());
        assert!(compare_record(&trust(2024, "x", true), &rec).is_empty());
        let diffs = compare_record(&trust(2025, "x", true), &rec);
        assert_eq!(rules(&diffs), ["verify-year"]);
        assert!(
            diffs[0]
                .message
                .contains("ours 2025 is after Crossref's earliest 2024")
        );
        assert!(compare_record(&trust(2025, "x", true), &record(None, None)).is_empty());
    }

    #[test]
    fn registry_year_must_be_a_reported_year() {
        let rec = record(None, Some((2023, 2024)));
        assert!(compare_record(&trust(2023, "x", false), &rec).is_empty());
        assert!(compare_record(&trust(2024, "x", false), &rec).is_empty());
        assert_eq!(
            rules(&compare_record(&trust(2018, "x", false), &rec)),
            ["verify-year"]
        );
        assert_eq!(
            rules(&compare_record(&trust(2025, "x", false), &rec)),
            ["verify-year"]
        );
    }

    #[test]
    fn venue_must_name_the_journal() {
        let rec = record(Some("Review of Synthetic Finance"), Some((2024, 2024)));
        let ok = trust(
            2018,
            "Review of Synthetic Finance 28(4), 1345–1381 (2024)",
            true,
        );
        assert!(compare_record(&ok, &rec).is_empty());
        let diffs = compare_record(&trust(2018, "SSRN working paper", true), &rec);
        assert_eq!(rules(&diffs), ["verify-venue"]);
        assert_eq!(diffs[0].severity, Severity::Warn);
    }

    #[test]
    fn record_reports_title_and_authors() {
        let rec = record(None, Some((2024, 2024)));
        let bad = Subject {
            titles: vec!["Patience pays".into()],
            authors: Some(vec!["H. Brennick".into()]),
            ..trust(2018, "x", true)
        };
        let diffs = compare_record(&bad, &rec);
        assert_eq!(
            rules(&diffs),
            ["verify-title", "verify-first-author", "verify-author-count"]
        );
        assert!(diffs[1].message.contains("surname \"Northcott\""));
        assert!(
            diffs[2]
                .message
                .starts_with("authors: ours 1 vs Crossref 2")
        );
    }

    #[test]
    fn outcomes_become_findings() {
        let mut s = subject("T", &["A B"], 2020, false);
        s.evidence = Some(("refs.md".into(), "line 9".into()));
        let not_found = Outcome::NotFound {
            against: "arXiv",
            used: "2601.00001".into(),
            message: "not found".into(),
            by_title: false,
        };
        let f = not_found.findings(&s);
        assert_eq!(
            (f[0].rule, f[0].severity),
            ("verify-not-found", Severity::Error)
        );
        assert_eq!(f[0].evidence[0].locator, "line 9");
        assert_eq!(not_found.verified(), Some(("arXiv", "not-found")));
        let ok = Outcome::Checked {
            against: "Crossref",
            used: "doi:10.1/x".into(),
            diffs: vec![],
            notices: vec![],
        };
        assert!(ok.findings(&s).is_empty());
        assert_eq!(ok.verified(), Some(("Crossref", "match")));
        assert_eq!(Outcome::Skipped("x".into()).verified(), None);
    }

    fn served(body: &str) -> (String, std::thread::JoinHandle<Vec<String>>) {
        crate::http::serve(vec![crate::http::response(
            "200 OK",
            &["Content-Type: text/xml"],
            body,
        )])
    }

    #[test]
    fn withdrawn_arxiv_versions_are_notices() {
        let mut s = subject(
            "Synthetic Widgets Rewrite Their Own Queries",
            &["Ena Morrow", "Fitz Calder"],
            2026,
            false,
        );
        s.version = Some("v1".into());
        let mut p = paper(&s.titles[0], &["Ena Morrow", "Fitz Calder"]);
        p.id = "2601.99977".into();
        p.version = 2;
        p.published = "2026-01-06".into();
        p.updated = "2026-02-12".into();
        p.comment = Some("The paper is about to undergo significant revisions".into());
        let (base, server) = served(include_str!("../tests/fixtures/arxiv-oai-withdrawn.xml"));
        let http = Http::new();
        let notices = withdrawals(&http, &format!("{base}/oai"), &s, &p);
        let requests = server.join().unwrap();
        assert!(
            requests[0].contains("identifier=oai:arXiv.org:2601.99977&metadataPrefix=arXivRaw"),
            "{requests:?}"
        );
        assert_eq!(rules(&notices), ["verify-withdrawn"]);
        assert_eq!(notices[0].severity, Severity::Warn);
        assert_eq!(
            notices[0].message,
            "arXiv v2 withdrawn 2026-02-12; ours pins v1"
        );
        let outcome = Outcome::Checked {
            against: "arXiv",
            used: p.id.clone(),
            diffs: compare_paper(&s, &p),
            notices,
        };
        assert_eq!(
            outcome.label(),
            "match",
            "a withdrawal is not a metadata mismatch"
        );
        assert_eq!(outcome.verified(), Some(("arXiv", "match")));
        assert!(!outcome.failed());
        let f = outcome.findings(&s);
        assert_eq!(
            (f[0].rule, f[0].severity),
            ("verify-withdrawn", Severity::Warn)
        );

        // A v1-only paper needs no lookup: withdrawing always adds a version.
        p.version = 1;
        assert!(withdrawals(&http, "http://127.0.0.1:9/unused", &s, &p).is_empty());
        // An old-style withdrawal says so in the comment.
        p.comment = Some("This paper has been withdrawn by the author".into());
        let notices = withdrawals(&http, "http://127.0.0.1:9/unused", &s, &p);
        assert_eq!(rules(&notices), ["verify-withdrawn"]);
        assert!(
            notices[0].message.contains("withdrawn by the author"),
            "{notices:?}"
        );
    }

    #[test]
    fn failed_version_history_fails_the_run() {
        let s = subject("T", &["A B"], 2026, false);
        let mut p = paper("T", &["A B"]);
        p.version = 2;
        let bad = include_str!("../tests/fixtures/arxiv-oai-error.xml")
            .replace("idDoesNotExist", "badArgument");
        let (base, server) = served(&bad);
        let notices = withdrawals(&Http::new(), &format!("{base}/oai"), &s, &p);
        server.join().unwrap();
        assert_eq!(rules(&notices), ["verify-failed"]);
        let outcome = Outcome::Checked {
            against: "arXiv",
            used: "2208.06046".into(),
            diffs: vec![],
            notices,
        };
        assert!(outcome.failed());
        assert_eq!(outcome.verified(), Some(("arXiv", "match")));
    }

    #[test]
    fn crossref_retractions_and_notices_are_reported() {
        let retracted =
            crossref::parse_work(include_str!("../tests/fixtures/crossref-retracted.json"))
                .unwrap();
        let notices = update_notices(&retracted);
        assert_eq!(rules(&notices), ["verify-updated", "verify-retracted"]);
        assert_eq!(
            (notices[0].severity, notices[1].severity),
            (Severity::Info, Severity::Warn)
        );
        assert_eq!(
            notices[1].message,
            "retraction 2021-03-09 by notice doi:10.5555/synth.2019.0042.retr (source retraction-watch)"
        );
        let notice = crossref::parse_work(include_str!(
            "../tests/fixtures/crossref-retraction-notice.json"
        ))
        .unwrap();
        let notices = update_notices(&notice);
        assert_eq!(rules(&notices), ["verify-notice"]);
        assert!(
            notices[0]
                .message
                .starts_with("this DOI is a retraction notice 2021-03-09 for doi:10.5555/synth.2019.0042; cite that work"),
            "{notices:?}"
        );
        let journal =
            crossref::parse_work(include_str!("../tests/fixtures/crossref-journal.json")).unwrap();
        assert!(update_notices(&journal).is_empty());
    }
}
