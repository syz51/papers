//! `rf.paper-structure/1`: where a paper's text has its pages, sections and
//! reference list, and which registry entries the reference list cites. The
//! sidecar `<text file name>.structure.json` lies beside the text; the text
//! itself is never changed. Lines are numbered from 1 as the file splits at
//! LF, and ranges include both ends. What detection does not find is recorded
//! as `unknown`, never guessed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::registry::Reference;
use crate::text;

pub const SCHEMA: &str = "rf.paper-structure/1";
/// Appended to the text's file name: `87.txt.structure.json`.
pub const SUFFIX: &str = ".structure.json";
const UNKNOWN: &str = "unknown";

/// A value, or the string `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Known<T> {
    Known(T),
    Unknown(&'static str),
}

impl<T> Known<T> {
    pub fn known(&self) -> Option<&T> {
        match self {
            Known::Known(v) => Some(v),
            Known::Unknown(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Before the first heading: title, authors, front matter.
    Front,
    Abstract,
    Body,
    References,
    Appendix,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    pub kind: Kind,
    /// The heading line, trimmed, with single spaces; `null` without one.
    pub heading: Option<String>,
    /// 1 for a top heading, 2 for `2.1`, and so on; 0 without a heading.
    pub level: u8,
    pub first_line: usize,
    pub last_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Page {
    pub page: usize,
    pub first_line: usize,
}

/// One reference of the list, or with `split: unknown` one match in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub first_line: usize,
    pub last_line: usize,
    /// The registry key the reference cites; `null` when none matches.
    pub key: Option<String>,
    /// What matched: `doi`, `arxiv` or `title`.
    pub by: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefList {
    /// The list without its heading and without blank lines at its end.
    pub first_line: usize,
    pub last_line: usize,
    /// How the list splits into references: `numbered`, `blank` (blank lines
    /// between them) or `unknown`. With `unknown`, `entries` holds only the
    /// matches, each with the lines of the matched id or title.
    pub split: &'static str,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextInfo {
    /// Relative to the project root, as in the registry.
    pub path: String,
    /// Of the file as read.
    pub sha256: String,
    pub lines: usize,
    /// The recorded extraction mode, or `unknown`.
    pub extraction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Structure {
    pub schema: &'static str,
    pub text: TextInfo,
    pub generator: String,
    /// Page starts from the form feeds the extractor wrote; `unknown` without any.
    pub pages: Known<Vec<Page>>,
    /// In line order; together they cover every line once.
    pub sections: Vec<Section>,
    pub references: Known<RefList>,
    pub warnings: Vec<String>,
}

impl Structure {
    pub fn render(&self) -> String {
        let mut out = serde_json::to_string_pretty(self).expect("structure serializes");
        out.push('\n');
        out
    }

    /// One line for the user.
    pub fn summary(&self) -> String {
        let count = |kind| self.sections.iter().filter(|s| s.kind == kind).count();
        let refs = match &self.references {
            Known::Known(r) => {
                let cited = r.entries.iter().filter(|e| e.key.is_some()).count();
                format!(
                    "references lines {}-{} ({} split, {} entries, {cited} cite the registry)",
                    r.first_line,
                    r.last_line,
                    r.split,
                    r.entries.len()
                )
            }
            Known::Unknown(_) => "references unknown".to_owned(),
        };
        format!(
            "{}: {} lines, {} sections (abstract {}, body {}, appendix {}, unknown {}), {refs}",
            self.text.path,
            self.text.lines,
            self.sections.len(),
            count(Kind::Abstract),
            count(Kind::Body),
            count(Kind::Appendix),
            count(Kind::Unknown)
        )
    }
}

/// The sidecar's path for the text at `file`.
pub fn sidecar(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(SUFFIX);
    file.with_file_name(name)
}

/// True for a structure sidecar's path.
pub fn is_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(SUFFIX))
}

/// The registry entries a reference list can cite, by id and title.
#[derive(Debug, Default)]
pub struct Targets {
    doi: BTreeMap<String, String>,
    arxiv: BTreeMap<String, String>,
    /// (normalized title, key), longest title first.
    titles: Vec<(String, String)>,
}

/// A title shorter than this (normalized) matches too much to be evidence.
const MIN_TITLE: usize = 20;

impl Targets {
    /// `own`: the keys of the text's own entries, which it does not cite. An
    /// entry that is a duplicate gives its `duplicate_of` key.
    pub fn new(records: &[Reference], own: &[String]) -> Self {
        let mut out = Targets::default();
        for r in records {
            let key = r.duplicate_of.as_ref().unwrap_or(&r.key);
            if own.contains(key) || own.contains(&r.key) {
                continue;
            }
            if let Some(doi) = &r.ids.doi {
                out.doi
                    .entry(doi.trim().to_lowercase())
                    .or_insert_with(|| key.clone());
            }
            if let Some(id) = &r.ids.arxiv {
                out.arxiv
                    .entry(id.trim().to_lowercase())
                    .or_insert_with(|| key.clone());
            }
            if let Some(title) = &r.title {
                let k = text::key(title);
                if k.len() >= MIN_TITLE {
                    out.titles.push((k, key.clone()));
                }
            }
        }
        // Longest first; of two entries with one title, the first in the
        // registry, as for a DOI. The sort is stable.
        out.titles
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        out.titles.dedup_by(|later, first| later.0 == first.0);
        out
    }
}

/// Builds the structure of `content`. `markdown`: the text is Markdown, whose
/// `#` headings are then the only headings.
pub fn build(
    content: &str,
    info: TextInfo,
    markdown: bool,
    targets: &Targets,
    generator: String,
) -> Structure {
    let lines = split(content);
    let mut warnings = Vec::new();
    let pages = match pages(&lines) {
        Known::Unknown(_) if markdown => comment_pages(&lines),
        found => found,
    };
    let headings = match markdown {
        true => markdown_headings(&lines),
        false => plain_headings(&lines, &mut warnings),
    };
    let sections = sections(&lines, &headings, markdown, &mut warnings);
    let references = references(&lines, &sections, targets, &mut warnings);
    Structure {
        schema: SCHEMA,
        text: TextInfo {
            lines: lines.len(),
            ..info
        },
        generator,
        pages,
        sections,
        references,
        warnings,
    }
}

/// The lines as the file splits at LF; a final LF ends the last line.
fn split(content: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// A form feed starts a page at its line, unless only blank text follows it.
/// Pages without text are not listed, so the numbers can skip.
fn pages(lines: &[&str]) -> Known<Vec<Page>> {
    if !lines.iter().any(|l| l.contains('\u{c}')) {
        return Known::Unknown(UNKNOWN);
    }
    let mut out = vec![Page {
        page: 1,
        first_line: 1,
    }];
    // Each form feed ends a page, so the count gives the page's number.
    let mut number = 1;
    for (i, line) in lines.iter().enumerate() {
        for (at, _) in line.match_indices('\u{c}') {
            number += 1;
            let rest_blank = line[at + 1..].trim().is_empty()
                && lines[i + 1..].iter().all(|l| clean(l).is_empty());
            if rest_blank {
                continue;
            }
            let first_line = match line[at + 1..].trim().is_empty() {
                // The page's text starts on the next line.
                true if at > 0 => i + 2,
                _ => i + 1,
            };
            let last = out.last_mut().expect("page 1");
            // A page without text is not listed: the next page takes its place.
            // Its text would start after this line when an earlier form
            // feed of this line began it.
            let from = last.first_line - 1;
            let empty = from > i
                || lines[from..i].iter().all(|l| clean(l).is_empty())
                    && clean(&line[..at]).is_empty();
            if empty {
                last.page = number;
                last.first_line = first_line;
            } else {
                out.push(Page {
                    page: number,
                    first_line,
                });
            }
        }
    }
    Known::Known(out)
}

static PAGE_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^<!--\s*page\b[^>]*-->$").unwrap());

/// Markdown page marks: a line that is only an HTML comment starting with
/// `page`, such as `<!-- page -->` or `<!-- page break -->`. The next page
/// starts on the line after the mark.
fn comment_pages(lines: &[&str]) -> Known<Vec<Page>> {
    let marks: Vec<usize> = (0..lines.len())
        .filter(|&i| PAGE_COMMENT.is_match(lines[i].trim()))
        .collect();
    if marks.is_empty() {
        return Known::Unknown(UNKNOWN);
    }
    let mut out = vec![Page {
        page: 1,
        first_line: 1,
    }];
    for i in marks {
        let text_before = lines[..i].iter().any(|l| !clean(l).is_empty());
        let text_after = lines[i + 1..].iter().any(|l| !clean(l).is_empty());
        if text_before && text_after {
            out.push(Page {
                page: out.len() + 1,
                first_line: i + 2,
            });
        }
    }
    Known::Known(out)
}

/// A line for matching: no form feed, trimmed, single spaces.
fn clean(line: &str) -> String {
    text::squash(&line.replace('\u{c}', " "))
}

/// Lower case without whitespace and without a final `.` or `:`, so that
/// `R EFERENCES` and `Abstract.` compare as `references` and `abstract`.
fn tight(s: &str) -> String {
    let t: String = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    t.trim_end_matches(['.', ':']).to_owned()
}

#[derive(Debug, Clone)]
struct Heading {
    /// 0-based.
    line: usize,
    text: String,
    level: u8,
    kind: Kind,
}

const ABSTRACTS: [&str; 5] = ["abstract", "概要", "要旨", "あらまし", "摘要"];
const REFERENCES: [&str; 7] = [
    "references",
    "bibliography",
    "referencesandnotes",
    "literaturecited",
    "workscited",
    "参考文献",
    "引用文献",
];
/// Headings without a number that papers use, as written (Title Case or capitals).
const NAMED: [&str; 22] = [
    "introduction",
    "related work",
    "related works",
    "background",
    "preliminaries",
    "method",
    "methods",
    "methodology",
    "approach",
    "experiments",
    "experimental setup",
    "evaluation",
    "results",
    "discussion",
    "limitations",
    "conclusion",
    "conclusions",
    "conclusion and future work",
    "acknowledgments",
    "acknowledgements",
    "acknowledgment",
    "ethics statement",
];

static INLINE_ABSTRACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?i:abstract)\s*[—–:.\-]\s*\S").unwrap());
static NUMBER_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\d{1,2}|[IVXivx]{1,5})\.?\s+(.+)$").unwrap());
static NUMBERED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{1,2})((?:\.\d{1,2}){0,3})\.?\s+(\S.*)$").unwrap());
static BARE_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\d{1,2}(?:\.\d{1,2}){0,3}|[A-Z](?:\.\d{1,2}){0,2})\.?$").unwrap()
});
static ROMAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([IVX]{1,5})\.\s+(\S.*)$").unwrap());
static LETTERED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Z])((?:\.\d{1,2}){0,2})\.?\s+(\S.*)$").unwrap());
static APPENDIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?i:appendix|appendices|supplementary materials?|supplemental materials?)(?:\s+([A-Z]|\d{1,2}|[IVX]{1,4}))?(?:\s*([.:—–\-])\s*|\s+|$)(.*)$",
    )
    .unwrap()
});

