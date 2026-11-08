//! One-note-per-paper archives: frontmatter, verification, and stubs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::atom::Paper;
use crate::refs::{Doi, Ref};
use crate::text::{key, surname, title_slug};

/// The frontmatter fields this tool reads; other fields are ignored.
#[derive(Debug, Deserialize)]
pub struct Meta {
    pub id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub url: Option<String>,
    pub doi: Option<String>,
    /// Date the metadata was confirmed by hand, for papers no index lists.
    pub checked: Option<String>,
    /// Earlier titles of the same paper (indexes often keep the first posting's).
    #[serde(default, deserialize_with = "one_or_many")]
    pub former_titles: Vec<String>,
    /// Spellings an index uses for the first author when it differs from the
    /// paper itself (a misspelt name in arXiv's or Crossref's metadata).
    #[serde(default, deserialize_with = "one_or_many")]
    pub author_variants: Vec<String>,
    /// Reading status, mapped to a registry depth by `[papers] status_depth`.
    pub status: Option<String>,
    /// Registry depth set by hand; it overrides the status mapping.
    pub depth: Option<String>,
}

impl Meta {
    /// The note's own `depth`, or an error when it is outside the
    /// `rf.reference/1` vocabulary.
    pub fn explicit_depth(&self) -> Result<Option<&str>, String> {
        match self.depth.as_deref() {
            Some(d) if !crate::registry::DEPTHS.contains(&d) => Err(format!(
                "depth {d:?} is not one of {}",
                crate::registry::DEPTHS.join(", ")
            )),
            d => Ok(d),
        }
    }
}

/// A list of strings, a single string as a one-item list, or nothing.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        None => Vec::new(),
        Some(OneOrMany::One(s)) => vec![s],
        Some(OneOrMany::Many(v)) => v,
    })
}

pub struct Note {
    pub path: PathBuf,
    pub meta: Result<Meta, String>,
}

impl Note {
    /// The work the note cites: an arXiv abs `url`; else its `doi` field; else
    /// a DOI derived from its `url` (doi.org, SSRN, NBER). `None` for other pages.
    pub fn reference(&self) -> Option<Ref> {
        let meta = self.meta.as_ref().ok()?;
        let from_url = meta.url.as_deref().and_then(Ref::from_note_url);
        if let Some(r @ Ref::Arxiv(_)) = from_url {
            return Some(r);
        }
        meta.doi
            .as_deref()
            .and_then(Doi::parse)
            .map(Ref::Doi)
            .or(from_url)
    }
}

impl Note {
    /// The note's key: its file name without `.md`.
    pub fn key(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The registry record for this note. `source_path` is the note's path as
    /// recorded. The depth is the note's own `depth`, else its `status` mapped
    /// by `status_depth`; an invalid `depth` gives none.
    pub fn to_reference(
        &self,
        source_path: &str,
        status_depth: &std::collections::BTreeMap<String, String>,
    ) -> crate::registry::Reference {
        let mut r = crate::registry::Reference::new(self.key());
        r.source = Some(crate::registry::Source {
            path: source_path.to_owned(),
            line: 1,
        });
        let Ok(meta) = &self.meta else {
            return r;
        };
        r.kind = Some("paper".to_owned());
        r.title = meta.title.clone();
        r.authors = (!meta.authors.is_empty()).then(|| meta.authors.clone());
        r.author_variants = meta.author_variants.clone();
        r.year = meta.year;
        r.venue = meta.venue.clone();
        r.ids.url = meta.url.clone();
        r.ids.doi = meta
            .doi
            .as_deref()
            .and_then(Doi::parse)
            .or_else(|| match meta.url.as_deref().and_then(Ref::from_note_url) {
                Some(Ref::Doi(d)) => Some(d),
                _ => None,
            })
            .map(|d| d.as_str().to_owned());
        if let Some(id) = meta
            .url
            .as_deref()
            .and_then(crate::id::ArxivId::from_abs_url)
        {
            r.version = id.version.map(|v| format!("v{v}"));
            r.ids.arxiv = Some(id.id);
        }
        r.depth = match meta.explicit_depth() {
            Ok(Some(d)) => Some(d.to_owned()),
            Ok(None) => meta
                .status
                .as_ref()
                .and_then(|s| status_depth.get(s))
                .cloned(),
            Err(_) => None,
        };
        r
    }
}

/// Splits `---\n<yaml>\n---\n<body>` (the archive's frontmatter format).
pub fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some((&rest[..end], &rest[end + 5..]))
}

pub fn parse_meta(text: &str) -> Result<Meta, String> {
    let (yaml, _) = split_frontmatter(text).ok_or("missing YAML frontmatter")?;
    yaml_serde::from_str(yaml).map_err(|e| format!("invalid frontmatter: {e}"))
}

