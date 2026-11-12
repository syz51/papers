//! `papers registry build` and `papers registry check`.

use std::collections::{BTreeMap, BTreeSet};

use crate::catalogue::{self, Issue};
use crate::check;
use crate::config::Config;
use crate::derivative;
use crate::finding::{self, Finding, Format, Severity};
use crate::notes;
use crate::registry::{self, Reference};

/// Where registry records come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum From {
    /// The numbered Markdown catalogue (`[papers] catalogue` or `--catalogue`).
    Catalogue,
    /// The one-note-per-paper directory (`[papers] notes` or `--papers`).
    Notes,
}

/// Records parsed from the archive, with the problems met on the way.
pub struct Parsed {
    pub records: Vec<Reference>,
    pub issues: Vec<Issue>,
    /// The source as shown in findings and records.
    pub source: String,
}

/// The default source: the catalogue when one is configured, else the notes.
pub fn default_from(config: &Config) -> From {
    if config.catalogue.is_some() {
        From::Catalogue
    } else {
        From::Notes
    }
}

pub fn parse_source(config: &Config, from: From) -> Result<Parsed, String> {
    match from {
        From::Catalogue => {
            let path = config.catalogue.as_ref().ok_or(
                "no catalogue configured: set [papers] catalogue in research.toml or pass --catalogue",
            )?;
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let source = config.display(path);
            let parsed = catalogue::parse(&text, &source);
            Ok(Parsed {
                records: parsed.records,
                issues: parsed.issues,
                source,
            })
        }
        From::Notes => {
            let all = notes::load(&config.notes)?
                .ok_or_else(|| format!("{}: no such directory", config.notes.display()))?;
            let mut records = Vec::new();
            let mut issues = Vec::new();
            for note in &all {
                let path = config.display(&note.path);
                let record = note.to_reference(&path, &config.status_depth);
                match &note.meta {
                    Err(e) => issues.push(Issue {
                        rule: "frontmatter-invalid",
                        severity: Severity::Error,
                        key: record.key.clone(),
                        path: path.clone(),
                        line: 1,
                        message: e.clone(),
                    }),
                    Ok(meta) => {
                        if let Err(e) = meta.explicit_depth() {
                            issues.push(Issue {
                                rule: "depth-invalid",
                                severity: Severity::Error,
                                key: record.key.clone(),
                                path: path.clone(),
                                line: 1,
                                message: e,
                            });
                        }
                        if let Some(id) = meta.id.as_deref().filter(|id| *id != record.key) {
                            issues.push(Issue {
                                rule: "key-mismatch",
                                severity: Severity::Error,
                                key: record.key.clone(),
                                path: path.clone(),
                                line: 1,
                                message: format!(
                                    "the file is named {}.md but its id is {id:?}",
                                    record.key
                                ),
                            });
                        }
                    }
                }
                records.push(record);
            }
            registry::sort(&mut records);
            Ok(Parsed {
                records,
                issues,
                source: config.display(&config.notes),
            })
        }
    }
}

/// How many records have each field, for the build summary.
pub fn stats(records: &[Reference]) -> Vec<(&'static str, usize)> {
    let count = |f: &dyn Fn(&Reference) -> bool| records.iter().filter(|r| f(r)).count();
    vec![
        ("title", count(&|r| r.title.is_some())),
        ("authors", count(&|r| r.authors.is_some())),
        ("authors_truncated", count(&|r| r.authors_truncated)),
        ("year", count(&|r| r.year.is_some())),
        ("venue", count(&|r| r.venue.is_some())),
        ("type", count(&|r| r.kind.is_some())),
        ("ids.doi", count(&|r| r.ids.doi.is_some())),
        ("ids.arxiv", count(&|r| r.ids.arxiv.is_some())),
        ("version", count(&|r| r.version.is_some())),
        ("ids.url", count(&|r| r.ids.url.is_some())),
        ("ids.openalex", count(&|r| r.ids.openalex.is_some())),
        ("ids.dblp", count(&|r| r.ids.dblp.is_some())),
        ("accessed", count(&|r| r.accessed.is_some())),
        ("depth", count(&|r| r.depth.is_some())),
        ("duplicate_of", count(&|r| r.duplicate_of.is_some())),
        ("texts", count(&|r| !r.texts.is_empty())),
        ("verified", count(&|r| r.verified.is_some())),
    ]
}

fn print_stats(records: &[Reference], issues: &[Issue]) {
    let total = records.len();
    eprintln!("  {:<18} {:>6} {:>6}", "field", "known", "null");
    for (field, n) in stats(records) {
        eprintln!("  {field:<18} {n:>6} {:>6}", total - n);
    }
    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for i in issues {
        *by_rule.entry(i.rule).or_default() += 1;
    }
    if !by_rule.is_empty() {
        let list: Vec<String> = by_rule.iter().map(|(r, n)| format!("{r} {n}")).collect();
        let hint = if issues.iter().any(|i| i.severity == Severity::Error) {
            "errors are listed above; run papers registry check for the rest"
        } else {
            "run papers registry check for details"
        };
        eprintln!("  parse issues: {} ({hint})", list.join(", "));
    }
}

/// Parse issues after which an entry's main text (the frontmatter, or the
/// catalogue entry line) could not be read.
const UNREADABLE: &[&str] = &["frontmatter-invalid", "entry-missing"];