fn is_references(clean: &str) -> bool {
    let t = tight(clean);
    if REFERENCES.contains(&t.as_str()) {
        return true;
    }
    NUMBER_PREFIX
        .captures(clean)
        .is_some_and(|c| REFERENCES.contains(&tight(&c[1]).as_str()))
}

/// True when `title` reads as a heading's words, not as a sentence or a table row.
fn is_title(title: &str) -> bool {
    let words: Vec<&str> = title.split(' ').collect();
    let numbers = words
        .iter()
        .filter(|w| w.trim_end_matches('%').parse::<f64>().is_ok())
        .count();
    let first = title.chars().next().is_some_and(|c| {
        c.is_uppercase() || (!c.is_ascii() && c.is_alphabetic() && !c.is_lowercase())
    });
    first
        && title.chars().count() <= 90
        && words.len() <= 12
        && numbers < 2
        && !title.ends_with(['.', ',', ';', ':'])
        && title.chars().filter(|c| c.is_alphabetic()).count() >= 2
}

/// True when most of the longer words start with a capital.
fn is_title_case(title: &str) -> bool {
    let long: Vec<&str> = title.split(' ').filter(|w| w.chars().count() > 3).collect();
    let capital = long
        .iter()
        .filter(|w| w.chars().next().is_some_and(char::is_uppercase))
        .count();
    !long.is_empty() && capital * 5 >= long.len() * 3
}

fn roman(s: &str) -> Option<u32> {
    let mut total = 0;
    let mut last = 0;
    for c in s.chars().rev() {
        let v = match c {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            _ => return None,
        };
        if v < last {
            total -= v;
        } else {
            total += v;
            last = v;
        }
    }
    u32::try_from(total).ok().filter(|v| *v > 0)
}

/// An appendix heading: the word alone or with its letter, or with a title
/// that reads as one. A sentence that starts with `Appendix A` is not one.
fn appendix(clean: &str, after_blank: bool) -> Option<Option<char>> {
    let c = APPENDIX.captures(clean)?;
    let letter = c
        .get(1)
        .and_then(|m| m.as_str().chars().next())
        .filter(char::is_ascii_uppercase);
    let title = c.get(3).map_or("", |m| m.as_str());
    let mark = c.get(2).map(|m| m.as_str());
    let ok = match (title.is_empty(), mark) {
        // `Appendix H.` ends a sentence.
        (true, Some(_)) => false,
        // `Appendix H` alone can be the end of a wrapped sentence.
        (true, None) => letter.is_none() || after_blank || clean == clean.to_uppercase(),
        (false, _) => {
            is_title(title)
                && clean.chars().count() <= 90
                && (is_title_case(title) || mark == Some(":") || after_blank)
        }
    };
    ok.then_some(letter)
}