/// Loads every `*.md` note in `dir`, sorted by path; `None` if `dir` does not exist.
pub fn load(dir: &Path) -> Result<Option<Vec<Note>>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut notes = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let meta = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| parse_meta(&text));
        notes.push(Note { path, meta });
    }
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Some(notes))
}

/// True when a title-search hit may stand in for a note: equal normalized titles.
pub fn title_matches(note_title: &str, found: &str) -> bool {
    !key(note_title).is_empty() && key(note_title) == key(found)
}

/// The frontmatter values a stub fills in.
pub struct StubFields {
    pub title: String,
    pub authors: Vec<String>,
    pub year: i64,
    /// Comment for the `year` line, e.g. asking to confirm the first public version.
    pub year_comment: Option<String>,
    pub venue: String,
    pub url: String,
}

impl StubFields {
    pub fn from_paper(paper: &Paper) -> Self {
        StubFields {
            title: paper.title.clone(),
            authors: paper.authors.clone(),
            year: paper.published[..4].parse().unwrap_or_default(),
            year_comment: None,
            venue: paper
                .journal_ref
                .clone()
                .unwrap_or_else(|| format!("arXiv:{}", paper.id)),
            url: paper.abs_url.clone(),
        }
    }

    /// Note id: `<year>-<first-author-surname>-<title-slug>`.
    pub fn note_id(&self) -> String {
        let family = self
            .authors
            .first()
            .map(|a| crate::text::ascii_words(surname(a)).join("-"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "anonymous".to_owned());
        format!("{}-{family}-{}", self.year, title_slug(&self.title))
    }
}

/// Renders a complete note: frontmatter filled from `fields` (field order and
/// comments from the template) followed by the template's body. Fields that need
/// judgment are left empty with a TODO comment, so a frontmatter validator
/// rejects the note until someone fills them. An empty field keeps the type of
/// the template's example value: `[]` for a list, `{}` for a mapping, else null.
pub fn stub_fields(fields: &StubFields, template: &str, added: &str) -> Result<String, String> {
    let (template_meta, body) =
        split_frontmatter(template).ok_or("paper template has no YAML frontmatter")?;
    let id = fields.note_id();
    let authors: Vec<String> = fields
        .authors
        .iter()
        .map(|a| yaml_scalar(a, true))
        .collect();
    let mut out = String::from("---\n");
    for line in template_meta.lines() {
        let Some((field, example, hint)) = template_field(line) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let value = match field {
            "id" => id.clone(),
            "title" => yaml_scalar(&fields.title, false),
            "authors" => format!("[{}]", authors.join(", ")),
            "year" => {
                let line = format!("year: {}", fields.year);
                match &fields.year_comment {
                    Some(comment) => {
                        let _ = writeln!(out, "{line:<35} # {comment}");
                    }
                    None => {
                        let _ = writeln!(out, "{line}");
                    }
                }
                continue;
            }
            "venue" => yaml_scalar(&fields.venue, false),
            "url" => fields.url.clone(),
            "added" => added.to_owned(),
            _ => {
                let placeholder = empty_like(example);
                let line = format!("{field}: {placeholder}");
                let _ = writeln!(out, "{line:<35} # TODO: {hint}");
                continue;
            }
        };
        let _ = writeln!(out, "{field}: {value}");
    }
    out.push_str("---\n");
    out.push_str(body);
    Ok(out)
}

/// `key: value  # comment` → (key, example value, comment or else the example
/// value) for an active (uncommented) field line.
fn template_field(line: &str) -> Option<(&str, &str, &str)> {
    let (field, rest) = line.split_once(':')?;
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        return None;
    }
    let (example, hint) = match rest.split_once(" # ") {
        Some((value, comment)) => (value.trim(), comment.trim()),
        None => (rest.trim(), rest.trim()),
    };
    Some((field, example, hint))
}

/// The empty value of the same YAML type as a template's example value:
/// `[]` for a flow sequence, `{}` for a flow mapping, else null.
fn empty_like(example: &str) -> &'static str {
    match example.chars().next() {
        Some('[') => "[]",
        Some('{') => "{}",
        _ => "",
    }
}

