//! The reference registry: `rf.reference/1` records, one JSON object per line,
//! ordered by key. `papers` owns the file; other tools only read it.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SCHEMA: &str = "rf.reference/1";
const SCHEMA_FAMILY: &str = "rf.reference/";

/// Inspection depths from shallowest to deepest.
pub const DEPTHS: [&str; 6] = [
    "metadata",
    "abstract",
    "method",
    "full",
    "artifact",
    "reproduced",
];
/// The `verified.status` values `verify --record` writes.
pub const VERIFIED_STATUSES: [&str; 3] = ["match", "mismatch", "not-found"];
pub const TYPES: [&str; 6] = ["paper", "dataset", "software", "web", "standard", "other"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub schema: String,
    pub key: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub title: Option<String>,
    /// `None` when the source does not establish the author list.
    pub authors: Option<Vec<String>>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    #[serde(default)]
    pub ids: Ids,
    pub version: Option<String>,
    /// `YYYY-MM-DD`.
    pub accessed: Option<String>,
    pub depth: Option<String>,
    #[serde(default)]
    pub texts: Vec<Text>,
    pub duplicate_of: Option<String>,
    pub source: Option<Source>,
    pub verified: Option<Verified>,
    /// Optional: true when the source lists only the first authors ("et al.").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub authors_truncated: bool,
    /// Optional: other spellings of the first author that an index uses, when
    /// its metadata misspells the name. `verify` accepts any of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub author_variants: Vec<String>,
    /// Fields this version does not know, kept when the registry is rewritten.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ids {
    pub doi: Option<String>,
    /// Versionless arXiv id; the version is in [`Reference::version`].
    pub arxiv: Option<String>,
    pub dblp: Option<String>,
    pub openalex: Option<String>,
    pub url: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text {
    /// Relative to the project root.
    pub path: String,
    pub sha256: String,
    pub origin: Option<String>,
    /// When `read` downloaded the text, UTC RFC 3339. Absent for texts
    /// attached by hand and for manifests written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved: Option<String>,
    /// Fields this version does not know, kept when the registry is rewritten,
    /// `extraction` (see [`Text::extraction`]) and `pdf_sha256` (see
    /// [`Text::pdf_sha256`]).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The `texts[]` field that names the extraction mode. It is kept in
/// [`Text::extra`], so a mode a later version adds survives a rewrite by this
/// one instead of failing the whole registry's load.
const EXTRACTION: &str = "extraction";

/// The `texts[]` field with the sha256 of the PDF the text was extracted from.
/// It is kept in [`Text::extra`], so it is carried over and dropped with the
/// other recorded fields when a text is attached again.
const PDF_SHA256: &str = "pdf_sha256";

impl Text {
    /// The sha256 of the PDF `read` extracted the text from; `None` when none
    /// is recorded, as for texts attached by hand and texts recorded before
    /// `read` recorded it, or when the value is not a string.
    pub fn pdf_sha256(&self) -> Option<&str> {
        self.extra.get(PDF_SHA256).and_then(Value::as_str)
    }

    pub fn set_pdf_sha256(&mut self, sha256: String) {
        self.extra.insert(PDF_SHA256.to_owned(), sha256.into());
    }

    /// How the text was extracted from its PDF. `Ok(None)` when no mode is
    /// recorded, as for texts attached by hand without `--extraction` and texts
    /// recorded before modes were; `Err` with the value when this version does
    /// not know it.
    pub fn extraction(&self) -> Result<Option<Extraction>, String> {
        match self.extra.get(EXTRACTION) {
            None => Ok(None),
            Some(v) => v
                .as_str()
                .and_then(Extraction::parse)
                .map(Some)
                .ok_or_else(|| v.to_string()),
        }
    }

    pub fn set_extraction(&mut self, mode: Extraction) {
        self.extra.insert(EXTRACTION.to_owned(), mode.name().into());
    }
}

/// How a text was extracted from its PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Extraction {
    /// Reading-order text: pdf-extract, or `pdftotext` when pdf-extract fails.
    Plain,
    /// `pdftotext -layout`, which keeps the page's columns and table alignment.
    Layout,
    /// Docling's Markdown, with headings and tables: a second text beside the
    /// plain one, `<name>.docling.md`.
    Docling,
}

impl Extraction {
    fn parse(s: &str) -> Option<Self> {
        [Extraction::Plain, Extraction::Layout, Extraction::Docling]
            .into_iter()
            .find(|m| m.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Extraction::Plain => "plain",
            Extraction::Layout => "layout",
            Extraction::Docling => "docling",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub path: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verified {
    /// `YYYY-MM-DD`.
    pub at: String,
    pub against: String,
    /// `match`, `mismatch`, or `not-found`.
    pub status: String,
}

/// The optional entry field with the work's abstract, with single spaces. It
/// is kept in [`Reference::extra`], so `registry build` carries it over.
const ABSTRACT: &str = "abstract";
/// Where the abstract is from: `arxiv:<id>v<n>` or `openalex:<W id>`.
const ABSTRACT_SOURCE: &str = "abstract_source";

impl Reference {
    /// The recorded abstract; `None` when none is recorded.
    pub fn abstract_text(&self) -> Option<&str> {
        self.extra.get(ABSTRACT).and_then(Value::as_str)
    }

    pub fn set_abstract(&mut self, text: String, source: String) {
        self.extra.insert(ABSTRACT.to_owned(), text.into());
        self.extra.insert(ABSTRACT_SOURCE.to_owned(), source.into());
    }

    pub fn new(key: String) -> Self {
        Reference {
            schema: SCHEMA.to_owned(),
            key,
            kind: None,
            title: None,
            authors: None,
            year: None,
            venue: None,
            ids: Ids::default(),
            version: None,
            accessed: None,
            depth: None,
            texts: Vec::new(),
            duplicate_of: None,
            source: None,
            verified: None,
            authors_truncated: false,
            author_variants: Vec::new(),
            extra: Map::new(),
        }
    }

    /// True when the bibliographic fields (what `verify` checks) are equal.
    fn same_metadata(&self, other: &Reference) -> bool {
        self.title == other.title
            && self.authors == other.authors
            && self.year == other.year
            && self.venue == other.venue
            && self.ids.doi == other.ids.doi
            && self.ids.arxiv == other.ids.arxiv
    }
}

/// Orders keys numerically when both are numbers, numbers before other keys,
/// and other keys as strings.
pub fn key_order(a: &str, b: &str) -> Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

pub fn sort(records: &mut [Reference]) {
    records.sort_by(|a, b| key_order(&a.key, &b.key));
}

/// Parses registry text; errors name the line.
pub fn parse(text: &str, origin: &str) -> Result<Vec<Reference>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = format!("{origin}:{}", i + 1);
        let value: Value = serde_json::from_str(line).map_err(|e| format!("{at}: {e}"))?;
        let schema = value
            .get("schema")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{at}: record has no schema field"))?;
        check_schema(schema).map_err(|e| format!("{at}: {e}"))?;
        let record: Reference = serde_json::from_value(value).map_err(|e| format!("{at}: {e}"))?;
        out.push(record);
    }
    Ok(out)
}

/// Accepts `rf.reference/1` and minor revisions written as `rf.reference/1.x`.
fn check_schema(schema: &str) -> Result<(), String> {
    let version = schema
        .strip_prefix(SCHEMA_FAMILY)
        .ok_or_else(|| format!("not a reference record (schema {schema:?})"))?;
    let major = version.split('.').next().unwrap_or_default();
    if major == "1" {
        Ok(())
    } else {
        Err(format!(
            "unsupported registry schema {schema:?}; this papers reads {SCHEMA}"
        ))
    }
}

/// Loads the registry; `Ok(None)` when the file does not exist.
pub fn load(path: &Path) -> Result<Option<Vec<Reference>>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, &path.display().to_string()).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn render(records: &[Reference]) -> String {
    let mut out = String::new();
    for r in records {
        out += &serde_json::to_string(r).expect("references serialize to JSON");
        out.push('\n');
    }
    out
}

/// Writes the registry atomically (temporary file, then rename), sorted by key.
pub fn save(path: &Path, records: &mut [Reference]) -> Result<(), String> {
    sort(records);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("jsonl.tmp-{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(render(records).as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// One read-modify-write of the registry under an exclusive lock on
/// `<registry>.lock`: the registry is loaded fresh (empty when absent), changed,
/// and saved, so concurrent `papers` runs never drop each other's changes.
/// Commands that run for minutes (verify, read) collect their changes first and
/// apply them here, never saving a copy loaded at their start.
pub fn update<T>(
    path: &Path,
    change: impl FnOnce(&mut Vec<Reference>) -> Result<T, String>,
) -> Result<T, String> {
    let lock_path = {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        path.with_file_name(name)
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    lock.lock()
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    let mut records = load(path)?.unwrap_or_default();
    let out = change(&mut records)?;
    save(path, &mut records)?;
    Ok(out)
}

/// Keeps each recorded depth for a fresh record whose source gives none. Notes
/// use this: a status without a mapped depth leaves the recorded depth alone.
pub fn keep_recorded_depth(fresh: &mut [Reference], previous: &[Reference]) {
    let recorded: BTreeMap<&str, &str> = previous
        .iter()
        .filter_map(|r| Some((r.key.as_str(), r.depth.as_deref()?)))
        .collect();
    for r in fresh.iter_mut().filter(|r| r.depth.is_none()) {
        r.depth = recorded.get(r.key.as_str()).map(|d| (*d).to_owned());
    }
}

/// Combines freshly parsed records with the previous registry. Tool-owned state
/// carries over by key: `texts`, unknown fields, ids other tools added
/// (`openalex`, `dblp`), and `verified` while the checked metadata is unchanged.
/// Keys that vanished from the source are kept (keys are never deleted) and
/// returned so the caller can report them.
pub fn merge(fresh: Vec<Reference>, previous: Vec<Reference>) -> (Vec<Reference>, Vec<String>) {
    let mut old: BTreeMap<String, Reference> =
        previous.into_iter().map(|r| (r.key.clone(), r)).collect();
    let mut out = Vec::with_capacity(fresh.len());
    for mut r in fresh {
        if let Some(prev) = old.remove(&r.key) {
            if r.same_metadata(&prev) {
                r.verified = prev.verified.clone();
            }
            r.ids.openalex = r.ids.openalex.or(prev.ids.openalex);
            r.ids.dblp = r.ids.dblp.or(prev.ids.dblp);
            for (k, v) in prev.ids.extra {
                r.ids.extra.entry(k).or_insert(v);
            }
            for t in prev.texts {
                if !r.texts.contains(&t) {
                    r.texts.push(t);
                }
            }
            for (k, v) in prev.extra {
                r.extra.entry(k).or_insert(v);
            }
        }
        out.push(r);
    }
    let vanished: Vec<String> = old.keys().cloned().collect();
    out.extend(old.into_values());
    sort(&mut out);
    (out, vanished)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"schema":"rf.reference/1","key":"177","type":"paper","title":"A Synthetic Title","authors":["Ann Lee","Bo Chen"],"year":2026,"venue":"arXiv preprint","ids":{"doi":null,"arxiv":"2601.00001","dblp":null,"openalex":null,"url":"https://arxiv.org/abs/2601.00001"},"version":"v1","accessed":"2026-10-31","depth":"method","texts":[{"path":"sources/text/2601.00001.txt","sha256":"ab","origin":"https://arxiv.org/pdf/2601.00001v1"}],"duplicate_of":null,"source":{"path":"docs/references.md","line":3015},"verified":{"at":"2026-11-07","against":"arxiv","status":"match"}}"#;

    #[test]
    fn updates_do_not_lose_concurrent_changes() {
        let dir = std::env::temp_dir().join(format!("papers-update-test-{}", std::process::id()));
        let path = dir.join("references.jsonl");
        let _ = std::fs::remove_dir_all(&dir);
        save(
            &path,
            &mut [Reference::new("1".into()), Reference::new("2".into())],
        )
        .unwrap();
        // A slow writer (verify) holds its change while a quick one (read) lands.
        let slow = {
            let path = path.clone();
            std::thread::spawn(move || {
                update(&path, |records| {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    records[0].verified = Some(Verified {
                        at: "2026-11-07".into(),
                        against: "arxiv".into(),
                        status: "match".into(),
                    });
                    Ok(())
                })
                .unwrap()
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        update(&path, |records| {
            records[1].texts.push(Text {
                path: "sources/text/2.txt".into(),
                sha256: "0".repeat(64),
                origin: Some("https://example.org/2.pdf".into()),
                retrieved: None,
                extra: Default::default(),
            });
            Ok(())
        })
        .unwrap();
        slow.join().unwrap();
        let records = load(&path).unwrap().unwrap();
        assert!(records[0].verified.is_some(), "the slow change landed");
        assert_eq!(records[1].texts.len(), 1, "the quick change survived");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn round_trips_the_contract_example() {
        let records = parse(LINE, "r.jsonl").unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.ids.arxiv.as_deref(), Some("2601.00001"));
        assert_eq!(r.verified.as_ref().unwrap().status, "match");
        assert_eq!(render(&records).trim_end(), LINE);
    }

    #[test]
    fn keeps_unknown_fields_and_rejects_other_majors() {
        let line = LINE.replace(r#""key":"177""#, r#""key":"177","future":{"x":1}"#);
        let records = parse(&line, "r.jsonl").unwrap();
        assert_eq!(records[0].extra["future"], serde_json::json!({"x": 1}));
        assert!(render(&records).contains(r#""future":{"x":1}"#));
        let v2 = LINE.replace("rf.reference/1", "rf.reference/2");
        let err = parse(&format!("\n{v2}"), "r.jsonl").unwrap_err();
        assert!(err.starts_with("r.jsonl:2: unsupported"), "{err}");
        assert!(parse("{}", "r.jsonl").unwrap_err().contains("no schema"));
        assert!(parse(&LINE.replace("rf.reference/1", "rf.reference/1.3"), "x").is_ok());
    }

    #[test]
    fn orders_numeric_keys_numerically() {
        let mut keys = vec!["10", "9", "b", "100", "a"];
        keys.sort_by(|a, b| key_order(a, b));
        assert_eq!(keys, ["9", "10", "100", "a", "b"]);
    }

    #[test]
    fn merge_keeps_tool_state_and_vanished_keys() {
        let prev = parse(LINE, "r").unwrap().remove(0);
        let mut gone = Reference::new("5".into());
        gone.title = Some("Gone".into());
        let mut fresh = prev.clone();
        fresh.texts.clear();
        fresh.verified = None;
        let (out, vanished) = merge(vec![fresh.clone()], vec![prev.clone(), gone]);
        assert_eq!(vanished, ["5"]);
        assert_eq!(
            out.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["5", "177"]
        );
        assert_eq!(out[1].texts, prev.texts);
        assert_eq!(out[1].verified, prev.verified);
        // Changed metadata invalidates the previous verification.
        fresh.title = Some("Another Title".into());
        let (out, _) = merge(vec![fresh], vec![prev]);
        assert!(out[0].verified.is_none());
    }

    #[test]
    fn merge_keeps_the_abstract() {
        let mut prev = parse(LINE, "r").unwrap().remove(0);
        let fresh = prev.clone();
        assert_eq!(prev.abstract_text(), None);
        prev.set_abstract("Words.".into(), "arxiv:2601.99901v4".into());
        let (out, _) = merge(vec![fresh], vec![prev]);
        assert_eq!(out[0].abstract_text(), Some("Words."));
        let line = serde_json::to_string(&out[0]).unwrap();
        assert!(
            line.contains(r#""abstract":"Words.","abstract_source":"arxiv:2601.99901v4""#),
            "{line}"
        );
    }
}