/// The headings of a plain text, by pattern.
fn plain_headings(lines: &[&str], warnings: &mut Vec<String>) -> Vec<Heading> {
    let mut out: Vec<Heading> = Vec::new();
    // The last accepted top number, Arabic or Roman.
    let mut major = 0u32;
    let mut roman_major = 0u32;
    let mut in_refs = false;
    let mut in_appendix = false;
    let mut letter: Option<char> = None;
    let mut abstract_seen = false;
    // The line after a heading's number, which holds the heading's words.
    let mut joined = None;
    // The last line that reads as a references heading.
    let last_refs = lines.iter().rposition(|l| {
        let c = clean(l);
        c.chars().count() <= 40 && is_references(&c)
    });
    for (i, raw) in lines.iter().enumerate() {
        let mut c = clean(raw);
        if c.is_empty() || joined == Some(i) {
            continue;
        }
        // Some extractors put a heading's number and its words on two lines:
        // `A`, then `Verifier Details`. The words must read as a title.
        if BARE_NUMBER.is_match(&c)
            && let Some(next) = lines.get(i + 1).map(|l| clean(l))
            && is_title(&next)
            && (is_title_case(&next) || next.split(' ').count() <= 4)
        {
            c = format!("{c} {next}");
            joined = Some(i + 1);
        }
        let split = joined == Some(i + 1);
        let after_blank = i == 0 || clean(lines[i - 1]).is_empty() || raw.contains('\u{c}');
        let heading = |text: &str, level: u8, kind: Kind| Heading {
            line: i,
            text: text.to_owned(),
            level,
            kind,
        };
        if !abstract_seen && !in_refs && !in_appendix {
            if ABSTRACTS.contains(&tight(&c).as_str()) && c.chars().count() <= 20 {
                abstract_seen = true;
                out.push(heading(&c, 1, Kind::Abstract));
                continue;
            }
            if INLINE_ABSTRACT.is_match(&c) {
                abstract_seen = true;
                out.push(heading("Abstract", 1, Kind::Abstract));
                continue;
            }
        }
        if c.chars().count() <= 40 && is_references(&c) {
            in_refs = true;
            in_appendix = false;
            out.push(heading(&c, 1, Kind::References));
            continue;
        }
        if let Some(found) = appendix(&c, after_blank) {
            in_appendix = true;
            in_refs = false;
            letter = found.or(letter);
            out.push(heading(&c, 1, Kind::Appendix));
            continue;
        }
        // `1 Introduction` after a references heading that is not the last one:
        // that heading was a line of a table of contents.
        if (in_refs || in_appendix)
            && last_refs.is_some_and(|last| last > i)
            && let Some(m) = NUMBERED.captures(&c)
            && &m[1] == "1"
            && m[2].is_empty()
            && is_title(&m[3])
        {
            if let (Some(first), Some(last)) = (out.first(), out.last()) {
                warnings.push(format!(
                    "lines {}-{} read as a table of contents; its headings are not sections",
                    first.line + 1,
                    last.line + 1
                ));
            }
            out.retain(|h| h.kind == Kind::Abstract);
            (in_refs, in_appendix, letter, major) = (false, false, None, 1);
            out.push(heading(&c, 1, Kind::Body));
            continue;
        }
        if in_refs || in_appendix {
            // Lettered appendix sections: `A Proofs`, then `A.1 …`, `B …`.
            let Some(m) = LETTERED.captures(&c) else {
                continue;
            };
            let l = m[1].chars().next().expect("one letter");
            let depth = m[2].matches('.').count() as u8;
            let next = match letter {
                None => 'A',
                Some(last) => (last as u8 + 1) as char,
            };
            // `A. Smith and B. Jones. 2024. …` is a reference, not a heading.
            // A heading that wraps can end in a comma.
            let title = m[3].trim_end_matches(',');
            let named = matches!(title.to_lowercase().as_str(), "appendix" | "appendices");
            // In the list, a line right after a page break, with no blank
            // line between, can be the rest of a reference: the line before
            // it then ends in a word.
            let mid_reference = in_refs
                && i > 0
                && clean(lines[i - 1])
                    .chars()
                    .last()
                    .is_some_and(|ch| ch.is_lowercase() || matches!(ch, ',' | '-' | ':' | ';'));
            let in_order = match depth {
                0 => l == next && (split || named || (after_blank && !mid_reference)),
                _ => Some(l) == letter || (l == next && (in_appendix || after_blank)),
            };
            let words = !title.contains(['.', ',']) && !title.chars().any(|ch| ch.is_ascii_digit());
            if in_order && words && is_title(title) && (in_appendix || l == 'A') {
                in_appendix = true;
                in_refs = false;
                letter = Some(l);
                out.push(heading(&c, depth + 1, Kind::Appendix));
            }
            continue;
        }
        if let Some(m) = NUMBERED.captures(&c)
            && is_title(&m[3])
        {
            let n: u32 = m[1].parse().expect("digits");
            let depth = m[2].matches('.').count() as u8;
            if depth > 0 {
                if n == major && major > 0 {
                    out.push(heading(&c, depth + 1, Kind::Body));
                }
                continue;
            }
            if n == major + 1 {
                major = n;
                out.push(heading(&c, 1, Kind::Body));
                continue;
            }
            // `1` again after a close run of numbered headings: that run was a
            // table of contents.
            let numbered: Vec<usize> = out
                .iter()
                .filter(|h| h.kind == Kind::Body)
                .map(|h| h.line)
                .collect();
            if n == 1
                && major >= 2
                && let (Some(first), Some(last)) = (numbered.first(), numbered.last())
                && last - first <= 4 * numbered.len()
            {
                warnings.push(format!(
                    "lines {}-{} read as a table of contents; its headings are not sections",
                    first + 1,
                    last + 1
                ));
                out.retain(|h| h.kind == Kind::Abstract);
                major = 1;
                out.push(heading(&c, 1, Kind::Body));
            }
            continue;
        }
        if major == 0
            && let Some(m) = ROMAN.captures(&c)
            && is_title(&m[2])
            && roman(&m[1]) == Some(roman_major + 1)
        {
            roman_major += 1;
            out.push(heading(&c, 1, Kind::Body));
            continue;
        }
        let name = c.trim_end_matches(['.', ':']);
        let as_written = name == name.to_uppercase()
            || name
                .chars()
                .next()
                .is_some_and(|first| first.is_uppercase());
        if as_written && NAMED.contains(&name.to_lowercase().as_str()) {
            out.push(heading(&c, 1, Kind::Body));
        }
    }
    out
}