/// Fills each metadata field that `r` lacks from `prev`. Values the text still
/// gives, such as a catalogue section's labelled lines, win; `source` stays.
fn fill_from(r: &mut Reference, prev: &Reference) {
    fn fill<T: Clone>(field: &mut Option<T>, prev: &Option<T>) {
        if field.is_none() {
            field.clone_from(prev);
        }
    }
    fill(&mut r.kind, &prev.kind);
    fill(&mut r.title, &prev.title);
    if r.authors.is_none() {
        r.authors.clone_from(&prev.authors);
        r.authors_truncated = prev.authors_truncated;
    }
    if r.author_variants.is_empty() {
        r.author_variants.clone_from(&prev.author_variants);
    }
    fill(&mut r.year, &prev.year);
    fill(&mut r.venue, &prev.venue);
    fill(&mut r.ids.doi, &prev.ids.doi);
    if r.ids.arxiv.is_none() {
        r.ids.arxiv.clone_from(&prev.ids.arxiv);
        r.version.clone_from(&prev.version);
    }
    fill(&mut r.ids.url, &prev.ids.url);
    fill(&mut r.accessed, &prev.accessed);
    fill(&mut r.depth, &prev.depth);
    fill(&mut r.duplicate_of, &prev.duplicate_of);
}

/// Merges the parsed records into `previous`. An entry whose main text could
/// not be read keeps the recorded values the text no longer gives; for notes,
/// a recorded depth stays when the note gives none.
fn merge(from: From, parsed: &Parsed, previous: Vec<Reference>) -> (Vec<Reference>, Vec<String>) {
    let unreadable: BTreeSet<&str> = parsed
        .issues
        .iter()
        .filter(|i| UNREADABLE.contains(&i.rule))
        .map(|i| i.key.as_str())
        .collect();
    let mut fresh = parsed.records.clone();
    if !unreadable.is_empty() {
        let recorded: BTreeMap<&str, &Reference> =
            previous.iter().map(|p| (p.key.as_str(), p)).collect();
        for r in &mut fresh {
            if unreadable.contains(r.key.as_str())
                && let Some(prev) = recorded.get(r.key.as_str())
            {
                fill_from(r, prev);
            }
        }
    }
    if from == From::Notes {
        registry::keep_recorded_depth(&mut fresh, &previous);
    }
    registry::merge(fresh, previous)
}

/// Writes the registry even when parsing finds errors, so one bad entry does
/// not hold back the others; returns false when any error-severity issue was
/// found. An entry whose note has an invalid depth keeps its recorded depth.
pub fn build(config: &Config, from: From, dry_run: bool) -> Result<bool, String> {
    let parsed = parse_source(config, from)?;
    // Printed before the write, so a failed write does not hide them.
    let errors: Vec<&Issue> = parsed
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    for i in &errors {
        eprintln!(
            "papers: error: {}:{}: {} {}: {}",
            i.path, i.line, i.rule, i.key, i.message
        );
    }
    let merge = |records: &mut Vec<Reference>| {
        let (merged, vanished) = merge(from, &parsed, std::mem::take(records));
        *records = merged;
        vanished
    };
    let mut merged = registry::load(&config.registry)?.unwrap_or_default();
    let vanished = if dry_run {
        let vanished = merge(&mut merged);
        registry::sort(&mut merged);
        print!("{}", registry::render(&merged));
        vanished
    } else {
        // Merged into the registry as it is under the lock, so a concurrent
        // verify or read is not overwritten.
        registry::update(&config.registry, |records| {
            let vanished = merge(records);
            merged = records.clone();
            Ok(vanished)
        })?
    };
    for key in &vanished {
        eprintln!(
            "papers: warning: key {key} is in {} but no longer in {}; kept (keys are never deleted)",
            config.display(&config.registry),
            parsed.source
        );
    }
    eprintln!(
        "papers: {} {} entries from {} → {}",
        if dry_run { "parsed" } else { "wrote" },
        merged.len(),
        parsed.source,
        config.display(&config.registry)
    );
    print_stats(&merged, &parsed.issues);
    Ok(errors.is_empty())
}

pub fn check(config: &Config, from: Option<From>, format: Format) -> Result<bool, String> {
    let existing = registry::load(&config.registry)?;
    let registry_name = config.display(&config.registry);
    let from = match from {
        Some(f) => Some(f),
        None if config.catalogue.is_some() => Some(From::Catalogue),
        None if config.notes.is_dir() => Some(From::Notes),
        None => None,
    };
    let mut findings: Vec<Finding> = Vec::new();
    let records = match from {
        Some(from) => {
            let parsed = parse_source(config, from)?;
            findings.extend(check::issue_findings(&parsed.issues));
            let (merged, vanished) =
                merge(from, &parsed, existing.clone().unwrap_or_default());
            for key in vanished {
                findings.push(Finding::new(
                    "key-missing-from-source",
                    Severity::Warn,
                    key.clone(),
                    format!("key {key} is in {registry_name} but not in {}", parsed.source),
                ));
            }
            if let Some(existing) = &existing
                && registry::render(existing) != registry::render(&merged)
            {
                findings.push(Finding::new(
                    "registry-stale",
                    Severity::Warn,
                    registry_name.clone(),
                    format!("{registry_name} differs from {}; run papers registry build", parsed.source),
                ));
            }
            merged
        }
        None => existing.ok_or_else(|| {
            format!(
                "nothing to check: no catalogue configured, no notes directory {}, and no registry {registry_name}",
                config.display(&config.notes)
            )
        })?,
    };
    findings.extend(check::check(&records));
    findings.extend(derivative::check(config, &records, |_| Vec::new()));
    let ok = finding::emit(&findings, format);
    eprintln!(
        "papers: checked {} entries: {} findings{}",
        records.len(),
        findings.len(),
        if findings.is_empty() {
            String::new()
        } else {
            format!(" ({})", finding::summary(&findings))
        }
    );
    Ok(ok)
}
