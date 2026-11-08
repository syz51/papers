//! Numbered Markdown catalogues: one `## Reference N` section per work, whose
//! entry line reads `[N] Authors. **Title**. Venue … [Original](url). Accessed <date>.`
//! Older entries may instead give `Authors in source metadata:`,
//! `Date reported in source metadata:` and `Venue reported in source metadata:`
//! lines. Every extracted value comes from the text; anything the text does not
//! establish unambiguously stays `None` and, where useful, is reported as an issue.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use jiff::civil::Date;
use regex::Regex;

use crate::finding::Severity;
use crate::id::ArxivId;
use crate::refs::Doi;
use crate::registry::{DEPTHS, Reference, Source};
use crate::text::squash;

/// A problem found while parsing, reported by `registry check`.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub rule: &'static str,
    pub severity: Severity,
    pub key: String,
    /// The file the issue is in, as shown to the user.
    pub path: String,
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub records: Vec<Reference>,
    pub issues: Vec<Issue>,
}

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("valid built-in pattern"));
    };
}

re!(HEADING, r"^##\s+Reference\s+(\d+)\s*$");
re!(ENTRY, r"^\[(\d+)\]\s+(.*)$");
re!(BOLD, r"\*\*(.+?)\*\*");
re!(ITALIC, r"(?:^|[^*])\*([^*\s][^*]*?)\*(?:[^*]|$)");
re!(LABEL_AUTHORS, r"^Authors in source metadata:\s*(.*?)\s*$");
re!(
    LABEL_DATE,
    r"^Date reported in source metadata:\s*(.*?)\s*$"
);
re!(
    LABEL_VENUE,
    r"^Venue reported in source metadata:\s*(.*?)\s*$"
);
re!(LABEL_LINKS, r"^Original link forms retained:\s*(.*?)\s*$");
// Parentheses stay in the match: DOIs may contain balanced ones, and
// `Doi::parse` drops an unbalanced closing one.
re!(DOI, r#"10\.\d{4,9}/[^\s\]>,;"'<]+"#);
re!(ARXIV_URL, r"(?i)arxiv\.org/(?:abs|pdf|html)/([^\s\)\]]+)");
re!(
    ARXIV_TEXT,
    r"(?i)\barxiv[:\s]*(\d{4}\.\d{4,5}(?:v\d+)?|[a-z-]+(?:\.[A-Z]{2})?/\d{7}(?:v\d+)?)"
);
re!(VERSION_LABEL, r"(?i)Version:\s*arXiv\s*v(\d+)\b");
re!(MD_LINK, r"\]\((https?://(?:[^\s()]|\([^\s()]*\))+)\)");
re!(BARE_URL, r"https?://[^\s\)\]>]+");
re!(OPENALEX, r"openalex\.org/(?:works/)?(W\d+)");
re!(
    DBLP,
    r"dblp\.org/rec/([A-Za-z0-9/_\-]+?)(?:\.html|\.bib|\.xml)?(?:[\s\)\]]|$)"
);
re!(ACCESSED, r"Accessed");
re!(ISO_DATE, r"^\s*\**\s*(\d{4})-(\d{1,2})-(\d{1,2})");
re!(
    DMY_DATE,
    r"^\s*\**\s*(\d{1,2})\s*([A-Za-z]+)\s*,?\s*(\d{4})"
);
re!(MDY_DATE, r"^\s*\**\s*([A-Za-z]+)\s*(\d{1,2}),?\s*(\d{4})");
re!(
    LABEL_DATE_VALUE,
    r"^(\d{4})(?:[/-](\d{1,2})(?:[/-](\d{1,2}))?)?\.?$"
);
re!(ET_AL, r"(?i),?\s*et\s+al\.?");
re!(AUTHOR_SPLIT, r"\s*,\s*(?:and\s+)?|\s+and\s+|\s*&\s*");
re!(
    NOT_GIVEN,
    r"(?i)not extracted|not stated|not reported|unknown|anonymous"
);
re!(
    INSPECTION,
    r"^(?:\*\*)?Inspection(?: depth)?:(?:\*\*)?\s*(.*)$"
);
re!(SENTENCE_END, r"\.\s+[A-Z(]");
re!(
    DUP_OF,
    r"(?i)\b(?:alias|duplicate)(?: numbered entry)? of\s*\[?(\d+)\]?"
);
re!(
    SAME_AS,
    r"(?i)\bsame (?:paper|work)[^.\[]{0,40}?\bas (?:the )?(?:canonical )?\[(\d+)\]"
);
re!(
    IS_DUP,
    r"(?i)\[(\d+)\](?:\(#reference-\d+\))? is a duplicate(?: numbered entry)?\b"
);
re!(
    ARROW_ALIAS,
    r"(\d+)\s*→\s*(\d+)\s+(?:historical\s+)?alias\b"
);

/// A section's lines: `(line number, text)`.
struct Section<'a> {
    key: String,
    heading_line: usize,
    body: Vec<(usize, &'a str)>,
}

/// Parses a catalogue. `source_path` is recorded in each record's `source`.
pub fn parse(text: &str, source_path: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut sections: Vec<Section> = Vec::new();
    let mut in_section = false;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if let Some(c) = HEADING.captures(line) {
            sections.push(Section {
                key: c[1].to_owned(),
                heading_line: n,
                body: Vec::new(),
            });
            in_section = true;
        } else if line.starts_with("## ") || line.starts_with("# ") {
            in_section = false;
        } else if in_section && let Some(s) = sections.last_mut() {
            s.body.push((n, line));
        }
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut aliases: Vec<(String, String, usize)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for c in ARROW_ALIAS.captures_iter(line) {
            aliases.push((c[1].to_owned(), c[2].to_owned(), i + 1));
        }
    }
    for section in &sections {
        if let Some(first) = seen.get(&section.key) {
            parsed.issues.push(Issue {
                rule: "duplicate-key",
                severity: Severity::Error,
                key: section.key.clone(),
                path: source_path.to_owned(),
                line: section.heading_line,
                message: format!(
                    "heading Reference {} repeats the heading on line {first}; only the first is kept",
                    section.key
                ),
            });
            continue;
        }
        seen.insert(section.key.clone(), section.heading_line);
        let (record, mut issues, mut found) = parse_section(section, source_path);
        parsed.records.push(record);
        parsed.issues.append(&mut issues);
        aliases.append(&mut found);
    }
    apply_duplicates(&mut parsed, aliases, source_path);
    parsed
}