/// A YAML scalar, plain when unambiguous for both YAML 1.1 and 1.2 loaders,
/// otherwise double-quoted. `in_flow` also quotes flow indicators (for `[a, b]`).
fn yaml_scalar(s: &str, in_flow: bool) -> String {
    let reserved = matches!(
        s.to_ascii_lowercase().as_str(),
        "y" | "n" | "yes" | "no" | "true" | "false" | "on" | "off" | "null"
    );
    let plain = s.starts_with(|c: char| c.is_alphabetic())
        && !reserved
        && s == s.trim()
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.ends_with(':')
        && !s.chars().any(|c| {
            c.is_control() || matches!(c, '"' | '\'' | '\\') || (in_flow && ",[]{}:#".contains(c))
        });
    if plain {
        return s.to_owned();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = "---\n\
        id: YYYY-lastname-short-slug        # = file name\n\
        title: Full paper title\n\
        authors: [First Author, Second Author]\n\
        year: YYYY                          # first public version\n\
        venue: arXiv:XXXX.XXXXX             # latest known version\n\
        url: https://arxiv.org/abs/XXXX.XXXXX  # canonical landing page\n\
        topics: [primary-topic]             # ids of files in research/topics/\n\
        kind: empirical                     # theory | empirical | method | system | survey\n\
        relevance: 3                        # 1-5\n\
        status: read                        # read = full text; skimmed = abstract/intro only\n\
        added: YYYY-MM-DD\n\
        takeaway: One sentence a reader should remember.\n\
        # code: https://github.com/...      # optional\n\
        ---\n\n## Summary\n\nWhat the paper does.\n";

    #[test]
    fn checked_date_parses_from_unquoted_yaml() {
        let note = "---\ntitle: T\nurl: https://example.org/p\nchecked: 2026-11-05\n---\n";
        let meta = parse_meta(note).unwrap();
        assert_eq!(meta.checked.as_deref(), Some("2026-11-05"));
        assert!(
            parse_meta("---\ntitle: T\n---\n")
                .unwrap()
                .checked
                .is_none()
        );
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

    #[derive(Debug, Deserialize)]
    struct Full {
        id: String,
        title: String,
        authors: Vec<String>,
        year: i64,
        venue: String,
        url: String,
        topics: Vec<String>,
        kind: Option<String>,
        relevance: Option<i64>,
        status: Option<String>,
        added: String,
        takeaway: Option<String>,
    }

    fn stub(paper: &Paper, template: &str, added: &str) -> Result<String, String> {
        stub_fields(&StubFields::from_paper(paper), template, added)
    }

    #[test]
    fn stub_frontmatter_round_trips() {
        let p = paper(
            "Arbitrage: \"Rebalancing-Versus-Holding\" #1 & More",
            &["José Ñúñez, Jr.", "Zoë Ørsted", "yes", "O'Brien"],
        );
        let text = stub(&p, TEMPLATE, "2026-11-05").unwrap();
        let (yaml, body) = split_frontmatter(&text).unwrap();
        let meta: Full = yaml_serde::from_str(yaml).unwrap();
        assert_eq!(meta.id, "2022-nunez-arbitrage-rebalancing-versus");
        assert_eq!(meta.title, p.title);
        assert_eq!(meta.authors, p.authors);
        assert_eq!(meta.year, 2022);
        assert_eq!(meta.venue, "arXiv:2208.06046");
        assert_eq!(meta.url, "https://arxiv.org/abs/2208.06046");
        assert_eq!(meta.added, "2026-11-05");
        assert!(meta.topics.is_empty());
        assert!(meta.kind.is_none() && meta.relevance.is_none() && meta.status.is_none());
        assert!(meta.takeaway.is_none());
        assert!(text.contains("kind:"));
        assert!(text.contains("# TODO: theory | empirical"));
        assert!(text.contains("# code: https://github.com/..."));
        assert_eq!(body, "\n## Summary\n\nWhat the paper does.\n");
        // The reading side used by fetch/verify agrees.
        let read = parse_meta(&text).unwrap();
        assert_eq!(read.title.as_deref(), Some(p.title.as_str()));
    }

    #[test]
    fn stub_uses_journal_ref_and_plain_scalars() {
        let mut p = paper(
            "Synthetic Market Making and Rebalancing-Versus-Holding",
            &["Mira Ellingwood"],
        );
        p.journal_ref = Some("Synth. J. Phys. 1:1-8,1999".into());
        let text = stub(&p, TEMPLATE, "2026-11-05").unwrap();
        assert!(text.contains("\ntitle: Synthetic Market Making and Rebalancing-Versus-Holding\n"));
        assert!(text.contains("\nauthors: [Mira Ellingwood]\n"));
        assert!(text.contains("\nvenue: Synth. J. Phys. 1:1-8,1999\n"));
        p.journal_ref = None;
        let text = stub(&p, TEMPLATE, "2026-11-05").unwrap();
        assert!(text.contains("\nvenue: arXiv:2208.06046\n"));
        assert!(text.contains(
            "\ntakeaway:                           # TODO: One sentence a reader should remember.\n"
        ));
        assert!(text.contains("\nkind:                               # TODO: theory |"));
        assert_eq!(yaml_scalar("Ends with:", false), "\"Ends with:\"");
        assert_eq!(yaml_scalar("a #tag", false), "\"a #tag\"");
        assert_eq!(yaml_scalar("Ann: Lee", true), "\"Ann: Lee\"");
        assert!(text.contains("\nid: 2022-ellingwood-synthetic-market-making\n"));
    }

    fn meta(url: Option<&str>, doi: Option<&str>) -> Meta {
        Meta {
            title: None,
            authors: vec![],
            year: None,
            venue: None,
            url: url.map(str::to_owned),
            doi: doi.map(str::to_owned),
            checked: None,
            former_titles: vec![],
            author_variants: vec![],
            id: None,
            status: None,
            depth: None,
        }
    }

    #[test]
    fn note_refs_from_urls_and_doi_field() {
        let reference = |url: &str, doi: Option<&str>| {
            Note {
                path: "x.md".into(),
                meta: Ok(meta(Some(url), doi)),
            }
            .reference()
            .map(|r| r.key())
        };
        assert_eq!(
            reference("https://arxiv.org/abs/2208.06046v2", None).as_deref(),
            Some("arxiv:2208.06046")
        );
        // An arXiv url wins over a doi field; otherwise the doi field wins over the url.
        assert_eq!(
            reference("https://arxiv.org/abs/2208.06046", Some("10.1234/x")).as_deref(),
            Some("arxiv:2208.06046")
        );
        assert_eq!(
            reference("https://doi.org/10.5555/rof.0004", None).as_deref(),
            Some("doi:10.5555/rof.0004")
        );
        assert_eq!(
            reference(
                "https://www.example.org/publications/dp99001",
                Some("10.2139/SSRN.1")
            )
            .as_deref(),
            Some("doi:10.2139/ssrn.1")
        );
        assert_eq!(
            reference("https://www.nber.org/papers/w99001", None).as_deref(),
            Some("doi:10.3386/w99001")
        );
        assert_eq!(
            reference("https://www.example.org/publications/dp99001", None),
            None
        );
        let note = Note {
            path: "n.md".into(),
            meta: Ok(meta(
                Some("https://arxiv.org/abs/2208.06046"),
                Some("10.1234/X"),
            )),
        };
        // The seen index files the note under both its arXiv id and its DOI.
        let mut idx = crate::seen::Index::default();
        idx.add(&note.to_reference("n.md", &Default::default()), "n.md");
        assert_eq!(idx.note("arxiv:2208.06046"), Some("n.md"));
        assert_eq!(idx.note("doi:10.1234/x"), Some("n.md"));
    }

    #[test]
    fn title_search_needs_equal_normalized_titles() {
        assert!(title_matches(
            "Network-Based Detection of Wash Trading",
            "Network based detection of wash trading"
        ));
        assert!(!title_matches(
            "Network-Based Detection of Wash Trading",
            "Wash Trading Detection in Automatic Market Makers"
        ));
        assert!(!title_matches("", ""));
    }

    #[test]
    fn empty_fields_follow_the_template_value_type() {
        let template = "---\ntitle: T\nkeywords: [a, b]        # any list field\n\
                        extra: {k: v}\nscore: 3               # 1-5\nnote: free text\n---\n";
        let fields = StubFields::from_paper(&paper("A Title", &["Ann Lee"]));
        let text = stub_fields(&fields, template, "2026-11-05").unwrap();
        assert!(text.contains("\nkeywords: []"), "{text}");
        assert!(text.contains("\nextra: {}"), "{text}");
        assert!(
            text.contains("\nscore:                              # TODO: 1-5\n"),
            "{text}"
        );
        let (yaml, _) = split_frontmatter(&text).unwrap();
        let v: serde_json::Value = yaml_serde::from_str(yaml).unwrap();
        assert_eq!(v["keywords"], serde_json::json!([]));
        assert_eq!(v["extra"], serde_json::json!({}));
        assert!(v["score"].is_null() && v["note"].is_null());
    }

    #[test]
    fn doi_stub_marks_year_for_confirmation() {
        let fields = StubFields {
            title: "Widget arbitrage across synthetic exchanges".into(),
            authors: vec!["Ada Quill".into(), "Bram Tolliver".into()],
            year: 2018,
            year_comment: Some("TODO: confirm the first public version's year".into()),
            venue: "Journal of Synthetic Finance 12(3), 101-120 (2020)".into(),
            url: "https://doi.org/10.5555/j.synth.2019.07.001".into(),
        };
        let text = stub_fields(&fields, TEMPLATE, "2026-11-05").unwrap();
        assert!(text.contains(
            "\nyear: 2018                          # TODO: confirm the first public version's year\n"
        ));
        let (yaml, _) = split_frontmatter(&text).unwrap();
        let meta: Full = yaml_serde::from_str(yaml).unwrap();
        assert_eq!(meta.id, "2018-quill-widget-arbitrage-across");
        assert_eq!(meta.year, 2018);
        assert_eq!(meta.venue, fields.venue);
        assert_eq!(meta.url, fields.url);
    }
}