/// The `#` headings of a Markdown text, outside code fences and front matter.
fn markdown_headings(lines: &[&str]) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut fence = false;
    let mut after_refs = false;
    let start = front_matter(lines).map_or(0, |end| end + 1);
    for (i, raw) in lines.iter().enumerate().skip(start) {
        let c = clean(raw);
        if c.starts_with("```") || c.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        let level = c.chars().take_while(|ch| *ch == '#').count();
        if fence || level == 0 || level > 6 || !c[level..].starts_with(' ') {
            continue;
        }
        let title = c[level..].trim().trim_end_matches('#').trim();
        let kind = if ABSTRACTS.contains(&tight(title).as_str()) {
            Kind::Abstract
        } else if is_references(title) {
            after_refs = true;
            Kind::References
        } else if after_refs || title.to_lowercase().starts_with("appendi") {
            Kind::Appendix
        } else {
            Kind::Body
        };
        // A section number gives the depth where it is there: some converters
        // write every heading as `##`.
        let numbered = NUMBERED
            .captures(title)
            .map(|m| m[2].matches('.').count())
            .or_else(|| {
                (kind == Kind::Appendix)
                    .then(|| LETTERED.captures(title))
                    .flatten()
                    .map(|m| m[2].matches('.').count())
            });
        out.push(Heading {
            line: i,
            text: c.clone(),
            level: numbered.map_or(level, |depth| depth + 1) as u8,
            kind,
        });
    }
    out
}

/// The 0-based line that closes a YAML front matter that starts at line 1.
fn front_matter(lines: &[&str]) -> Option<usize> {
    (lines.first().map(|l| l.trim_end()) == Some("---"))
        .then(|| lines.iter().skip(1).position(|l| l.trim_end() == "---"))
        .flatten()
        .map(|p| p + 1)
}

/// An abstract that no heading ends within this many lines ends at its first
/// blank line, or here.
const ABSTRACT_MAX: usize = 80;

fn sections(
    lines: &[&str],
    headings: &[Heading],
    markdown: bool,
    warnings: &mut Vec<String>,
) -> Vec<Section> {
    let span = |kind, first: usize, last: usize| Section {
        kind,
        heading: None,
        level: 0,
        first_line: first + 1,
        last_line: last + 1,
    };
    if lines.is_empty() {
        warnings.push("the text is empty".to_owned());
        return Vec::new();
    }
    let Some(first) = headings.first() else {
        warnings.push("no section headings detected; the whole text is one unknown span".into());
        let matter = front_matter(lines).filter(|_| markdown);
        return match matter {
            Some(end) if end + 1 < lines.len() => vec![
                span(Kind::Front, 0, end),
                span(Kind::Unknown, end + 1, lines.len() - 1),
            ],
            _ => vec![span(Kind::Unknown, 0, lines.len() - 1)],
        };
    };
    let mut out = Vec::new();
    if first.line > 0 {
        out.push(span(Kind::Front, 0, first.line - 1));
    }
    for (n, h) in headings.iter().enumerate() {
        let end = headings.get(n + 1).map_or(lines.len(), |next| next.line) - 1;
        let mut section = Section {
            kind: h.kind,
            heading: Some(h.text.clone()),
            level: h.level,
            first_line: h.line + 1,
            last_line: end + 1,
        };
        if h.kind == Kind::Abstract && end - h.line >= ABSTRACT_MAX {
            // The first blank line after the abstract's text, else the cap.
            let text_from = (h.line..=end)
                .find(|&i| i > h.line || h.text == "Abstract" && clean(lines[i]) != "Abstract")
                .and_then(|from| (from..=end).find(|&i| !clean(lines[i]).is_empty()));
            let cut = text_from
                .and_then(|from| (from..=end).find(|&i| clean(lines[i]).is_empty()))
                .filter(|&blank| blank - h.line <= ABSTRACT_MAX)
                .unwrap_or(h.line + ABSTRACT_MAX);
            section.last_line = cut;
            warnings.push(format!(
                "no heading ends the abstract; lines {}-{} are unknown",
                cut + 1,
                end + 1
            ));
            out.push(section);
            out.push(span(Kind::Unknown, cut, end));
            continue;
        }
        out.push(section);
    }
    if !headings.iter().any(|h| h.kind == Kind::Abstract) {
        warnings.push("no abstract heading detected".to_owned());
    }
    out
}

/// `[12]`, also as a Markdown list item: `- [12]`.
static BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[-*+]\s+)?\[(\d{1,3})\]").unwrap());
static DOTTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d{1,3})\.\s+\S").unwrap());
static DOI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"10\.\d{4,9}/[^\s"<>]+"#).unwrap());
/// A DOI that the line end may divide: its last token, possibly only
/// `10.18653/`.
static DOI_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"10\.\d{4,9}/[^\s"<>]*$"#).unwrap());
/// A line that starts a reference of an author-year list: a name of
/// capitalised words, initials and particles, then a comma (`Asai, A.` or
/// `Akari Asai, Zeqiu Wu`), `and` and a second name, or a full stop and a year
/// (`Meta AI. 2025.`). A Markdown list item's `- ` may come first.
static AUTHOR: LazyLock<Regex> = LazyLock::new(|| {
    let name = r"\p{Lu}[\p{L}\p{M}'’\-]*\.?";
    let part = format!(r"(?:{name}|van|von|der|den|de|del|della|da|di|du|dos|le|la|ten|ter)");
    Regex::new(&format!(
        r"^\s*(?:[-*+]\s+)?{name}(?:[ \t]+{part}){{0,4}}(?:,|\.?[ \t]+(?:19|20)\d{{2}}|[ \t]+and[ \t]+\p{{Lu}})"
    ))
    .unwrap()
});
/// A year as references give it: `2023`, `2023a`.
static YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\d])(?:19|20)\d{2}[a-z]?(?:[^\d]|$)").unwrap());
/// A Markdown list item.
static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[-*+]\s").unwrap());
static ARXIV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:arxiv:\s*|arxiv\.org/(?:abs|pdf)/|abs/)(\d{4}\.\d{4,5}|[a-z\-]+(?:\.[a-z]{2})?/\d{7})")
        .unwrap()
});

/// A match of a registry entry in the list: 0-based lines.
struct Match {
    first: usize,
    last: usize,
    key: String,
    by: &'static str,
}

fn rank(by: &str) -> u8 {
    match by {
        "doi" => 0,
        "arxiv" => 1,
        _ => 2,
    }
}