/// Records duplicate annotations `(duplicate, survivor, line)`; conflicting
/// annotations leave `duplicate_of` unset and are reported.
fn apply_duplicates(parsed: &mut Parsed, aliases: Vec<(String, String, usize)>, source_path: &str) {
    let mut targets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut lines: BTreeMap<String, usize> = BTreeMap::new();
    for (dup, survivor, line) in aliases {
        if dup != survivor {
            targets.entry(dup.clone()).or_default().insert(survivor);
            lines.entry(dup).or_insert(line);
        }
    }
    for record in &mut parsed.records {
        let Some(set) = targets.get(&record.key) else {
            continue;
        };
        if set.len() == 1 {
            record.duplicate_of = set.first().cloned();
        } else {
            parsed.issues.push(Issue {
                rule: "duplicate-annotation-conflict",
                severity: Severity::Error,
                key: record.key.clone(),
                path: source_path.to_owned(),
                line: lines[&record.key],
                message: format!(
                    "annotated as a duplicate of several keys ({}); duplicate_of left unset",
                    set.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
            });
        }
    }
}

type SectionResult = (Reference, Vec<Issue>, Vec<(String, String, usize)>);

fn parse_section(section: &Section, source_path: &str) -> SectionResult {
    let key = section.key.clone();
    let mut issues = Vec::new();
    let mut issue = |rule, severity, line, message: String| {
        issues.push(Issue {
            rule,
            severity,
            key: key.clone(),
            path: source_path.to_owned(),
            line,
            message,
        })
    };
    let mut record = Reference::new(key.clone());

    // The entry line: `[N] …`, else the first paragraph that is not a dated note.
    let numbered = section.body.iter().find_map(|&(n, l)| {
        ENTRY
            .captures(l)
            .map(|c| (n, c[1].to_owned(), c.get(2).map_or("", |m| m.as_str())))
    });
    let entry: Option<(usize, &str)> = match numbered {
        Some((n, number, rest)) => {
            if number != key {
                issue(
                    "key-mismatch",
                    Severity::Error,
                    n,
                    format!(
                        "heading says Reference {key} but the entry line is numbered [{number}]"
                    ),
                );
            }
            Some((n, rest))
        }
        None => {
            let fallback = section.body.iter().find(|(_, l)| {
                let t = l.trim_start();
                !t.is_empty() && !t.starts_with("**") && t.contains('*')
            });
            match fallback {
                Some(&(n, l)) => {
                    issue(
                        "entry-unnumbered",
                        Severity::Warn,
                        n,
                        format!(
                            "Reference {key} has no [{key}] entry line; read the first paragraph instead"
                        ),
                    );
                    Some((n, l.trim()))
                }
                None => {
                    issue(
                        "entry-missing",
                        Severity::Error,
                        section.heading_line,
                        format!("Reference {key} has no entry line"),
                    );
                    None
                }
            }
        }
    };
    record.source = Some(Source {
        path: source_path.to_owned(),
        line: entry.map_or(section.heading_line, |(n, _)| n),
    });

    let label = |re: &Regex| {
        section
            .body
            .iter()
            .find_map(|&(n, l)| re.captures(l.trim()).map(|c| (n, c[1].trim().to_owned())))
            .filter(|(_, v)| !v.is_empty())
    };
    let label_authors = label(&LABEL_AUTHORS);
    let label_date = label(&LABEL_DATE);
    let label_venue = label(&LABEL_VENUE);
    let label_links = label(&LABEL_LINKS);

    let (entry_line, entry_text) = entry.unwrap_or((section.heading_line, ""));
    let parts = split_entry(entry_text);

    // Authors: the entry's own segment, else the labelled line.
    let (authors, truncated) = match parts.authors.as_deref().map(parse_authors) {
        Some((Some(a), t)) => (Some(a), t),
        other => match &label_authors {
            Some((_, v)) => parse_authors(v),
            None => other.unwrap_or((None, false)),
        },
    };
    if authors.is_none()
        && let Some(segment) = parts
            .authors
            .as_deref()
            .or(label_authors.as_ref().map(|(_, v)| v.as_str()))
        && !NOT_GIVEN.is_match(segment)
    {
        issue(
            "authors-unparsed",
            Severity::Info,
            entry_line,
            format!(
                "author text {:?} does not split into names unambiguously",
                clip(segment)
            ),
        );
    }
    record.authors = authors;
    record.authors_truncated = truncated;
    record.title = parts.title.clone();

    // Ids: from the entry line and the retained link forms.
    let id_text = format!(
        "{entry_text}\n{}",
        label_links.as_ref().map_or("", |(_, v)| v.as_str())
    );
    let dois: BTreeSet<String> = DOI
        .find_iter(&id_text)
        .filter_map(|m| Doi::parse(m.as_str()))
        .map(|d| d.as_str().to_owned())
        .collect();
    record.ids.doi = single(&dois, "DOI", entry_line, &mut issue);
    let arxiv_mentions: Vec<ArxivId> = ARXIV_URL
        .captures_iter(&id_text)
        .chain(ARXIV_TEXT.captures_iter(&id_text))
        .filter_map(|c| {
            let raw = c[1].trim_end_matches(['.', ',', ';']);
            let raw = raw.strip_suffix(".pdf").unwrap_or(raw);
            ArxivId::parse(raw).ok()
        })
        .collect();
    let arxiv_ids: BTreeSet<String> = arxiv_mentions.iter().map(|a| a.id.clone()).collect();
    record.ids.arxiv = single(&arxiv_ids, "arXiv id", entry_line, &mut issue);
    if let Some(id) = &record.ids.arxiv {
        let versions: BTreeSet<u32> = arxiv_mentions
            .iter()
            .filter(|a| &a.id == id)
            .filter_map(|a| a.version)
            .collect();
        record.version = match versions.len() {
            1 => versions.first().map(|v| format!("v{v}")),
            0 => VERSION_LABEL
                .captures(entry_text)
                .map(|c| format!("v{}", &c[1])),
            _ => None,
        };
    }
    record.ids.url = MD_LINK
        .captures(entry_text)
        .map(|c| c[1].to_owned())
        .or_else(|| {
            BARE_URL
                .find(entry_text)
                .map(|m| m.as_str().trim_end_matches(['.', ',', ';']).to_owned())
        });
    record.ids.openalex = OPENALEX.captures(&id_text).map(|c| c[1].to_owned());
    record.ids.dblp = DBLP.captures(&id_text).map(|c| c[1].to_owned());

    // Venue and year from the text between the title and the first link.
    let venue_text = parts.venue.as_deref().unwrap_or_default();
    record.venue = first_sentence(venue_text)
        .or_else(|| label_venue.as_ref().map(|(_, v)| clean_value(v)))
        .filter(|v| !v.is_empty());
    record.year = first_year(venue_text);
    if let Some((n, date)) = &label_date {
        if !LABEL_DATE_VALUE.is_match(date) {
            issue(
                "malformed-date",
                Severity::Warn,
                *n,
                format!("reported date {date:?} is not YYYY, YYYY/MM or YYYY/MM/DD"),
            );
        }
        if record.year.is_none() {
            record.year = first_year(date);
        }
    }

    // Accessed date: the entry line, else a labelled line.
    let accessed_source = std::iter::once((entry_line, entry_text)).chain(
        section
            .body
            .iter()
            .copied()
            .filter(|(_, l)| l.contains("in source metadata:")),
    );
    for (n, text) in accessed_source {
        let Some(m) = ACCESSED.find(text) else {
            continue;
        };
        let after = &text[m.end()..];
        match parse_accessed(after) {
            Ok(date) => record.accessed = Some(date),
            Err(raw) => issue(
                "malformed-date",
                Severity::Error,
                n,
                format!("access date {raw:?} is not a valid calendar date"),
            ),
        }
        break;
    }

    record.depth = depth(section, &mut issue);
    record.kind = kind(&parts, &record);

    // Duplicate annotations in this section.
    let mut aliases = Vec::new();
    for &(n, l) in &section.body {
        for (dup, survivor) in duplicate_claims(&key, l) {
            aliases.push((dup, survivor, n));
        }
    }
    (record, issues, aliases)
}

re!(
    DUP_NEGATED,
    r"(?i)\b(?:not|never|no longer|neither|nor)\b|n['’]t\b"
);
re!(CLAUSE_BREAK, r"[,:;]|\.\s");
re!(REF_NUMBER, r"\[(\d+)\]");
re!(OF_NEXT, r"^\s*(?:numbered entry\s+)?of\b");

/// Duplicate claims `(duplicate, survivor)` made by one line of section `key`.
/// A claim's subject is the `[N]` named earlier in its clause ("[12] is a
/// duplicate of [7]"), else the section itself ("Duplicate of [7]"). Negated
/// clauses ("not a duplicate of [7]") claim nothing.
fn duplicate_claims(key: &str, line: &str) -> Vec<(String, String)> {
    let clause = |start: usize, end: usize| {
        let from = CLAUSE_BREAK
            .find_iter(&line[..start])
            .last()
            .map_or(0, |m| m.end());
        &line[from..end]
    };
    let mut claims = Vec::new();
    for c in DUP_OF
        .captures_iter(line)
        .chain(SAME_AS.captures_iter(line))
    {
        let m = c.get(0).expect("group 0 always matches");
        let prefix = clause(m.start(), m.start());
        if DUP_NEGATED.is_match(prefix) || DUP_NEGATED.is_match(m.as_str()) {
            continue;
        }
        let subject = REF_NUMBER
            .captures_iter(prefix)
            .last()
            .map_or_else(|| key.to_owned(), |r| r[1].to_owned());
        claims.push((subject, c[1].to_owned()));
    }
    for c in IS_DUP.captures_iter(line) {
        let m = c.get(0).expect("group 0 always matches");
        // "[A] is a duplicate of [B]" names its survivor; DUP_OF handled it.
        if OF_NEXT.is_match(&line[m.end()..]) || DUP_NEGATED.is_match(clause(m.start(), m.end())) {
            continue;
        }
        claims.push((c[1].to_owned(), key.to_owned()));
    }
    claims
}

/// The only value of `set`, or `None` (reported) when several disagree.
fn single(
    set: &BTreeSet<String>,
    what: &str,
    line: usize,
    issue: &mut impl FnMut(&'static str, Severity, usize, String),
) -> Option<String> {
    match set.len() {
        0 => None,
        1 => set.first().cloned(),
        _ => {
            issue(
                "ambiguous-id",
                Severity::Info,
                line,
                format!(
                    "the entry names several {what}s ({}); none recorded",
                    set.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
            );
            None
        }
    }
}

/// The parts of an entry line around its bold (or else italic) title.
#[derive(Debug, Default, PartialEq)]
struct EntryParts {
    authors: Option<String>,
    title: Option<String>,
    /// Text after the title, up to the first link, URL or "Accessed".
    venue: Option<String>,
}

fn split_entry(entry: &str) -> EntryParts {
    let span = match BOLD.captures(entry) {
        Some(c) => c.get(0).map(|w| (w.start(), c[1].to_owned(), w.end())),
        // The italic pattern's match may include one character on either side.
        None => ITALIC
            .captures(entry)
            .and_then(|c| c.get(1))
            .map(|m| (m.start() - 1, m.as_str().to_owned(), m.end() + 1)),
    };
    let Some((start, title, end)) = span else {
        return EntryParts::default();
    };
    let authors = clean_value(&entry[..start]);
    let rest = &entry[end.min(entry.len())..];
    let stop = ["[", "http://", "https://", "Accessed"]
        .iter()
        .filter_map(|s| rest.find(s))
        .min()
        .unwrap_or(rest.len());
    let venue = rest[..stop]
        .trim_start_matches(['.', ',', ';', ':', ' '])
        .trim();
    let title = clean_value(&title);
    EntryParts {
        authors: (!authors.is_empty()).then_some(authors),
        title: (!title.is_empty()).then_some(title),
        venue: (!venue.is_empty()).then(|| venue.to_owned()),
    }
}

/// Squashed text without emphasis markers, backticks, or a trailing period
/// (kept after an initial or a common abbreviation such as `Inc.`).
fn clean_value(s: &str) -> String {
    let s = squash(&s.replace(['*', '`'], ""));
    let s = s.trim().trim_end_matches([',', ';', ':']).trim();
    if let Some(stripped) = s.strip_suffix("..") {
        return format!("{stripped}.");
    }
    let last = s.split_whitespace().last().unwrap_or_default();
    let keep = is_initial(last)
        || ["Inc.", "Ltd.", "Co.", "Corp.", "Jr.", "Sr.", "Bros.", "al."].contains(&last);
    match s.strip_suffix('.') {
        Some(stripped) if !keep => stripped.trim_end().to_owned(),
        _ => s.to_owned(),
    }
}

fn is_initial(token: &str) -> bool {
    let token = token.trim_end_matches(',');
    let mut chars = token.chars();
    matches!((chars.next(), chars.next(), chars.next()), (Some(c), Some('.'), None) if c.is_uppercase())
        || (token.contains('-') && token.split('-').all(|p| {
            let mut c = p.chars();
            matches!((c.next(), c.next(), c.next()), (Some(x), Some('.'), None) if x.is_uppercase())
        }))
}

/// Up to the first sentence end, without emphasis markers. A period after an
/// initial or an abbreviation (`pp.`, `U.S.`) does not end a sentence.
fn first_sentence(s: &str) -> Option<String> {
    const ABBREVIATIONS: [&str; 10] = [
        "pp.", "p.", "Vol.", "No.", "al.", "vs.", "Proc.", "Inc.", "Ltd.", "Co.",
    ];
    let end = SENTENCE_END
        .find_iter(s)
        .find(|m| {
            let word = s[..m.start() + 1]
                .split_whitespace()
                .last()
                .unwrap_or_default();
            !(is_initial(word)
                || word.trim_end_matches('.').contains('.')
                || ABBREVIATIONS.contains(&word))
        })
        .map_or(s.len(), |m| m.start());
    let v = clean_value(&s[..end]);
    (!v.is_empty()).then_some(v)
}

/// First plausible year (1900–2099) standing alone as four digits, after
/// removing DOIs, arXiv ids and URLs whose digits are not years.
fn first_year(s: &str) -> Option<i64> {
    let s = BARE_URL.replace_all(s, " ");
    let s = DOI.replace_all(&s, " ");
    let s = ARXIV_TEXT.replace_all(&s, " ");
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // A digit run joined to another by '.' is a number such as 1913.00001.
            let dotted = (start > 0
                && bytes[start - 1] == b'.'
                && start > 1
                && bytes[start - 2].is_ascii_digit())
                || (i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit());
            if i - start == 4 && !dotted {
                let y: i64 = s[start..i].parse().unwrap_or_default();
                if (1900..=2099).contains(&y) {
                    return Some(y);
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

fn month(name: &str) -> Option<i8> {
    let name = name.to_ascii_lowercase();
    if name.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| m.starts_with(&name) || (name == "sept" && *m == "september"))
        .map(|i| i as i8 + 1)
}

/// Parses the text after "Accessed": `2026-10-09`, `6 November 2026`,
/// `6November2026`, `**21 October 2026**` or `November 6, 2026`. Returns the
/// ISO date, or the raw text when it is not a valid date.
fn parse_accessed(after: &str) -> Result<String, String> {
    let raw = || clip(after.trim()).trim_end_matches('.').to_owned();
    let date = if let Some(c) = ISO_DATE.captures(after) {
        let p = |i: usize| c[i].parse::<i64>().ok();
        match (p(1), p(2), p(3)) {
            (Some(y), Some(m), Some(d)) => civil(y, m, d),
            _ => None,
        }
    } else if let Some(c) = DMY_DATE.captures(after) {
        month(&c[2]).and_then(|m| civil(c[3].parse().ok()?, m.into(), c[1].parse().ok()?))
    } else if let Some(c) = MDY_DATE.captures(after) {
        month(&c[1]).and_then(|m| civil(c[3].parse().ok()?, m.into(), c[2].parse().ok()?))
    } else {
        None
    };
    date.map(|d| d.to_string()).ok_or_else(raw)
}

fn civil(y: i64, m: i64, d: i64) -> Option<Date> {
    Date::new(
        i16::try_from(y).ok()?,
        i8::try_from(m).ok()?,
        i8::try_from(d).ok()?,
    )
    .ok()
}

fn clip(s: &str) -> String {
    let mut out: String = s.chars().take(60).collect();
    if s.chars().count() > 60 {
        out.push('…');
    }
    out
}

pub const ORG_WORDS: [&str; 26] = [
    "Inc",
    "Inc.",
    "Ltd",
    "Ltd.",
    "LLC",
    "Co.",
    "Corp.",
    "Corporation",
    "Company",
    "Commission",
    "University",
    "School",
    "Institute",
    "Department",
    "Division",
    "Office",
    "Agency",
    "Association",
    "Society",
    "Committee",
    "Foundation",
    "Project",
    "project",
    "International",
    "contributors",
    "organizers",
];
pub const PARTICLES: [&str; 14] = [
    "van", "von", "de", "der", "den", "da", "di", "du", "la", "le", "bin", "binti", "al", "el",
];

/// Splits an author segment into names. Handles `A; B`, `Family, Given; …`,
/// `A, B and C`, `A. Family, B. Family`, `Family, Given, and Family, Given`,
/// a trailing "et al." (reported as truncated), and organizations. Returns
/// `None` when the shape is ambiguous.
pub fn parse_authors(segment: &str) -> (Option<Vec<String>>, bool) {
    let segment = clean_value(segment);
    if segment.is_empty() || NOT_GIVEN.is_match(&segment) {
        return (None, false);
    }
    let truncated = ET_AL.is_match(&segment);
    let segment = ET_AL.replace_all(&segment, "").trim().to_owned();
    if segment.contains(';') {
        let names: Option<Vec<String>> = segment
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once(',') {
                Some((family, given))
                    if plausible_family(family.trim()) && plausible_given(given.trim()) =>
                {
                    Some(format!("{} {}", given.trim(), family.trim()))
                }
                Some(_) => None,
                None => plausible_name(p).then(|| p.to_owned()),
            })
            .collect();
        return (names.filter(|n| !n.is_empty()), truncated);
    }
    if segment
        .split_whitespace()
        .any(|w| ORG_WORDS.contains(&w.trim_end_matches(',')))
    {
        return (Some(vec![segment]), truncated);
    }
    let parts: Vec<&str> = AUTHOR_SPLIT
        .split(&segment)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() == 1 {
        return (
            plausible_name(parts[0]).then(|| vec![parts[0].to_owned()]),
            truncated,
        );
    }
    if !parts.iter().all(|p| plausible_name(p)) {
        return (None, truncated);
    }
    // A part made only of initials ("A.") is the given half of "Family, I.",
    // never a whole name.
    let initials_only = parts.iter().any(|p| p.split_whitespace().all(is_initial));
    let direct = !initials_only
        && (parts
            .iter()
            .any(|p| p.split_whitespace().next().is_some_and(is_initial))
            || parts.iter().all(|p| p.split_whitespace().count() >= 2));
    if direct {
        return (
            Some(parts.iter().map(|p| (*p).to_owned()).collect()),
            truncated,
        );
    }
    let inverted = parts.len().is_multiple_of(2)
        && parts
            .chunks(2)
            .all(|pair| plausible_family(pair[0]) && plausible_given(pair[1]));
    if inverted {
        let names = parts
            .chunks(2)
            .map(|pair| format!("{} {}", pair[1], pair[0]))
            .collect();
        return (Some(names), truncated);
    }
    (None, truncated)
}

/// A personal or organizational name: no digits and at most one word that
/// starts in lower case other than a name particle.
fn plausible_name(s: &str) -> bool {
    !s.is_empty()
        && !s.bytes().any(|b| b.is_ascii_digit())
        && s.split_whitespace()
            .filter(|w| w.starts_with(|c: char| c.is_lowercase()) && !PARTICLES.contains(w))
            .count()
            <= 1
}

/// A family name: words that start in upper case, or name particles.
fn plausible_family(s: &str) -> bool {
    !s.is_empty()
        && s.split_whitespace().all(|w| {
            PARTICLES.contains(&w)
                || (w.starts_with(|c: char| c.is_uppercase())
                    && w.chars().all(|c| c.is_alphabetic() || "-'’".contains(c)))
        })
}

/// Given names: capitalized words or initials.
fn plausible_given(s: &str) -> bool {
    !s.is_empty()
        && s.split_whitespace().all(|w| {
            is_initial(w)
                || (w.starts_with(|c: char| c.is_uppercase())
                    && w.chars().all(|c| c.is_alphabetic() || "-'’.".contains(c)))
        })
}

/// Depth phrases, checked in order on the section's `Inspection:` lines.
static DEPTH_PHRASES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        (
            "abstract",
            r"(?i)\babstract(?:-level reading)? only\b|\babstract and crossref ?metadata only\b",
        ),
        ("metadata", r"(?i)\bmetadata[- ]only\b"),
        (
            "method",
            r"(?i)\bselected (?:current |fresh |published )?original[- ]?methods?\b|\bselected original-method\b|\bselected method\b|\boriginal method/evaluation\b",
        ),
        (
            "full",
            r"(?i)\bread in full\b|\bfull[- ]text read\b|\bread fully\b|\bfull paper read\b",
        ),
    ]
    .into_iter()
    .map(|(d, p)| (d, Regex::new(p).expect("valid built-in pattern")))
    .collect()
});
re!(NEGATION, r"(?i)\b(?:not|no|without|never)\b[^.;]{0,20}$");

/// An explicit depth line: `Inspection depth: <level>`, in any letter case,
/// optionally a list item and with the label in bold or italics.
static EXPLICIT_DEPTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:[-*+]\s+)?[*_]{0,2}inspection depth[*_]{0,2}:[*_]{0,2}\s*(.*?)\s*$")
        .expect("valid built-in pattern")
});