/// The reference list: the longest `references` section.
fn references(
    lines: &[&str],
    sections: &[Section],
    targets: &Targets,
    warnings: &mut Vec<String>,
) -> Known<RefList> {
    let Some(section) = sections
        .iter()
        .filter(|s| s.kind == Kind::References)
        .max_by_key(|s| s.last_line - s.first_line)
    else {
        if !sections.is_empty() && sections.iter().any(|s| s.heading.is_some()) {
            warnings.push("no reference list heading detected".to_owned());
        }
        return Known::Unknown(UNKNOWN);
    };
    // 0-based, without the heading and without blank lines or page marks at
    // either end.
    let filled = |i: &usize| listed(lines[*i]);
    let range = section.first_line..section.last_line;
    let (Some(first), Some(last)) = (
        range.clone().find(|i| filled(i)),
        range.clone().rev().find(|i| filled(i)),
    ) else {
        warnings.push("the reference list is empty".to_owned());
        return Known::Unknown(UNKNOWN);
    };
    let matches = matches(lines, first, last, targets);
    let starts = entry_starts(lines, first, last);
    let mut last = last;
    let (split, entries) = match starts {
        Some((split, starts, end)) => {
            last = end;
            let stop = |n: usize| starts.get(n + 1).map_or(last, |next| next - 1);
            let mut entries = Vec::with_capacity(starts.len());
            for (n, &start) in starts.iter().enumerate() {
                let stop = stop(n);
                let end = (start..=stop).rev().find(|i| filled(i)).unwrap_or(start);
                let best = matches
                    .iter()
                    .filter(|m| (start..=stop).contains(&m.first))
                    .min_by_key(|m| (rank(m.by), m.first));
                entries.push(Entry {
                    first_line: start + 1,
                    last_line: end + 1,
                    key: best.map(|m| m.key.clone()),
                    by: best.map(|m| m.by),
                });
            }
            (split, entries)
        }
        None => {
            warnings.push(
                "the reference list does not split into references; only matches are listed".into(),
            );
            let mut found: Vec<&Match> = matches.iter().collect();
            found.sort_by_key(|m| (m.first, rank(m.by)));
            let mut entries: Vec<Entry> = Vec::new();
            for m in found {
                if !entries.iter().any(|e| e.key.as_deref() == Some(&m.key)) {
                    entries.push(Entry {
                        first_line: m.first + 1,
                        last_line: m.last + 1,
                        key: Some(m.key.clone()),
                        by: Some(m.by),
                    });
                }
            }
            (UNKNOWN, entries)
        }
    };
    Known::Known(RefList {
        first_line: first + 1,
        last_line: last + 1,
        split,
        entries,
    })
}

/// How the list splits, where each reference starts, and the list's last
/// line: at `[n]` or `n.` marks, else after blank lines, else at authors'
/// names (see [`author_year`]). A list without marks ends at its last
/// reference with a year (see [`dated`]).
fn entry_starts(
    lines: &[&str],
    first: usize,
    last: usize,
) -> Option<(&'static str, Vec<usize>, usize)> {
    let bracket: Vec<usize> = (first..=last)
        .filter(|&i| BRACKET.is_match(lines[i]))
        .collect();
    // Marks that do not start at the top of the list are not its references.
    let at_top = |starts: &[usize]| starts.first().is_some_and(|s| *s <= first + 2);
    if bracket.len() >= 3 && at_top(&bracket) {
        let end = close(lines, &bracket, last);
        return Some(("numbered", bracket, end));
    }
    let mut dotted = Vec::new();
    for (i, line) in lines.iter().enumerate().take(last + 1).skip(first) {
        if DOTTED
            .captures(line)
            .is_some_and(|c| c[1].parse() == Ok(dotted.len() + 1))
        {
            dotted.push(i);
        }
    }
    if dotted.len() >= 3 && at_top(&dotted) {
        let end = close(lines, &dotted, last);
        return Some(("numbered", dotted, end));
    }
    let blank = |i: usize| !listed(lines[i]);
    let paragraphs: Vec<usize> = (first..=last)
        .filter(|&i| !blank(i) && (i == first || blank(i - 1) || ITEM.is_match(lines[i])))
        .collect();
    let (paragraphs, end) = dated(lines, paragraphs, last);
    // References of one to a few lines each, not pages of text between blanks.
    if paragraphs.len() >= 3 && (end + 1 - first) <= paragraphs.len() * 8 {
        return Some(("blank", paragraphs, end));
    }
    author_year(lines, first, last).map(|(starts, end)| ("author-year", starts, end))
}

/// `starts` without its last references that have no year, and the last
/// filled line of the rest: what follows a list without marks, such as table
/// rows or an appendix's contents, is not a reference. The first stays.
fn dated(lines: &[&str], mut starts: Vec<usize>, mut last: usize) -> (Vec<usize>, usize) {
    while let [.., before, start] = starts[..] {
        if has_year(lines, start, last) {
            break;
        }
        starts.pop();
        last = (before..start)
            .rev()
            .find(|&i| listed(lines[i]))
            .unwrap_or(before);
    }
    let last = close(lines, &starts, last);
    (starts, last)
}

/// The list's last line: a last reference longer than every other runs on
/// into what follows the list, such as an appendix or a table, so it ends
/// before its first blank line.
fn close(lines: &[&str], starts: &[usize], last: usize) -> usize {
    let filled = |i: &usize| listed(lines[*i]);
    let Some((&start, others)) = starts.split_last() else {
        return last;
    };
    let longest = (0..others.len())
        .map(|n| starts[n + 1] - starts[n])
        .max()
        .unwrap_or(0);
    if others.is_empty() || last + 1 - start <= longest {
        return last;
    }
    (start..=last)
        .find(|i| !filled(i))
        .and_then(|blank| (start..blank).rev().find(filled))
        .unwrap_or(last)
}

/// Whether `line` is text in a reference list: not blank and not a page mark.
fn listed(line: &str) -> bool {
    !clean(line).is_empty() && !PAGE_COMMENT.is_match(line.trim())
}

/// Whether `line` can end a reference: a full stop, or a DOI or URL at its end.
fn ends_reference(line: &str) -> bool {
    let line = clean(line);
    line.ends_with('.')
        || (!line.ends_with('-')
            && line
                .rsplit(' ')
                .next()
                .is_some_and(|w| DOI.is_match(w) || w.starts_with("http")))
}

/// Whether lines `from..=to` hold a year.
fn has_year(lines: &[&str], from: usize, to: usize) -> bool {
    (from..=to).any(|i| YEAR.is_match(lines[i]))
}

/// The starts of an author-year list without numbers or blank lines between
/// references: the list's first line and each line that reads as a name (see
/// [`AUTHOR`]) after a line that ends a reference. The list ends at its last
/// reference with a year (see [`dated`]); before it, a start whose reference
/// has no year was not a start, unless it opens a Markdown list item. `None` unless the first line reads as a name,
/// there are 3 references or more, and they average at most 8 lines. Returns
/// the starts and the list's last line.
fn author_year(lines: &[&str], first: usize, last: usize) -> Option<(Vec<usize>, usize)> {
    // Page marks and blank lines are not text.
    let filled: Vec<usize> = (first..=last)
        .filter(|&i| listed(lines[i]) && !clean(lines[i]).bytes().all(|b| b.is_ascii_digit()))
        .collect();
    if !AUTHOR.is_match(lines[first]) {
        return None;
    }
    let mut starts = vec![first];
    for pair in filled.windows(2) {
        if ends_reference(lines[pair[0]]) && AUTHOR.is_match(lines[pair[1]]) {
            starts.push(pair[1]);
        }
    }
    let (starts, last) = dated(lines, starts, last);
    let stop = |n: usize| starts.get(n + 1).map_or(last, |next| next - 1);
    let kept: Vec<usize> = (0..starts.len())
        .filter(|&n| {
            n == 0 || ITEM.is_match(lines[starts[n]]) || has_year(lines, starts[n], stop(n))
        })
        .map(|n| starts[n])
        .collect();
    (kept.len() >= 3 && last + 1 - first <= kept.len() * 8).then_some((kept, last))
}