/// The inspection depth. A line `Inspection depth: <level>` naming one
/// `rf.reference/1` level states it outright and wins over prose; a one-word
/// value outside the vocabulary, or two different explicit levels, is reported
/// and leaves the depth unknown. A longer value is read as prose. Without an explicit line, it is the level when the
/// section's `Inspection:` lines name exactly one in plain words (negated
/// mentions such as "not read in full" are ignored).
fn depth(
    section: &Section,
    issue: &mut impl FnMut(&'static str, Severity, usize, String),
) -> Option<String> {
    let mut explicit = BTreeMap::new();
    let mut levels = BTreeSet::new();
    for &(n, line) in &section.body {
        let line = line.trim();
        // A one-word value is a stated level; longer text is prose, read below.
        if let Some(c) = EXPLICIT_DEPTH.captures(line)
            && let stated = c[1]
                .trim_matches(|ch: char| "*_`.;,".contains(ch) || ch.is_whitespace())
                .to_ascii_lowercase()
            && !stated.is_empty()
            && !stated.contains(char::is_whitespace)
        {
            match DEPTHS.iter().find(|d| **d == stated) {
                Some(level) => {
                    explicit.entry(*level).or_insert(n);
                }
                None => issue(
                    "depth-unparsed",
                    Severity::Warn,
                    n,
                    format!(
                        "inspection depth {:?} is not one of {}",
                        clip(&c[1]),
                        DEPTHS.join(", ")
                    ),
                ),
            }
            continue;
        }
        let Some(c) = INSPECTION.captures(line) else {
            continue;
        };
        let mut text = c[1].to_owned();
        for (level, re) in DEPTH_PHRASES.iter() {
            let mut hit = false;
            for m in re.find_iter(&text) {
                if !NEGATION.is_match(&text[..m.start()]) {
                    hit = true;
                }
            }
            if hit {
                levels.insert(*level);
            }
            text = re.replace_all(&text, " ").into_owned();
        }
    }
    if !explicit.is_empty() {
        if explicit.len() > 1 {
            let line = explicit.values().copied().max().unwrap_or_default();
            let named: Vec<&str> = explicit.keys().copied().collect();
            issue(
                "depth-unparsed",
                Severity::Warn,
                line,
                format!(
                    "inspection depth lines name different levels: {}",
                    named.join(", ")
                ),
            );
            return None;
        }
        return explicit.keys().next().map(|l| (*l).to_owned());
    }
    (levels.len() == 1)
        .then(|| levels.first().map(|l| (*l).to_owned()))
        .flatten()
}

/// The work's type when the entry states it in plain words; `paper` for an
/// arXiv id with no other statement; `None` when unclear or contradictory.
fn kind(parts: &EntryParts, record: &Reference) -> Option<String> {
    static KINDS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
        [
            ("software", r"(?i)\bsoftware\b|\bsource code\b|\brepository\b"),
            ("dataset", r"(?i)\bdataset\b|\bdata set\b|\bdataset card\b"),
            ("web", r"(?i)\bweb ?page\b|\bdocumentation\b|\bwebsite\b"),
            ("standard", r"(?i)\bstandard\b|\bspecification\b|\brecommendation\b"),
            (
                "paper",
                r"(?i)\bjournal\b|\bproceedings\b|\bconference\b|\bworkshop\b|\bpreprint\b|\bworking paper\b|\barxiv\b",
            ),
        ]
        .into_iter()
        .map(|(k, p)| (k, Regex::new(p).expect("valid built-in pattern")))
        .collect()
    });
    let text = parts.venue.as_deref().unwrap_or_default();
    let found: Vec<&str> = KINDS
        .iter()
        .filter(|(_, re)| re.is_match(text))
        .map(|(k, _)| *k)
        .collect();
    match found.as_slice() {
        [one] => Some((*one).to_owned()),
        [] if record.ids.arxiv.is_some() => Some("paper".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic entries in the shapes the parser supports; no real catalogue text.
    const CATALOGUE: &str = "\
# Source catalogue

Preface: entries 2→1 historical alias noted here.

## Reference 1

**Dated note, 3 March 2026:** [2](#reference-2) is a duplicate numbered entry for this same paper.

[1] Doe, Jane Q.; Roe, Rick; Okafor, Ada. **A Synthetic Study of Widget Retrieval**. Conference on Widgets 2025; 2025-06-01. Version: arXivv2. [Original source](https://arxiv.org/abs/2506.01234). Accessed12October2026.

Inspection: selected original methods read; not full read.

## Reference 2

**Historical alias:** this numbered entry identifies the same paper as canonical [1](#reference-1).

[2] Jane Q. Doe; Rick Roe; Ada Okafor. **A Synthetic Study of Widget Retrieval (Extended)**. arXiv 2506.01234v2, 1 June 2025. [Original](https://arxiv.org/html/2506.01234v2). Accessed **21 October 2026**.

## Notes between sections

Not a reference.

## Reference 3

[3] **Labelled Metadata for Gadget Ranking**. [Original source](https://example.org/2024.gadget-1.7/).

Authors in source metadata: Mo Ito; Lu Park; Sam Tran.
Date reported in source metadata: 2024/8.
Venue reported in source metadata: Proceedings of the 2nd Gadget Workshop.
Original link forms retained: [link 1](https://example.org/2024.gadget-1.7.pdf).

## Reference 4

[5] Example Standards Body. **Widget Interchange Format 1.0**. Official specification, 2019. DOI10.5555/wif.2019.1. [Original](https://doi.org/10.5555/wif.2019.1). Accessed 2026-13-40.

## Reference 6

Example Corp. **Pricing Notes.** Official web page, date not stated. https://example.com/pricing. Accessed 3 Febtober 2026.

## Reference 7

[7] Kim, Sora, and Lee, Jin-Ho. **Two Inverted Authors**. Journal of Synthetic Results 12(3):45-67, 2023. Accessed 2026-01-05.
";

    fn parsed() -> Parsed {
        parse(CATALOGUE, "docs/references.md")
    }

    fn get(p: &Parsed, key: &str) -> Reference {
        p.records.iter().find(|r| r.key == key).cloned().unwrap()
    }

    #[test]
    fn parses_prose_entries() {
        let p = parsed();
        assert_eq!(
            p.records.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["1", "2", "3", "4", "6", "7"]
        );
        let r = get(&p, "1");
        assert_eq!(
            r.title.as_deref(),
            Some("A Synthetic Study of Widget Retrieval")
        );
        assert_eq!(
            r.authors.as_deref().unwrap(),
            ["Jane Q. Doe", "Rick Roe", "Ada Okafor"]
        );
        assert_eq!(r.year, Some(2025));
        assert_eq!(
            r.venue.as_deref(),
            Some("Conference on Widgets 2025; 2025-06-01")
        );
        assert_eq!(r.ids.arxiv.as_deref(), Some("2506.01234"));
        assert_eq!(r.version.as_deref(), Some("v2"));
        assert_eq!(
            r.ids.url.as_deref(),
            Some("https://arxiv.org/abs/2506.01234")
        );
        assert_eq!(r.accessed.as_deref(), Some("2026-10-12"), "glued date");
        assert_eq!(
            r.depth.as_deref(),
            Some("method"),
            "negated full read is ignored"
        );
        assert_eq!(r.kind.as_deref(), Some("paper"));
        assert_eq!(r.source.as_ref().unwrap().line, 9);
        assert_eq!(r.duplicate_of, None);

        let r = get(&p, "2");
        assert_eq!(r.accessed.as_deref(), Some("2026-10-21"), "bold date");
        assert_eq!(r.version.as_deref(), Some("v2"));
        assert_eq!(r.year, Some(2025));
        assert_eq!(r.duplicate_of.as_deref(), Some("1"));
        assert_eq!(r.depth, None);
    }

    #[test]
    fn an_explicit_depth_line_wins_over_prose_and_conflicts_stay_unknown() {
        let catalogue = "\
## Reference 1

[1] Doe, Jane. **Explicit Over Prose**. Example Venue, 2025. [Original](https://example.org/1). Accessed 2026-01-05.

Inspection: selected original methods read.
Inspection depth: Artifact.

## Reference 2

[2] Doe, Jane. **Only The Explicit Line**. Example Venue, 2025. [Original](https://example.org/2). Accessed 2026-01-05.

**Inspection depth:** reproduced

## Reference 3

[3] Doe, Jane. **Two Explicit Levels**. Example Venue, 2025. [Original](https://example.org/3). Accessed 2026-01-05.

Inspection depth: abstract.
Inspection depth: full.

## Reference 4

[4] Doe, Jane. **Not A Level**. Example Venue, 2025. [Original](https://example.org/4). Accessed 2026-01-05.

Inspection depth: skimmed.
";
        let p = parse(catalogue, "docs/references.md");
        assert_eq!(get(&p, "1").depth.as_deref(), Some("artifact"));
        assert_eq!(get(&p, "2").depth.as_deref(), Some("reproduced"));
        assert_eq!(get(&p, "3").depth, None, "conflicting explicit levels");
        assert_eq!(get(&p, "4").depth, None, "unknown word is not a level");
        let reported: Vec<(&str, &str, usize)> = p
            .issues
            .iter()
            .map(|i| (i.rule, i.key.as_str(), i.line))
            .collect();
        assert_eq!(
            reported,
            [("depth-unparsed", "3", 19), ("depth-unparsed", "4", 25)],
            "both are reported where they are written"
        );
    }

    #[test]
    fn only_a_depth_label_is_explicit_and_its_markup_is_tolerated() {
        let catalogue = "\
## Reference 1

[1] Doe, Jane. **Plain Inspection Lines Stay Prose**. Example Venue, 2025. [Original](https://example.org/1). Accessed 2026-01-05.

Inspection: full
Inspection: abstract only.

## Reference 2

[2] Doe, Jane. **Bold Label, Code Value**. Example Venue, 2025. [Original](https://example.org/2). Accessed 2026-01-05.

**Inspection depth**: `full`

## Reference 3

[3] Doe, Jane. **List Item, Other Case**. Example Venue, 2025. [Original](https://example.org/3). Accessed 2026-01-05.

- Inspection Depth: Method;

## Reference 4

[4] Doe, Jane. **Prose After The Depth Label**. Example Venue, 2025. [Original](https://example.org/4). Accessed 2026-01-05.

Inspection depth: original page fetched; abstract only.
";
        let p = parse(catalogue, "docs/references.md");
        assert_eq!(get(&p, "1").depth.as_deref(), Some("abstract"));
        assert_eq!(get(&p, "2").depth.as_deref(), Some("full"));
        assert_eq!(get(&p, "3").depth.as_deref(), Some("method"));
        assert_eq!(get(&p, "4").depth.as_deref(), Some("abstract"), "prose");
        assert!(p.issues.is_empty(), "{:?}", p.issues);
    }

    #[test]
    fn parses_labelled_entries() {
        let r = get(&parsed(), "3");
        assert_eq!(
            r.title.as_deref(),
            Some("Labelled Metadata for Gadget Ranking")
        );
        assert_eq!(
            r.authors.as_deref().unwrap(),
            ["Mo Ito", "Lu Park", "Sam Tran"]
        );
        assert_eq!(r.year, Some(2024));
        assert_eq!(
            r.venue.as_deref(),
            Some("Proceedings of the 2nd Gadget Workshop")
        );
        assert_eq!(
            r.ids.url.as_deref(),
            Some("https://example.org/2024.gadget-1.7/")
        );
        assert_eq!(r.accessed, None);
        assert_eq!(r.ids.doi, None);
    }

    #[test]
    fn reports_mismatched_keys_bad_dates_and_unnumbered_entries() {
        let p = parsed();
        let rules: Vec<(&str, &str)> = p.issues.iter().map(|i| (i.rule, i.key.as_str())).collect();
        assert!(rules.contains(&("key-mismatch", "4")), "{rules:?}");
        assert!(rules.contains(&("malformed-date", "4")), "{rules:?}");
        assert!(rules.contains(&("entry-unnumbered", "6")), "{rules:?}");
        assert!(rules.contains(&("malformed-date", "6")), "{rules:?}");
        let r = get(&p, "4");
        assert_eq!(r.ids.doi.as_deref(), Some("10.5555/wif.2019.1"));
        assert_eq!(r.year, Some(2019));
        assert_eq!(r.kind.as_deref(), Some("standard"));
        assert_eq!(r.authors.as_deref().unwrap(), ["Example Standards Body"]);
        assert_eq!(r.accessed, None);
        let r = get(&p, "6");
        assert_eq!(r.title.as_deref(), Some("Pricing Notes"));
        assert_eq!(r.ids.url.as_deref(), Some("https://example.com/pricing"));
        assert_eq!(r.kind.as_deref(), Some("web"));
        assert_eq!(r.year, None, "no year is stated");
        let r = get(&p, "7");
        assert_eq!(r.authors.as_deref().unwrap(), ["Sora Kim", "Jin-Ho Lee"]);
        assert_eq!(r.year, Some(2023));
        assert_eq!(r.accessed.as_deref(), Some("2026-01-05"));
    }

    #[test]
    fn conflicting_duplicate_annotations_are_reported() {
        let text = "## Reference 1\n\n[1] **A**. Accessed 2026-01-01.\n\n## Reference 2\n\n\
                    [2] **B**. Alias of [1]. Also a duplicate of [3].\n\n## Reference 3\n\n[3] **C**.\n";
        let p = parse(text, "r.md");
        assert_eq!(get(&p, "2").duplicate_of, None);
        assert!(
            p.issues
                .iter()
                .any(|i| i.rule == "duplicate-annotation-conflict")
        );
        let text = "## Reference 1\n\n[1] **A**.\n\n## Reference 1\n\n[1] **B**.\n";
        let p = parse(text, "r.md");
        assert_eq!(p.records.len(), 1);
        assert_eq!(p.issues[0].rule, "duplicate-key");
    }

    #[test]
    fn duplicate_claims_follow_negation_and_subject() {
        let text = "## Reference 5\n\n[5] **Widgets**.\n\nNote: not a duplicate of [7].\n\n\
                    ## Reference 7\n\n[7] **Widgets again**.\n\n[16] is a duplicate numbered entry.\n\n\
                    ## Reference 9\n\n[9] **Gadgets**.\n\nAudit: [12] is a duplicate of [7].\n\n\
                    ## Reference 12\n\n[12] **Gadgets**.\n\n\
                    ## Reference 14\n\n[14] **Gizmos**.\n\nAs [3] notes, this is the same work as [7].\n\n\
                    ## Reference 15\n\n[15] **Gizmos**.\n\nIt isn't the same paper as [7].\n\n\
                    ## Reference 16\n\n[16] **Widgets**.\n";
        let p = parse(text, "r.md");
        let dup = |k| get(&p, k).duplicate_of.clone();
        assert_eq!(dup("5"), None, "negated");
        assert_eq!(dup("9"), None, "the claim is about [12]");
        assert_eq!(dup("12").as_deref(), Some("7"));
        assert_eq!(dup("14").as_deref(), Some("7"));
        assert_eq!(dup("15"), None, "negated");
        assert_eq!(dup("16").as_deref(), Some("7"));
    }

    #[test]
    fn dois_keep_balanced_parentheses() {
        let text = "## Reference 1\n\n[1] **Widgets**. J. Synth. \
                    [Original](https://doi.org/10.5555/0304-405X(90)90011-X). Accessed 2026-01-01.\n\n\
                    ## Reference 2\n\n[2] **Gadgets** (doi:10.5555/w1).\n";
        let p = parse(text, "r.md");
        let r = get(&p, "1");
        assert_eq!(r.ids.doi.as_deref(), Some("10.5555/0304-405x(90)90011-x"));
        assert_eq!(
            r.ids.url.as_deref(),
            Some("https://doi.org/10.5555/0304-405X(90)90011-X")
        );
        assert_eq!(get(&p, "2").ids.doi.as_deref(), Some("10.5555/w1"));
    }

    #[test]
    fn splits_author_segments() {
        let a = |s: &str| parse_authors(s).0;
        assert_eq!(
            a("Q. Example, Z. Sample.").unwrap(),
            ["Q. Example", "Z. Sample"]
        );
        assert_eq!(a("Lee, A., Chen, B.").unwrap(), ["A. Lee", "B. Chen"]);
        assert_eq!(a("Lee, A. and Chen, B.").unwrap(), ["A. Lee", "B. Chen"]);
        assert_eq!(
            a("Doe, J. R., Roe, A. and Poe, L. M.").unwrap(),
            ["J. R. Doe", "A. Roe", "L. M. Poe"]
        );
        assert_eq!(
            a("Ana Silva, Bo Li, Cy Moe and Di Wu.").unwrap(),
            ["Ana Silva", "Bo Li", "Cy Moe", "Di Wu"]
        );
        assert_eq!(
            a("Li, Ying, de Souza, Marta, and Ma, Tao.").unwrap(),
            ["Ying Li", "Marta de Souza", "Tao Ma"]
        );
        assert_eq!(a("Park, Bernard J.").unwrap(), ["Bernard J. Park"]);
        assert_eq!(a("Widget Makers, Inc.").unwrap(), ["Widget Makers, Inc."]);
        assert_eq!(a("Gadget Co., Ltd..").unwrap(), ["Gadget Co., Ltd."]);
        assert_eq!(
            a("Example University, Graduate School of Widgets.").unwrap(),
            ["Example University, Graduate School of Widgets"]
        );
        assert_eq!(a("Example Org.").unwrap(), ["Example Org"]);
        let (names, truncated) = parse_authors("Wu, Hana, et al.");
        assert_eq!(
            (names.unwrap(), truncated),
            (vec!["Hana Wu".to_owned()], true)
        );
        let (names, truncated) = parse_authors("A. Moe, B. Roe, et al..");
        assert_eq!(names.unwrap(), ["A. Moe", "B. Roe"]);
        assert!(truncated);
        assert_eq!(a("Authors not extracted."), None);
        assert_eq!(a("user_x, Site.tv."), None);
        assert_eq!(a("Group9, course12345."), None);
        assert_eq!(a("owner; model card credits two people for it"), None);
        assert_eq!(a("Mori, as named on the cover."), None);
        assert_eq!(a("森 太郎.").unwrap(), ["森 太郎"]);
    }

    #[test]
    fn parses_access_dates() {
        assert_eq!(parse_accessed("6 November 2026.").unwrap(), "2026-11-06");
        assert_eq!(
            parse_accessed("6November2026,Asia/Tokyo").unwrap(),
            "2026-11-06"
        );
        assert_eq!(
            parse_accessed(" **27 October 2026**.").unwrap(),
            "2026-10-27"
        );
        assert_eq!(parse_accessed("2026-10-09; pinned").unwrap(), "2026-10-09");
        assert_eq!(parse_accessed(" Nov 6, 2026").unwrap(), "2026-11-06");
        assert_eq!(parse_accessed(" 6 Sept 2026").unwrap(), "2026-09-06");
        assert!(parse_accessed(" 31 February 2026").is_err());
        assert!(parse_accessed(" recently").is_err());
        assert!(parse_accessed("2026-02-30").is_err());
    }

    #[test]
    fn finds_years_outside_identifiers() {
        assert_eq!(
            first_year("arXiv 1913.00001v1 read (v2, 24 May 2019)"),
            Some(2019)
        );
        assert_eq!(
            first_year("Management19(6),402; DOI10.3390/x19060402; 2026-05-31"),
            Some(2026)
        );
        assert_eq!(first_year("SIGIR2003, pp.10–17"), Some(2003));
        assert_eq!(first_year("Vol4283, revision 1703157"), None);
        assert_eq!(first_year("https://example.org/2020/x"), None);
    }

    #[test]
    fn several_dois_are_ambiguous() {
        let text = "## Reference 9\n\n[9] **T**. DOI 10.1234/aa and DOI 10.1234/bb.\n";
        let p = parse(text, "r.md");
        assert_eq!(p.records[0].ids.doi, None);
        assert_eq!(p.issues[0].rule, "ambiguous-id");
    }
}