/// The registry entries that lines `first..=last` name by DOI, arXiv id or title.
fn matches(lines: &[&str], first: usize, last: usize, targets: &Targets) -> Vec<Match> {
    let mut out = Vec::new();
    // The normalized text, with the line of each of its bytes. Normalizing
    // drops hyphens and line ends, so a title broken across lines still matches.
    let mut normal = String::new();
    let mut line_of: Vec<usize> = Vec::new();
    let doi_hit = |doi: &str| {
        let doi = doi
            .trim_end_matches(['.', ',', ';', ')', ']'])
            .to_lowercase();
        targets.doi.get(&doi).map(|k| (k, "doi")).or_else(|| {
            doi.strip_prefix("10.48550/arxiv.")
                .and_then(|id| targets.arxiv.get(id))
                .map(|k| (k, "arxiv"))
        })
    };
    for (i, line) in lines.iter().enumerate().take(last + 1).skip(first) {
        let mut ends_with_hit = false;
        for m in DOI.find_iter(line) {
            if let Some((key, by)) = doi_hit(m.as_str()) {
                ends_with_hit |= m.end() == line.trim_end().len();
                out.push(Match {
                    first: i,
                    last: i,
                    key: key.clone(),
                    by,
                });
            }
        }
        // A DOI that the line end divides: joined with the next line's first
        // word, with a final hyphen kept or dropped, as the registry has it.
        let next_word = lines
            .get(i + 1)
            .filter(|_| i < last)
            .and_then(|l| l.split_whitespace().next());
        if let (Some(tail), Some(word), false) =
            (DOI_TAIL.find(line.trim_end()), next_word, ends_with_hit)
        {
            let tail = tail.as_str();
            let unhyphenated = tail.strip_suffix('-').map(|t| format!("{t}{word}"));
            let hit = doi_hit(&format!("{tail}{word}"))
                .or_else(|| unhyphenated.as_deref().and_then(doi_hit));
            if let Some((key, by)) = hit {
                out.push(Match {
                    first: i,
                    last: i + 1,
                    key: key.clone(),
                    by,
                });
            }
        }
        for c in ARXIV.captures_iter(line) {
            if let Some(key) = targets.arxiv.get(&c[1].to_lowercase()) {
                out.push(Match {
                    first: i,
                    last: i,
                    key: key.clone(),
                    by: "arxiv",
                });
            }
        }
        let k = text::key(line);
        line_of.resize(line_of.len() + k.len(), i);
        normal.push_str(&k);
    }
    // Longest titles first; a shorter title inside a matched one does not count.
    let mut taken = vec![false; normal.len()];
    for (title, key) in &targets.titles {
        for (at, _) in normal.match_indices(title.as_str()) {
            let end = at + title.len();
            if taken[at..end].iter().any(|t| *t) {
                continue;
            }
            taken[at..end].fill(true);
            out.push(Match {
                first: line_of[at],
                last: line_of[end - 1],
                key: key.clone(),
                by: "title",
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> TextInfo {
        TextInfo {
            path: "texts/a.txt".into(),
            sha256: "0".repeat(64),
            lines: 0,
            extraction: UNKNOWN.into(),
        }
    }

    fn structure(content: &str, markdown: bool, targets: &Targets) -> Structure {
        build(content, info(), markdown, targets, "papers test".into())
    }

    fn spans(s: &Structure) -> Vec<(Kind, usize, usize)> {
        s.sections
            .iter()
            .map(|x| (x.kind, x.first_line, x.last_line))
            .collect()
    }

    fn entry(key: &str, doi: Option<&str>, arxiv: Option<&str>, title: &str) -> Reference {
        let mut r = Reference::new(key.into());
        r.ids.doi = doi.map(Into::into);
        r.ids.arxiv = arxiv.map(Into::into);
        r.title = Some(title.into());
        r
    }

    const PAPER: &str = include_str!("../tests/fixtures/structure-paper.txt");

    #[test]
    fn marks_abstract_body_references_and_appendix_and_covers_every_line() {
        let s = structure(PAPER, false, &Targets::default());
        let lines: Vec<&str> = PAPER.lines().collect();
        let at = |needle: &str| lines.iter().position(|l| l.trim() == needle).unwrap() + 1;
        let refs = at("References");
        let wrapped = at("B Prompts for Widgets,");
        assert_eq!(
            spans(&s),
            vec![
                (Kind::Front, 1, at("Abstract") - 1),
                (Kind::Abstract, at("Abstract"), at("1 Introduction") - 1),
                (Kind::Body, at("1 Introduction"), at("2 Method") - 1),
                (Kind::Body, at("2 Method"), at("2.1 Synthetic Quotes") - 1),
                (Kind::Body, at("2.1 Synthetic Quotes"), at("3") - 1),
                (Kind::Body, at("3"), refs - 1),
                (Kind::References, refs, at("A") - 1),
                (Kind::Appendix, at("A"), at("A.1 Details") - 1),
                (Kind::Appendix, at("A.1 Details"), wrapped - 1),
                (Kind::Appendix, wrapped, lines.len()),
            ]
        );
        // A number and its words on two lines are one heading.
        assert_eq!(s.sections[5].heading.as_deref(), Some("3 Results"));
        assert_eq!(s.sections[7].heading.as_deref(), Some("A Proofs"));
        assert_eq!(s.sections[4].level, 2);
        assert_eq!(s.sections[2].heading.as_deref(), Some("1 Introduction"));
        // Contiguous cover.
        for pair in s.sections.windows(2) {
            assert_eq!(pair[0].last_line + 1, pair[1].first_line);
        }
        assert_eq!(s.text.lines, lines.len());
        assert_eq!(s.pages, Known::Unknown("unknown"));
        // A list item `4 Widgets were …`, `Appendix A.` in a sentence and
        // `Appendix H` at the end of a wrapped line are not headings.
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn reference_entries_cite_registry_keys_by_doi_arxiv_and_title() {
        let records = [
            entry(
                "12",
                Some("10.1234/Synthetic.12"),
                None,
                "Unrelated Title Of Some Length",
            ),
            entry(
                "15",
                None,
                Some("2601.99915"),
                "Another Unrelated Title Here",
            ),
            entry(
                "20",
                None,
                None,
                "Rebalancing Costs of a Synthetic Market-Maker",
            ),
            // A duplicate gives its target; the text's own entry is not cited.
            {
                let mut r = entry("21", Some("10.1234/synthetic.21"), None, "x");
                r.duplicate_of = Some("15".into());
                r
            },
            entry(
                "87",
                None,
                None,
                "A Synthetic Paper About Widgets And Quotes",
            ),
        ];
        let targets = Targets::new(&records, &["87".into()]);
        let s = structure(PAPER, false, &targets);
        let list = s.references.known().expect("a reference list");
        let lines: Vec<&str> = PAPER.lines().collect();
        let at = |start: &str| lines.iter().position(|l| l.starts_with(start)).unwrap() + 1;
        assert_eq!(list.split, "numbered");
        assert_eq!(list.first_line, at("[1]"));
        assert_eq!(list.last_line, at("    Example Press."));
        let got: Vec<(usize, usize, Option<&str>, Option<&str>)> = list
            .entries
            .iter()
            .map(|e| (e.first_line, e.last_line, e.key.as_deref(), e.by))
            .collect();
        assert_eq!(
            got,
            vec![
                (at("[1]"), at("[2]") - 1, Some("12"), Some("doi")),
                (at("[2]"), at("[3]") - 1, Some("15"), Some("arxiv")),
                (at("[3]"), at("[4]") - 1, Some("20"), Some("title")),
                (at("[4]"), at("[5]") - 1, Some("15"), Some("doi")),
                (at("[5]"), at("[5]") + 1, None, None),
            ]
        );
    }

    #[test]
    fn a_text_without_headings_is_one_unknown_span_with_a_warning() {
        let s = structure(
            "some words\n\nmore words\nand more\n",
            false,
            &Targets::default(),
        );
        assert_eq!(spans(&s), vec![(Kind::Unknown, 1, 4)]);
        assert_eq!(s.references, Known::Unknown("unknown"));
        assert_eq!(s.warnings.len(), 1);
        assert!(
            s.warnings[0].contains("no section headings"),
            "{:?}",
            s.warnings
        );
        let json: serde_json::Value = serde_json::from_str(&s.render()).unwrap();
        assert_eq!(json["schema"], "rf.paper-structure/1");
        assert_eq!(json["pages"], "unknown");
        assert_eq!(json["references"], "unknown");
        assert_eq!(json["sections"][0]["kind"], "unknown");
        assert_eq!(json["sections"][0]["heading"], serde_json::Value::Null);
    }

    #[test]
    fn form_feeds_give_page_starts() {
        let s = structure(
            "one\ntwo\n\u{c}three\nfour\n\u{c}",
            false,
            &Targets::default(),
        );
        assert_eq!(
            s.pages,
            Known::Known(vec![
                Page {
                    page: 1,
                    first_line: 1
                },
                Page {
                    page: 2,
                    first_line: 3
                },
            ])
        );
    }

    #[test]
    fn markdown_uses_its_own_headings_and_front_matter() {
        let text = "---\nlicense: mit\n---\n# Card\n\n```\n# not a heading\n```\n\n## Abstract\n\nwords\n\n## References\n\nSmith. A Title.\n";
        let s = structure(text, true, &Targets::default());
        assert_eq!(
            spans(&s),
            vec![
                (Kind::Front, 1, 3),
                (Kind::Body, 4, 9),
                (Kind::Abstract, 10, 13),
                (Kind::References, 14, 16),
            ]
        );
        assert_eq!(s.sections[2].level, 2);
        assert_eq!(s.pages, Known::Unknown("unknown"));
    }

    #[test]
    fn a_contents_line_that_reads_references_does_not_hide_the_sections() {
        let text = "Title\n\nContents\nIntroduction\n1\nReferences\n9\n\n1 Introduction\nwords\n\n2 Method\nwords\n\nReferences\n[1] Smith. A Title.\n";
        let s = structure(text, false, &Targets::default());
        assert_eq!(
            spans(&s),
            vec![
                (Kind::Front, 1, 8),
                (Kind::Body, 9, 11),
                (Kind::Body, 12, 14),
                (Kind::References, 15, 16),
            ]
        );
        assert!(s.warnings.iter().any(|w| w.contains("table of contents")));
    }

    #[test]
    fn a_reference_that_wraps_over_a_page_break_is_not_an_appendix_heading() {
        let text = "1 Introduction\nwords\n\nReferences\nSmith, J. 2024. Widgets and\n\u{c}A Survey of Synthetic Models\nfor Markets. Example Press.\n\nJones, K. 2023. Other Work.\n\u{c}A Proofs\nwords\n";
        let s = structure(text, false, &Targets::default());
        assert_eq!(
            spans(&s),
            vec![
                (Kind::Body, 1, 3),
                (Kind::References, 4, 9),
                (Kind::Appendix, 10, 11),
            ]
        );
    }

    #[test]
    fn a_page_without_text_keeps_its_number() {
        let page = |page, first_line| Page { page, first_line };
        let s = structure(
            "one\n\u{c}two\n\u{c}\u{c}four\n\u{c}",
            false,
            &Targets::default(),
        );
        assert_eq!(
            s.pages,
            Known::Known(vec![page(1, 1), page(2, 2), page(4, 3)])
        );
        let s = structure("\u{c}two\n", false, &Targets::default());
        assert_eq!(s.pages, Known::Known(vec![page(2, 1)]));
        let s = structure("one\u{c}\u{c}\nthree\n", false, &Targets::default());
        assert_eq!(s.pages, Known::Known(vec![page(1, 1), page(3, 2)]));
    }

    #[test]
    fn two_entries_with_one_title_give_the_first_in_the_registry() {
        let title = "A Synthetic Paper About Widgets And Quotes";
        let records = [
            entry("9", None, None, title),
            entry("10", None, None, title),
        ];
        let text = "References\n[1] X. A synthetic paper about widgets and quotes. 2024.\n[2] Y. Other.\n[3] Z. More.\n";
        let s = structure(text, false, &Targets::new(&records, &[]));
        let list = s.references.known().unwrap();
        assert_eq!(list.entries[0].key.as_deref(), Some("9"));
    }

    #[test]
    fn markdown_takes_levels_from_section_numbers_and_pages_from_comments() {
        let text = "## A Title

## 1 Introduction

words

<!-- page break -->

## 1.1 Scope

words

## References

- Smith. A Title.

<!-- page -->

## A.2 Proofs

words
<!-- page -->
";
        let s = structure(text, true, &Targets::default());
        let levels: Vec<(Kind, u8)> = s.sections.iter().map(|s| (s.kind, s.level)).collect();
        assert_eq!(
            levels,
            [
                (Kind::Body, 2),
                (Kind::Body, 1),
                (Kind::Body, 2),
                (Kind::References, 2),
                (Kind::Appendix, 2),
            ]
        );
        // The last mark has no text after it.
        let page = |page, first_line| Page { page, first_line };
        assert_eq!(
            s.pages,
            Known::Known(vec![page(1, 1), page(2, 8), page(3, 18)])
        );
    }

    #[test]
    fn a_table_of_contents_is_not_the_sections_and_spaced_capitals_match() {
        let text = "Title\n\nContents\n1 Introduction\n2 Method\n3 Results\n\nA BSTRACT\nwords\n\n1 I NTRODUCTION\nwords\n2 M ETHOD\nwords\nR EFERENCES\nSmith. A Title.\n";
        let s = structure(text, false, &Targets::default());
        assert_eq!(
            spans(&s),
            vec![
                (Kind::Front, 1, 7),
                (Kind::Abstract, 8, 10),
                (Kind::Body, 11, 12),
                (Kind::Body, 13, 14),
                (Kind::References, 15, 16),
            ]
        );
        assert!(s.warnings.iter().any(|w| w.contains("table of contents")));
    }

    #[test]
    fn an_abstract_without_a_following_heading_ends_at_its_blank_line() {
        let mut text = String::from("Title\nAbstract—words of the abstract\nmore words\n\n");
        for _ in 0..100 {
            text.push_str("body words without any heading\n");
        }
        let s = structure(&text, false, &Targets::default());
        assert_eq!(
            spans(&s),
            vec![
                (Kind::Front, 1, 1),
                (Kind::Abstract, 2, 3),
                (Kind::Unknown, 4, 104),
            ]
        );
        assert!(
            s.warnings
                .iter()
                .any(|w| w.contains("lines 4-104 are unknown"))
        );
    }

    #[test]
    fn an_unsplit_list_holds_only_its_matches() {
        let records = [entry(
            "20",
            None,
            None,
            "Rebalancing Costs of a Synthetic Market-Maker",
        )];
        let text = "1 Introduction\nwords\nReferences\nA. Smith and B. Jones. 2024. Rebalancing Costs of a Syn-\nthetic Market-Maker. In Proceedings.\nC. Lee. 2023. Something Else. Journal.\n";
        let s = structure(text, false, &Targets::new(&records, &[]));
        let list = s.references.known().unwrap();
        assert_eq!(
            (list.first_line, list.last_line, list.split),
            (4, 6, "unknown")
        );
        assert_eq!(
            list.entries,
            vec![Entry {
                first_line: 4,
                last_line: 5,
                key: Some("20".into()),
                by: Some("title"),
            }]
        );
    }

    fn entry_spans(list: &RefList) -> Vec<(usize, usize, Option<&str>, Option<&str>)> {
        list.entries
            .iter()
            .map(|e| (e.first_line, e.last_line, e.key.as_deref(), e.by))
            .collect()
    }

    #[test]
    fn an_author_year_list_without_blank_lines_splits_at_names_and_ends_at_its_last_reference() {
        let records = [entry(
            "157",
            None,
            None,
            "RARE: Redundancy-Aware Retrieval Evaluation for Synthetic Corpora",
        )];
        // Line 10 reads as a name but its reference has no year; line 11 is a
        // page number; an appendix's contents follow without a heading.
        let text = "1 Introduction
words
References
Cho, H.; and Lee, J. 2026. RARE: Redundancy-Aware
Retrieval Evaluation for Synthetic Corpora. In Proceedings.
Chen, Z.; Smith, A.; and Wang, W. Y. 2021. FinQA: A
Dataset. Transactions on Data.
María Andrea Cruz Blandón and Bruno Charron. 2025.
MEMERAG: A benchmark. In Proceedings, pages 1–9.
Preprint, arXiv:2509.03787.
12
Gordon V. Cormack and Stefan Buettcher. 2009. Rank
fusion. In Proceedings of SIGIR.
Lee, K. 2023. Other Work. Journal.

Technical Appendix

A.1 Example 1 . . . . . . . . 13

A.2 Example 2 . . . . . . . . 14
";
        let s = structure(text, false, &Targets::new(&records, &[]));
        let list = s.references.known().expect("a reference list");
        assert_eq!(
            (list.first_line, list.last_line, list.split),
            (4, 14, "author-year")
        );
        assert_eq!(
            entry_spans(list),
            vec![
                (4, 5, Some("157"), Some("title")),
                (6, 7, None, None),
                (8, 11, None, None),
                (12, 13, None, None),
                (14, 14, None, None),
            ]
        );
    }

    #[test]
    fn a_list_followed_by_a_table_ends_at_its_last_reference() {
        let text = "1 Introduction
words
References
[1] A. Smith. 2024. One. Journal.
[2] B. Jones. 2023. Two. Journal.
[3] C. Lee. 2022. Three. Journal.

Table 5: Results
Model  Score
GPT    0.91
Claude 0.95
";
        let s = structure(text, false, &Targets::default());
        let list = s.references.known().unwrap();
        assert_eq!(
            (list.first_line, list.last_line, list.split),
            (4, 6, "numbered")
        );
        assert_eq!(
            entry_spans(list),
            vec![(4, 4, None, None), (5, 5, None, None), (6, 6, None, None)]
        );

        // Markdown: each list item is a reference, a page mark is not, and the
        // table after the list has no year.
        let text = "## References

- Smith, A. 2024. One. Journal.
- Jones, B. 2023. Two. Journal.

<!-- page -->

- Lee, C. 2022. Three. Journal.

| Model | Score |
|---|---|
| GPT | 0.91 |
";
        let s = structure(text, true, &Targets::default());
        let list = s.references.known().unwrap();
        assert_eq!(
            (list.first_line, list.last_line, list.split),
            (3, 8, "blank")
        );
        assert_eq!(
            entry_spans(list),
            vec![(3, 3, None, None), (4, 4, None, None), (8, 8, None, None)]
        );
    }

    #[test]
    fn a_doi_divided_by_a_line_end_matches_and_a_paper_title_beats_its_venue() {
        let records = [
            entry(
                "6",
                None,
                None,
                "Proceedings of the Synthetic Conference on Widgets and Markets",
            ),
            entry("1", Some("10.18653/v1/2023.acl-long.137"), None, "x"),
            entry("2", Some("10.1234/synthetic.5"), None, "y"),
            entry("3", Some("10.1234/a-b"), None, "z"),
            entry(
                "34",
                None,
                None,
                "MEMERAG: A Multilingual Benchmark for Synthetic Widgets",
            ),
        ];
        let text = "References
[1] A. Smith. 2023. Widgets. In Proceedings of ACL. https://doi.org/10.18653/
v1/2023.acl-long.137.
[2] B. Jones. 2024. Hyphen. https://doi.org/10.1234/syn-
thetic.5
[3] C. Lee. 2022. Kept. https://doi.org/10.1234/a-
b.
[4] D. Cruz. 2025. MEMERAG: A multilingual benchmark for synthetic
widgets. In Proceedings of the Synthetic Conference on Widgets and
Markets, pages 1–9.
";
        let s = structure(text, false, &Targets::new(&records, &[]));
        let list = s.references.known().unwrap();
        assert_eq!(
            entry_spans(list),
            vec![
                (2, 3, Some("1"), Some("doi")),
                (4, 5, Some("2"), Some("doi")),
                (6, 7, Some("3"), Some("doi")),
                (8, 10, Some("34"), Some("title")),
            ]
        );
    }
}
