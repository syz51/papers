//! `papers`: paper lookups (arXiv, Crossref, OpenAlex), arXiv search,
//! open-access reading, note verification, and note stubs for a research
//! archive. Requests are throttled per host across processes (arXiv ≤1 per 3 s,
//! other hosts ≤1 per second).

mod abstracts;
mod arxiv;
mod atom;
mod bib;
mod browser;
mod card;
mod catalogue;
mod check;
mod cite;
mod config;
mod crossref;
mod derivative;
mod docling;
mod finding;
mod graph;
mod http;
mod id;
mod notes;
mod openalex;
mod pdf;
mod refs;
mod registered;
mod registry;
mod registry_cmd;
mod searchlog;
mod seen;
mod sources;
mod structure;
mod text;
mod verify;
mod work;

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jiff::civil::Date;
use serde::Serialize;

use crate::arxiv::{Client, Sort};
use crate::atom::Paper;
use crate::http::{Get, Http};
use crate::id::ArxivId;
use crate::refs::{Doi, Ref};
use crate::work::{Record, Resolved};

/// Largest PDF `read` downloads.
const PDF_LIMIT: u64 = 64 << 20;
/// Title-search results scanned for other versions of a paper with no open copy.
const OTHER_VERSIONS: usize = 5;
/// URLs the headless browser retries before following links found on pages.
const BROWSER_URLS: usize = 8;
/// Attempts per PDF candidate (5xx and transport errors only; 403/429 are reported).
const PDF_ATTEMPTS: u32 = 2;
/// Title-search results `fetch --title` shows.
const TITLE_RESULTS: usize = 3;

#[derive(Parser)]
#[command(
    name = "papers",
    version,
    about = "Literature CLI for research archives: arXiv, Crossref, and OpenAlex"
)]
struct Cli {
    /// Project config (default: the first research.toml found walking up from here).
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Paper notes directory, used to annotate results and by verify/stub
    /// (default: [papers] notes, else research/papers).
    #[arg(long, global = true, value_name = "DIR")]
    papers: Option<PathBuf>,
    /// Ignore cached API responses and fetch them again ([papers] cache_dir);
    /// `fetch` also looks up a paper that the registry already holds.
    #[arg(long, global = true)]
    refresh: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print metadata for arXiv ids, DOIs, or SSRN/NBER URLs, in input order.
    Fetch {
        #[arg(
            required_unless_present = "title",
            conflicts_with = "title",
            value_name = "REF"
        )]
        refs: Vec<String>,
        /// Look a work up by title (OpenAlex) and show the closest matches.
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// Do not append this lookup to the search log ([papers] search_log).
        #[arg(long)]
        no_log: bool,
        /// Include abstracts in text output (JSON always includes them).
        #[arg(long = "abstract")]
        with_abstract: bool,
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Search arXiv (default), OpenAlex, DBLP, Semantic Scholar, or CiNii Research.
    /// For arXiv, plain words are AND-ed and arXiv field syntax passes through.
    Search {
        query: String,
        /// Where to search.
        #[arg(long, value_enum, default_value_t = sources::Source::Arxiv)]
        source: sources::Source,
        /// Results to return (at most 200).
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=200))]
        max: u16,
        /// Only papers first submitted on or after this date (YYYY-MM-DD).
        #[arg(long, value_name = "YYYY-MM-DD", value_parser = parse_date)]
        since: Option<Date>,
        /// Restrict to an arXiv category (repeatable; categories are OR-ed).
        #[arg(long = "category", value_name = "CAT")]
        categories: Vec<String>,
        /// arXiv sort order, always descending (default submitted); other sources
        /// return relevance order.
        #[arg(long, value_enum)]
        sort: Option<Sort>,
        /// Do not append this search to the search log ([papers] search_log).
        #[arg(long)]
        no_log: bool,
        /// Include abstracts in text output (JSON always includes them).
        #[arg(long = "abstract")]
        with_abstract: bool,
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Download an open-access PDF of a paper and extract its text; the last
    /// stdout line is the text's manifest {path, sha256, origin, retrieved,
    /// extraction, pdf_sha256} as JSON.
    Read {
        #[arg(
            required_unless_present = "key",
            conflicts_with = "key",
            value_name = "REF"
        )]
        /// An arXiv id, DOI, SSRN or NBER URL, or the http(s) URL of an open PDF
        /// or landing page.
        reference: Option<String>,
        /// Read the registry entry with this key (its arXiv version, DOI, or URL);
        /// files are named after the key.
        #[arg(long, value_name = "KEY")]
        key: Option<String>,
        /// Directory for <name>.pdf, <name>.txt and the <name>.json manifest
        /// (default: sources/text under the project root).
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Add the text to the registry entry's texts (with --key).
        #[arg(long)]
        record: bool,
        /// Download again even when a copy with the recorded sha256 exists.
        #[arg(long)]
        force: bool,
        /// How to extract the text: plain (pdf-extract, else pdftotext), layout
        /// (`pdftotext -layout`), or docling (Docling's Markdown, a second text
        /// <name>.docling.md beside <name>.txt). Default: the mode recorded for
        /// the text in its manifest or registry entry, else plain.
        #[arg(long, value_enum, value_name = "MODE")]
        extraction: Option<registry::Extraction>,
        /// Skip the headless-browser retry of pages that answer with a JavaScript
        /// check (never used after a 403, 429, or other refusal).
        #[arg(long)]
        no_browser: bool,
    },
    /// Compare registry entries (or, without a registry, paper notes) with arXiv,
    /// Crossref, or OpenAlex; differences are rf.finding/1 (exit 1 on any error).
    Verify {
        /// Only these keys (repeatable or comma-separated).
        #[arg(long, value_delimiter = ',', value_name = "KEY")]
        keys: Vec<String>,
        /// Only registry entries without a verified result.
        #[arg(long)]
        unverified: bool,
        /// Check at most N entries, in key order, after the other filters.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Write each result to the registry entry's verified field.
        #[arg(long, conflicts_with = "notes")]
        record: bool,
        /// Check the notes directory even when a registry exists.
        #[arg(long)]
        notes: bool,
        #[arg(long, value_enum, default_value_t = finding::Format::Text)]
        format: finding::Format,
    },
    /// Report whether a work is already in the registry or notes, by DOI, arXiv id,
    /// URL, or normalized title; prints matching keys. Exit 0 if seen, 1 if not.
    Seen {
        #[arg(
            required_unless_present = "title",
            conflicts_with = "title",
            value_name = "REF"
        )]
        reference: Option<String>,
        /// Look the work up by title (case, accents, and punctuation ignored).
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// One JSON object per match: key, origin, by.
        #[arg(long)]
        json: bool,
    },
    /// List the works a paper cites (OpenAlex, else Semantic Scholar), marking
    /// those already in the registry or notes.
    Refs {
        #[command(flatten)]
        args: GraphArgs,
    },
    /// List works citing a paper (OpenAlex, else Semantic Scholar), marking those
    /// already in the registry or notes.
    CitedBy {
        #[command(flatten)]
        args: GraphArgs,
    },
    /// Export registry entries as BibTeX or CSL-JSON, with stable citation keys.
    Bib {
        /// Registry keys, in output order (default: every entry that is not a duplicate).
        #[arg(value_name = "KEY")]
        keys: Vec<String>,
        #[arg(long, value_enum, default_value_t = bib::Format::Bibtex)]
        format: bib::Format,
    },
    /// Number the citations in Markdown documents by first appearance, write
    /// renumbered copies, and print the reference list (exit 1 on an unknown key).
    Cite {
        /// Documents, scanned in this order.
        #[arg(value_name = "DOC", required = true)]
        docs: Vec<PathBuf>,
        /// Write renumbered copies into this directory. The documents
        /// themselves are never rewritten: they keep their citation keys, so
        /// `cite` can run again after edits.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Citation regex with a (?P<key>…) group; repeat for several (default:
        /// [papers] cite_patterns, else `[N](…#reference-KEY)` and `[@KEY]`).
        #[arg(long = "pattern", value_name = "REGEX")]
        patterns: Vec<String>,
        /// Replacement for each citation, with {n}, {key}, {citekey} (default:
        /// [papers] cite_replace, else "[{n}]").
        #[arg(long, value_name = "TEMPLATE")]
        replace: Option<String>,
        #[arg(long, value_enum, default_value_t = cite::Style::Ipsj)]
        style: cite::Style,
        /// One JSON object per reference: {n, key, citekey, text}.
        #[arg(long)]
        json: bool,
    },
    /// Write the structure sidecar (rf.paper-structure/1) of texts that exist:
    /// `<text file name>.structure.json` beside each text, with its pages,
    /// section spans, reference list and the registry entries that list cites.
    /// Downloads nothing and never changes a text.
    Structure {
        /// Registry keys (all texts of the entry) or text files.
        #[arg(
            required_unless_present = "all",
            conflicts_with = "all",
            value_name = "KEY|PATH"
        )]
        targets: Vec<String>,
        /// Every text the registry records.
        #[arg(long)]
        all: bool,
        /// Print what each sidecar would hold; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Make the paper card (rf.paper-card/1) of a registered paper: one
    /// headless Claude call on its local text, then a check of every item.
    /// An item is kept only when its quote, of at least 8 words, is found in
    /// the text and every number of its value is in its quote. A card that
    /// keeps under 80% of its items is not stored (a suspected text-extraction
    /// fault). Off unless `[papers] cards = true`.
    Card {
        /// The registry key.
        #[arg(long)]
        key: String,
        /// Make it from the entry's Docling text (default: its first other text).
        #[arg(long)]
        docling: bool,
        /// Call Claude even when the card matches the text, prompt and model.
        #[arg(long)]
        force: bool,
        /// Only check a saved raw reply: no call, nothing stored.
        #[arg(long, value_name = "FILE")]
        reply: Option<PathBuf>,
    },
    /// Build or check the reference registry (rf.reference/1 JSONL), attach a
    /// text to an entry, or fill abstracts.
    Registry {
        #[command(subcommand)]
        action: RegistryAction,
    },
    /// Print the resolved configuration: config file, project root, and paths.
    Config {
        /// One JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Print a paper note filled from arXiv or Crossref/OpenAlex, using the archive's
    /// template ([papers] template, else <notes>/../_templates/paper.md).
    Stub {
        #[arg(value_name = "REF")]
        id: String,
        /// Create <papers>/<id>.md instead of printing (never overwrites).
        #[arg(long)]
        write: bool,
    },
}

#[derive(clap::Args)]
struct GraphArgs {
    /// An arXiv id or URL, a DOI, an SSRN/NBER URL, or an OpenAlex work id (W123).
    #[arg(value_name = "REF")]
    reference: String,
    /// Where to look.
    #[arg(long, value_enum, default_value_t = graph::GraphSource::Auto)]
    source: graph::GraphSource,
    /// Works to list (at most 1000).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    max: u16,
    /// Do not append this lookup to the search log.
    #[arg(long)]
    no_log: bool,
    /// One JSON object per line.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct RegistrySource {
    /// Where entries come from (default: the catalogue when one is configured, else the notes).
    #[arg(long, value_enum)]
    from: Option<registry_cmd::From>,
    /// Numbered Markdown catalogue (overrides [papers] catalogue; implies --from catalogue).
    #[arg(long, value_name = "PATH")]
    catalogue: Option<PathBuf>,
    /// Registry file (overrides [papers] registry).
    #[arg(long, value_name = "PATH")]
    registry: Option<PathBuf>,
}

#[derive(Subcommand)]
enum RegistryAction {
    /// Parse the catalogue or notes and write the registry, keeping tool-added state
    /// (exit 1 when any parse issue is an error; the registry is still written).
    Build {
        #[command(flatten)]
        source: RegistrySource,
        /// Print the registry to stdout instead of writing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Report duplicates, missing identifiers, malformed dates, and parse problems
    /// as rf.finding/1 (exit 1 when any finding is an error).
    Check {
        #[command(flatten)]
        source: RegistrySource,
        #[arg(long, value_enum, default_value_t = finding::Format::Text)]
        format: finding::Format,
    },
    /// Record a text file already in the project (fetched by hand, or by another
    /// tool) in an entry's `texts`, with its sha256. Replaces a text at the same path,
    /// and updates the <name>.json manifest that `read` left beside a <name>.txt.
    Attach {
        /// Registry key of the entry.
        #[arg(long)]
        key: String,
        /// The text file; its path must be inside the project root. Symlinks on
        /// the path are kept, so a linked sources directory may lead elsewhere.
        #[arg(long, value_name = "PATH")]
        text: PathBuf,
        /// Where the text came from, such as the URL of the original.
        #[arg(long, value_name = "URL")]
        origin: Option<String>,
        /// When the original was downloaded (RFC 3339, such as 2026-11-07T10:00:00Z).
        #[arg(long, value_name = "TIME", value_parser = parse_timestamp)]
        retrieved: Option<String>,
        /// How the text was extracted from its PDF: plain, layout (`pdftotext
        /// -layout`) or docling.
        #[arg(long, value_enum, value_name = "MODE")]
        extraction: Option<registry::Extraction>,
        /// Registry file (overrides [papers] registry).
        #[arg(long, value_name = "PATH")]
        registry: Option<PathBuf>,
    },
    /// Record `pdf_sha256` for texts that `read` saved before it recorded that
    /// field, so that `read --extraction <mode>` can extract them again from the
    /// saved PDF. For each <name>.json manifest without one and with <name>.pdf
    /// beside it, extracts the PDF in the text's recorded mode (plain when none
    /// is recorded) and records the PDF's sha256, in the manifest and the
    /// registry, only when that gives the recorded text sha256. Other texts are
    /// left unchanged and listed as `not reproduced`. Makes no network request.
    RecordPdfs {
        /// Directory of the texts, their PDFs and manifests (default:
        /// sources/text under the project root).
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Print what would be recorded; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Registry file (overrides [papers] registry).
        #[arg(long, value_name = "PATH")]
        registry: Option<PathBuf>,
    },
    /// Fill the `abstract` field of entries that have none, from arXiv (by
    /// arXiv id) and OpenAlex (by DOI or OpenAlex id), with the response cache.
    /// Sends only those ids; downloads no PDF. Changes no other field.
    Abstracts {
        /// Only these entries (repeatable; default: every entry).
        #[arg(long = "key", value_name = "KEY")]
        keys: Vec<String>,
        /// Look up at most this many entries.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Print what would be added; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Registry file (overrides [papers] registry).
        #[arg(long, value_name = "PATH")]
        registry: Option<PathBuf>,
    },
}

/// The registry entry for a text file inside the project: its root-relative path
/// (with `/` separators) and sha256, and the file's path as named.
///
/// The path is taken as written, after removing `.` and `..`, and is inside
/// the project when one of its directories is the project root, under any name
/// (the root may be reached through a link). Directories below that may be
/// symlinks, so a `sources` directory linked to shared storage is accepted and
/// the text is recorded under its in-project name. A symlinked file must point
/// into the project or into its own directory.
fn local_text(
    config: &config::Config,
    cwd: &Path,
    path: &Path,
    origin: Option<String>,
) -> Result<(registry::Text, PathBuf), String> {
    let canonical =
        |p: &Path| std::fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()));
    let raw = cwd.join(path);
    let given = config::normalize(&raw);
    if !given.is_file() {
        // Says why: missing, or a directory.
        std::fs::metadata(&given).map_err(|e| format!("{}: {e}", given.display()))?;
        return Err(format!("{} is not a file", given.display()));
    }
    // `..` is removed here by its name, but the system resolves a `..` after a
    // symlinked directory from the link's target: the two can be other files.
    if std::fs::canonicalize(&raw).ok() != std::fs::canonicalize(&given).ok() {
        return Err(format!(
            "{}: a `..` after a symlinked directory leads elsewhere than the path without it; \
             name the file without `..`",
            raw.display()
        ));
    }
    let root = canonical(&config.root)?;
    let outside = |what: &str| {
        format!(
            "{} {what} outside the project root {}; texts are recorded relative to it",
            given.display(),
            config.root.display()
        )
    };
    let rel = given
        .ancestors()
        .find(|dir| std::fs::canonicalize(dir).is_ok_and(|d| d == root))
        .map(|dir| given.strip_prefix(dir).expect("an ancestor is a prefix"))
        .ok_or_else(|| outside("is"))?;
    if std::fs::symlink_metadata(&given).is_ok_and(|m| m.is_symlink()) {
        let target = canonical(&given)?;
        let dir = canonical(given.parent().expect("a file has a parent"))?;
        if !target.starts_with(&root) && !target.starts_with(&dir) {
            return Err(outside("is a symlink to a file"));
        }
    }
    let rel = rel
        .iter()
        .map(|c| c.to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| format!("{}: the path is not valid UTF-8", given.display()))?
        .join("/");
    let bytes = std::fs::read(&given).map_err(|e| format!("{}: {e}", given.display()))?;
    if bytes.is_empty() {
        return Err(format!("{} is empty", given.display()));
    }
    if std::str::from_utf8(&bytes).is_err() {
        return Err(format!(
            "{} is not UTF-8 text; attach the extracted text, not the PDF",
            given.display()
        ));
    }
    let text = registry::Text {
        path: rel,
        sha256: http::sha256_hex(&bytes),
        origin,
        retrieved: None,
        extra: Default::default(),
    };
    Ok((text, given))
}

/// The manifest `read` left beside the text at `file` (`<name>.json` beside
/// `<name>.txt`), with its path. `None` when there is none, or when the JSON
/// file there is not a manifest of the text at `path`, which is said.
fn sidecar_manifest(file: &Path, path: &str) -> Result<Option<(PathBuf, registry::Text)>, String> {
    if !file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
    {
        return Ok(None);
    }
    let sidecar = file.with_extension("json");
    let bytes = match std::fs::read(&sidecar) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", sidecar.display())),
    };
    match serde_json::from_slice::<registry::Text>(&bytes) {
        Ok(t) if t.path == path => Ok(Some((sidecar, t))),
        _ => {
            println!(
                "manifest: left {} alone; it is not a manifest of {path}",
                sidecar.display()
            );
            Ok(None)
        }
    }
}

/// Fills in what `text` leaves unset from `old`, an earlier record of a text at
/// the same path: its origin, and when the file is unchanged (same sha256), its
/// download time and other fields (the extraction mode, and fields this
/// version does not know). Returns notes on what a changed file drops.
fn inherit(text: &mut registry::Text, old: &registry::Text) -> Vec<String> {
    if text.origin.is_none() {
        text.origin.clone_from(&old.origin);
    }
    if old.sha256 == text.sha256 {
        if text.retrieved.is_none() {
            text.retrieved.clone_from(&old.retrieved);
        }
        for (k, v) in &old.extra {
            text.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
        return Vec::new();
    }
    // A changed file may be another extraction or download: what was recorded
    // for the old one is not carried over, but said.
    let mut notes = Vec::new();
    let old_mode = match old.extraction() {
        Ok(mode) => mode.map(|m| m.name().to_owned()),
        Err(value) => Some(value),
    };
    if let (Ok(None), Some(mode)) = (text.extraction(), old_mode) {
        notes.push(format!(
            "note: {} changed; its recorded extraction mode {mode} is dropped \
             (--extraction to record one)",
            text.path
        ));
    }
    if let (None, Some(at)) = (&text.retrieved, &old.retrieved) {
        notes.push(format!(
            "note: {} changed; its recorded download time {at} is dropped \
             (--retrieved to record one)",
            text.path
        ));
    }
    notes
}

/// Records `text` in entry `key`'s `texts`, replacing a text at the same path.
/// With `file`, the text's file, the manifest `read` left beside it is read
/// first and rewritten with the recorded text, under the registry's lock; when
/// that write fails, the registry is left unchanged. What `text` leaves unset
/// is filled in from those of the manifest and the replaced text that record
/// the same file (same sha256), then from the others (see `inherit`). Returns the text as recorded and the manifest's path, if any.
fn record_text(
    config: &config::Config,
    key: &str,
    mut text: registry::Text,
    file: Option<&Path>,
    missing: impl FnOnce() -> String,
) -> Result<(registry::Text, Option<PathBuf>), String> {
    let (text, manifest, notes) = registry::update(&config.registry, |records| {
        let r = records
            .iter_mut()
            .find(|r| r.key == key)
            .ok_or_else(missing)?;
        if let Some(target) = &r.duplicate_of {
            println!("note: entry {key} is a duplicate of {target}");
        }
        let manifest = match file {
            Some(file) => sidecar_manifest(file, &text.path)?,
            None => None,
        };
        // Records of this very file first: a stale one's origin is older.
        let mut older: Vec<&registry::Text> = manifest.iter().map(|(_, t)| t).collect();
        older.extend(r.texts.iter().filter(|t| t.path == text.path));
        older.sort_by_key(|t| t.sha256 != text.sha256);
        let mut notes = Vec::new();
        for old in older {
            for note in inherit(&mut text, old) {
                if !notes.contains(&note) {
                    notes.push(note);
                }
            }
        }
        if let Some((sidecar, _)) = &manifest {
            let line = serde_json::to_string(&text).expect("texts serialize");
            std::fs::write(sidecar, format!("{line}\n"))
                .map_err(|e| format!("{}: {e}", sidecar.display()))?;
        }
        // An entry keeps one Docling text: a new one replaces it, wherever it is.
        // A text read again keeps its place.
        let docling = is_docling(&text);
        let replaced = |t: &registry::Text| t.path == text.path || (docling && is_docling(t));
        let at = r.texts.iter().position(replaced);
        r.texts.retain(|t| !replaced(t));
        match at {
            Some(at) => r.texts.insert(at.min(r.texts.len()), text.clone()),
            None => r.texts.push(text.clone()),
        }
        Ok((text, manifest.map(|(path, _)| path), notes))
    })?;
    for note in notes {
        println!("{note}");
    }
    Ok((text, manifest))
}

/// What `record_pdfs` found; texts are named by key (see `record_pdfs`).
#[derive(Debug, Default, PartialEq)]
struct PdfRecords {
    recorded: Vec<String>,
    not_reproduced: Vec<String>,
    /// Manifests that already had a `pdf_sha256`.
    already: usize,
    /// Manifests without a `<name>.pdf` beside them.
    no_pdf: usize,
}

/// The extractor with which the PDF at `path`, whose bytes are `bytes`, gives
/// the text with `sha256` in `mode`, or why none does. `pdftotext` gives the
/// program, or why it cannot run.
///
/// A plain text came from pdf-extract or from `pdftotext`: which one `read`
/// kept depended on the title it knew then, so both are tried here, and either
/// reproducing the text shows that it came from this PDF. A layout text came
/// from `pdftotext -layout` only.
fn reproduce<'a>(
    path: &Path,
    bytes: &[u8],
    mode: Result<Option<registry::Extraction>, String>,
    sha256: &str,
    pdftotext: impl FnOnce() -> Result<&'a str, String>,
) -> Result<String, String> {
    let same = |text: &str| http::sha256_hex(text.as_bytes()) == sha256;
    let run = |program: &str, layout: bool| {
        pdf::pdftotext(
            program,
            path,
            layout,
            pdf::PDFTOTEXT_TIMEOUT,
            pdf::PDFTOTEXT_MAX_OUTPUT,
        )
    };
    match mode.map(|m| m.unwrap_or(registry::Extraction::Plain)) {
        Ok(registry::Extraction::Plain) => {
            let first = match pdf::extract_text(bytes) {
                Ok(text) if same(&text) => return Ok("pdf-extract".to_owned()),
                Ok(_) => "pdf-extract gives another text".to_owned(),
                Err(e) => e,
            };
            let program = pdftotext().map_err(|e| format!("{first}; {e}"))?;
            match run(program, false) {
                Ok(text) if same(&text) => Ok(program.to_owned()),
                Ok(_) => Err(format!("{first}, and so does {program}")),
                Err(e) => Err(format!("{first}; {e}")),
            }
        }
        Ok(registry::Extraction::Layout) => {
            let program = pdftotext()
                .map_err(|e| format!("layout extraction needs pdftotext from Poppler ({e})"))?;
            match run(program, true)? {
                text if same(&text) => Ok(format!("{program} -layout")),
                _ => Err(format!("{program} -layout gives another text")),
            }
        }
        Ok(registry::Extraction::Docling) => {
            Err("a Docling text has its own <name>.docling.json manifest".to_owned())
        }
        Err(value) => Err(format!(
            "its extraction mode {value} is not known to this version of papers"
        )),
    }
}

/// Records `pdf_sha256` for the texts in `dir` that `read` saved before it
/// recorded that field: each `<name>.json` manifest of `<name>.txt` without a
/// `pdf_sha256` and with `<name>.pdf` beside it. The PDF is extracted in the
/// text's recorded mode (plain when none is recorded; see `reproduce`). Only
/// when that gives the recorded text sha256 is the PDF's sha256 written to the
/// manifest and to each registry text with the same path and sha256, under the
/// registry's lock. Any other text is left as it is and listed. Makes no
/// network request; with `dry_run`, writes nothing.
///
/// A text is named by the keys of the registry entries that list its path,
/// else by its file name without the extension. `pdftotext` is
/// `pdf::PDFTOTEXT`, or a stand-in in tests.
fn record_pdfs(
    config: &config::Config,
    dir: &Path,
    dry_run: bool,
    pdftotext: &str,
) -> Result<PdfRecords, String> {
    struct Reproduced {
        manifest: PathBuf,
        text: registry::Text,
        key: String,
        pdf_sha256: String,
        extractor: String,
    }
    let at = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    let mut manifests = std::fs::read_dir(dir)
        .map_err(|e| at(dir, e))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| at(dir, e))?;
    // A Docling manifest records its PDF's sha256 when it is written.
    manifests.retain(|p| {
        p.extension().is_some_and(|e| e == "json")
            && !structure::is_sidecar(p)
            && !p.to_string_lossy().ends_with(docling::MANIFEST_SUFFIX)
    });
    manifests.sort();
    let known = registry::load(&config.registry)?.unwrap_or_default();
    // Started once, and only when a text needs it.
    let checked = std::cell::OnceCell::new();
    let program = || {
        checked
            .get_or_init(|| pdf::check_pdftotext(pdftotext))
            .clone()
            .map(|()| pdftotext)
    };
    let mut out = PdfRecords::default();
    let mut reproduced = Vec::new();
    for manifest in manifests {
        let path = config.display(&manifest.with_extension("txt"));
        let bytes = std::fs::read(&manifest).map_err(|e| at(&manifest, e))?;
        let Some(text) = serde_json::from_slice::<registry::Text>(&bytes)
            .ok()
            .filter(|t| t.path == path)
        else {
            println!(
                "skipped: {} is not a manifest of {path}",
                config.display(&manifest)
            );
            continue;
        };
        if text.pdf_sha256().is_some() {
            out.already += 1;
            continue;
        }
        let pdf_path = manifest.with_extension("pdf");
        let pdf = match std::fs::read(&pdf_path) {
            Ok(pdf) => pdf,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.no_pdf += 1;
                continue;
            }
            Err(e) => return Err(at(&pdf_path, e)),
        };
        let keys: Vec<&str> = known
            .iter()
            .filter(|r| r.texts.iter().any(|t| t.path == path))
            .map(|r| r.key.as_str())
            .collect();
        let key = match keys.is_empty() {
            true => manifest
                .with_extension("")
                .file_name()
                .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned()),
            false => keys.join(","),
        };
        let pdf_sha256 = http::sha256_hex(&pdf);
        // pdftotext reads the file, not `pdf`: the two must be the same PDF.
        let extractor = reproduce(&pdf_path, &pdf, text.extraction(), &text.sha256, program)
            .and_then(|extractor| {
                match sha256_file(&pdf_path).as_deref() == Some(pdf_sha256.as_str()) {
                    true => Ok(extractor),
                    false => Err("the PDF changed during the run".to_owned()),
                }
            });
        match extractor {
            Ok(extractor) => reproduced.push(Reproduced {
                manifest,
                text,
                key,
                pdf_sha256,
                extractor,
            }),
            Err(why) => {
                println!("not reproduced: {key} ({path}): {why}");
                out.not_reproduced.push(key);
            }
        }
    }
    let nothing = reproduced.is_empty();
    let record = |records: &mut Vec<registry::Reference>| -> Result<(), String> {
        for found in reproduced {
            let Reproduced {
                manifest,
                mut text,
                key,
                pdf_sha256,
                extractor,
            } = found;
            // The manifest as it is under the lock: a `read` may have replaced the text.
            let now = std::fs::read(&manifest).map_err(|e| at(&manifest, e))?;
            if serde_json::from_slice::<registry::Text>(&now).ok().as_ref() != Some(&text) {
                println!(
                    "not reproduced: {key} ({}): its manifest changed during the run",
                    text.path
                );
                out.not_reproduced.push(key);
                continue;
            }
            text.set_pdf_sha256(pdf_sha256.clone());
            if !dry_run {
                let line = serde_json::to_string(&text).expect("texts serialize");
                std::fs::write(&manifest, format!("{line}\n")).map_err(|e| at(&manifest, e))?;
            }
            let mut in_registry = false;
            for t in records
                .iter_mut()
                .flat_map(|r| &mut r.texts)
                .filter(|t| t.path == text.path && t.sha256 == text.sha256)
                .filter(|t| t.pdf_sha256().is_none())
            {
                t.set_pdf_sha256(pdf_sha256.clone());
                in_registry = true;
            }
            println!(
                "{}: {key} ({}): pdf_sha256 {pdf_sha256}, reproduced by {extractor}; in the manifest{}",
                if dry_run { "would record" } else { "recorded" },
                text.path,
                match in_registry {
                    true => " and the registry",
                    false => " only (no registry entry lists this text with this sha256)",
                }
            );
            out.recorded.push(key);
        }
        Ok(())
    };
    if nothing {
        // Nothing to write: the registry is not rewritten.
    } else if dry_run || !config.registry.is_file() {
        record(&mut { known })?;
    } else {
        registry::update(&config.registry, record)?;
    }
    Ok(out)
}

/// An RFC 3339 time, in UTC to the second as `read` records it.
fn parse_timestamp(s: &str) -> Result<String, String> {
    let t: jiff::Timestamp = s
        .parse()
        .map_err(|e| format!("expected an RFC 3339 time such as 2026-11-07T10:00:00Z: {e}"))?;
    let t = t
        .round(jiff::Unit::Second)
        .map_err(|e| format!("{s}: {e}"))?;
    Ok(t.to_string())
}

fn parse_date(s: &str) -> Result<Date, String> {
    let ok = s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-';
    ok.then(|| s.parse::<Date>().ok())
        .flatten()
        .ok_or_else(|| format!("expected a date as YYYY-MM-DD, got {s:?}"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match load_config(&cli) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("papers: error: {message}");
            return ExitCode::from(2);
        }
    };
    let http = Http::new().with_cache(http::Cache {
        dir: config.cache_dir.join("http"),
        refresh: cli.refresh,
    });
    let result = match cli.command {
        Command::Fetch {
            refs,
            title,
            no_log,
            with_abstract,
            json,
        } => {
            let out = Output {
                with_abstract,
                json,
                log: !no_log,
            };
            match title {
                Some(title) => fetch_title(&http, &config, &title, out, cli.refresh),
                None => fetch(&Client::new(&http), &http, &config, &refs, out, cli.refresh),
            }
        }
        Command::Search {
            query,
            source,
            max,
            since,
            categories,
            sort,
            no_log,
            with_abstract,
            json,
        } if source != sources::Source::Arxiv => {
            let out = Output {
                with_abstract,
                json,
                log: !no_log,
            };
            if !categories.is_empty() || sort.is_some() {
                Err(format!(
                    "--category and --sort apply to arXiv only, not --source {}",
                    source.name()
                ))
            } else {
                search_source(&http, &config, source, &query, max.into(), since, out)
            }
        }
        Command::Search {
            query,
            source: _,
            max,
            since,
            categories,
            sort,
            no_log,
            with_abstract,
            json,
        } => {
            let sort = sort.unwrap_or(Sort::Submitted);
            let params = serde_json::json!({
                "max": max,
                "since": since.map(|d| d.to_string()),
                "categories": categories,
                "sort": format!("{sort:?}").to_lowercase(),
            });
            let arxiv_query = arxiv::build_query(&query, &categories, since);
            let mut params = params;
            params["arxiv_query"] = arxiv_query.clone().into();
            search(
                &Client::new(&http),
                &config,
                (&query, &arxiv_query),
                sort,
                max.into(),
                params,
                Output {
                    with_abstract,
                    json,
                    log: !no_log,
                },
            )
        }
        Command::Read {
            reference,
            key,
            out,
            record,
            force,
            extraction,
            no_browser,
        } => read(
            &http,
            &config,
            &ReadOptions {
                reference,
                key,
                out,
                record,
                force,
                extraction,
                browser: !no_browser,
            },
            pdf::PDFTOTEXT,
        ),
        Command::Verify {
            keys,
            unverified,
            limit,
            record,
            notes,
            format,
        } => verify(
            &http,
            &config,
            &VerifyOptions {
                keys,
                unverified,
                limit,
                record,
                notes,
                format,
            },
        ),
        Command::Stub { id, write } => stub(&http, &config, &id, write),
        Command::Seen {
            reference,
            title,
            json,
        } => seen(&config, reference.as_deref(), title.as_deref(), json),
        Command::Refs { args } => citations(&http, &config, graph::Direction::Refs, &args),
        Command::CitedBy { args } => citations(&http, &config, graph::Direction::CitedBy, &args),
        Command::Bib { keys, format } => export_bib(&config, &keys, format),
        Command::Cite {
            docs,
            out,
            patterns,
            replace,
            style,
            json,
        } => cite_docs(
            &config,
            &CiteOptions {
                docs,
                out,
                patterns,
                replace,
                style,
                json,
            },
        ),
        Command::Structure {
            targets,
            all,
            dry_run,
        } => structure_cmd(&config, &targets, all, dry_run),
        Command::Card {
            key,
            docling,
            force,
            reply,
        } => card::run(&config, &key, docling, force, reply.as_deref()),
        Command::Registry { action } => registry(config, action, &http),
        Command::Config { json } => {
            show_config(&config, json);
            Ok(true)
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("papers: error: {message}");
            ExitCode::from(2)
        }
    }
}

fn export_bib(
    config: &config::Config,
    keys: &[String],
    format: bib::Format,
) -> Result<bool, String> {
    let records = registry::load(&config.registry)?
        .ok_or_else(|| format!("no registry at {}", config.display(&config.registry)))?;
    let by_key: HashMap<&str, &registry::Reference> =
        records.iter().map(|r| (r.key.as_str(), r)).collect();
    let resolver = cite::Resolver::new(&records)?;
    let mut chosen: Vec<&registry::Reference> = Vec::new();
    if keys.is_empty() {
        let mut all: Vec<&registry::Reference> = records
            .iter()
            .filter(|r| r.duplicate_of.is_none())
            .collect();
        all.sort_by(|a, b| registry::key_order(&a.key, &b.key));
        chosen = all;
    }
    for key in keys {
        let entry = *by_key.get(key.as_str()).ok_or_else(|| {
            format!(
                "{}: no entry with key {key}",
                config.display(&config.registry)
            )
        })?;
        let r = resolver.canonical(entry)?;
        if r.key != entry.key {
            eprintln!(
                "papers: entry {key} is a duplicate of {0}; exporting {0}",
                r.key
            );
        }
        if !chosen.iter().any(|c| c.key == r.key) {
            chosen.push(r);
        }
    }
    let citekey = |r: &registry::Reference| resolver.citekey(r).to_owned();
    let mut out = std::io::stdout().lock();
    let text = match format {
        bib::Format::Bibtex => chosen
            .iter()
            .map(|r| bib::bibtex(r, &citekey(r)))
            .collect::<Vec<_>>()
            .join("\n"),
        bib::Format::CslJson => {
            let items: Vec<serde_json::Value> =
                chosen.iter().map(|r| bib::csl(r, &citekey(r))).collect();
            serde_json::to_string_pretty(&items).expect("CSL-JSON serializes") + "\n"
        }
    };
    out.write_all(text.as_bytes())
        .map_err(|e| format!("stdout: {e}"))?;
    Ok(true)
}

struct CiteOptions {
    docs: Vec<PathBuf>,
    out: Option<PathBuf>,
    patterns: Vec<String>,
    replace: Option<String>,
    style: cite::Style,
    json: bool,
}

fn cite_docs(config: &config::Config, o: &CiteOptions) -> Result<bool, String> {
    let patterns = cite::Patterns::new(if o.patterns.is_empty() {
        &config.cite_patterns
    } else {
        &o.patterns
    })?;
    let template = o
        .replace
        .as_deref()
        .or(config.cite_replace.as_deref())
        .unwrap_or(cite::DEFAULT_REPLACE);
    let records = registry::load(&config.registry)?
        .ok_or_else(|| format!("no registry at {}", config.display(&config.registry)))?;
    let resolver = cite::Resolver::new(&records)?;
    let mut numbering = cite::Numbering::default();
    let mut rewritten: Vec<(PathBuf, String)> = Vec::new();
    let mut unknown = 0;
    let mut citations = 0;
    for doc in &o.docs {
        let text = std::fs::read_to_string(doc).map_err(|e| format!("{}: {e}", doc.display()))?;
        let mut replacements = Vec::new();
        for c in cite::scan(&text, &patterns) {
            let Some(r) = resolver.resolve(&c.key)? else {
                eprintln!(
                    "{}:{}:{}: unknown citation key {}",
                    doc.display(),
                    c.line,
                    c.column,
                    c.key
                );
                unknown += 1;
                continue;
            };
            citations += 1;
            let n = numbering.number(&r.key);
            replacements.push((
                c.span,
                cite::expand(template, n, &r.key, resolver.citekey(r)),
            ));
        }
        rewritten.push((doc.clone(), cite::rewrite(&text, &replacements)));
    }
    if unknown > 0 {
        eprintln!("papers: {unknown} unknown citation key(s); nothing written");
        return Ok(false);
    }
    let targets: Vec<PathBuf> = match &o.out {
        Some(dir) => {
            let mut names = std::collections::HashSet::new();
            let mut targets = Vec::new();
            for (doc, _) in &rewritten {
                let name = doc
                    .file_name()
                    .ok_or_else(|| format!("{}: not a file name", doc.display()))?;
                if !names.insert(name.to_owned()) {
                    return Err(format!(
                        "two documents are named {}; --out would write both to one file",
                        name.to_string_lossy()
                    ));
                }
                let target = dir.join(name);
                let same = match (std::fs::canonicalize(doc), std::fs::canonicalize(&target)) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                };
                if same {
                    return Err(format!(
                        "{} is the input itself; --out must not overwrite the documents",
                        target.display()
                    ));
                }
                targets.push(target);
            }
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            targets
        }
        None => Vec::new(),
    };
    // Every copy is staged before any is moved into place, so a failed write
    // leaves no mix of old and new outputs.
    let mut staged = Vec::new();
    for (target, (_, text)) in targets.iter().zip(&rewritten) {
        let mut name = target.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".papers-tmp-{}", std::process::id()));
        let tmp = target.with_file_name(name);
        if let Err(e) = std::fs::write(&tmp, text) {
            for (tmp, _) in &staged {
                let _ = std::fs::remove_file(tmp);
            }
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{}: {e}", tmp.display()));
        }
        staged.push((tmp, target));
    }
    for (tmp, target) in &staged {
        std::fs::rename(tmp, target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let by_key: HashMap<&str, &registry::Reference> =
        records.iter().map(|r| (r.key.as_str(), r)).collect();
    match o.style {
        cite::Style::Ipsj => eprintln!(
            "papers: style ipsj follows {} (read {})",
            cite::IPSJ_GUIDE,
            cite::IPSJ_ACCESSED
        ),
    }
    let mut out = std::io::stdout().lock();
    for (i, key) in numbering.order.iter().enumerate() {
        let r = by_key[key.as_str()];
        let text = match o.style {
            cite::Style::Ipsj => cite::ipsj(r),
        };
        let line = if o.json {
            serde_json::json!({"n": i + 1, "key": key, "citekey": resolver.citekey(r), "text": text})
                .to_string()
        } else {
            format!("[{}] {text}", i + 1)
        };
        writeln!(out, "{line}").map_err(|e| format!("stdout: {e}"))?;
    }
    eprintln!(
        "papers: {citations} citation(s) of {} work(s) in {} document(s){}",
        numbering.order.len(),
        o.docs.len(),
        if targets.is_empty() {
            String::new()
        } else {
            format!("; wrote {}", targets.len())
        }
    );
    Ok(true)
}

fn seen(
    config: &config::Config,
    reference: Option<&str>,
    title: Option<&str>,
    json: bool,
) -> Result<bool, String> {
    let index = seen::Index::load(config)?;
    if !index.loaded {
        return Err(format!(
            "nothing to look in: no registry {} and no notes directory {}",
            config.display(&config.registry),
            config.display(&config.notes)
        ));
    }
    let query = match reference {
        Some(r) => seen::parse_input(r)?,
        None => seen::Query {
            title: title.map(str::to_owned),
            ..seen::Query::default()
        },
    };
    let hits = index.lookup(&query);
    for h in &hits {
        if json {
            println!("{}", serde_json::to_string(h).expect("hits serialize"));
        } else {
            println!("{}\t{}\t{}", h.key, h.origin, h.by);
        }
    }
    if hits.is_empty() {
        eprintln!("papers: not seen");
    }
    Ok(!hits.is_empty())
}

/// The structure of the text at the root-relative `path`, read from its file.
/// The text's own entries are those that record the path.
fn structure_of(
    config: &config::Config,
    records: &[registry::Reference],
    path: &str,
) -> Result<structure::Structure, String> {
    let file = config.root.join(path);
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sha256 = http::sha256_hex(&bytes);
    let own: Vec<String> = records
        .iter()
        .filter(|r| r.texts.iter().any(|t| t.path == path))
        .map(|r| r.key.clone())
        .collect();
    let recorded: Vec<&registry::Text> = records
        .iter()
        .flat_map(|r| &r.texts)
        .filter(|t| t.path == path)
        .collect();
    let extraction = recorded
        .iter()
        .find(|t| t.sha256 == sha256)
        .or(recorded.first())
        .and_then(|t| t.extra.get("extraction"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let markdown = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
    let mut built = structure::build(
        &String::from_utf8_lossy(&bytes),
        structure::TextInfo {
            path: path.to_owned(),
            sha256: sha256.clone(),
            lines: 0,
            extraction,
        },
        markdown,
        &structure::Targets::new(records, &own),
        format!("papers {}", env!("CARGO_PKG_VERSION")),
    );
    if !recorded.is_empty() && recorded.iter().all(|t| t.sha256 != sha256) {
        built
            .warnings
            .push("the file's sha256 is not the one the registry records".to_owned());
    }
    Ok(built)
}

/// Writes the sidecar beside the text unless it already holds `built`; gives
/// the sidecar's path.
fn write_structure(
    config: &config::Config,
    built: &structure::Structure,
) -> Result<PathBuf, String> {
    let sidecar = structure::sidecar(&config.root.join(&built.text.path));
    let rendered = built.render();
    if std::fs::read_to_string(&sidecar).ok().as_deref() != Some(rendered.as_str()) {
        derivative::write(&sidecar, rendered.as_bytes())?;
    }
    Ok(sidecar)
}

/// `papers structure`: one line per text, then its warnings. False when a
/// text could not be read.
fn structure_cmd(
    config: &config::Config,
    targets: &[String],
    all: bool,
    dry_run: bool,
) -> Result<bool, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let records = registry::load(&config.registry)?.unwrap_or_default();
    let mut paths: Vec<String> = Vec::new();
    let mut add = |path: String| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    if all {
        records
            .iter()
            .flat_map(|r| &r.texts)
            .for_each(|t| add(t.path.clone()));
    }
    for target in targets {
        match records.iter().find(|r| &r.key == target) {
            Some(r) if r.texts.is_empty() => {
                return Err(format!("entry {target} records no text"));
            }
            Some(r) => r.texts.iter().for_each(|t| add(t.path.clone())),
            None => add(config.display(&config::normalize(&cwd.join(target)))),
        }
    }
    let mut ok = true;
    let (mut with_refs, mut with_abstract) = (0, 0);
    for path in &paths {
        let built = match structure_of(config, &records, path) {
            Ok(built) => built,
            Err(e) => {
                eprintln!("papers: {e}");
                ok = false;
                continue;
            }
        };
        if !dry_run {
            write_structure(config, &built)?;
        }
        println!("{}", built.summary());
        for w in &built.warnings {
            println!("  warning: {w}");
        }
        with_refs += usize::from(built.references.known().is_some());
        with_abstract += usize::from(
            built
                .sections
                .iter()
                .any(|s| s.kind == structure::Kind::Abstract),
        );
    }
    println!(
        "{} texts{}: {with_refs} with a reference list, {with_abstract} with an abstract span",
        paths.len(),
        if dry_run { " (nothing written)" } else { "" }
    );
    Ok(ok)
}

fn registry(
    mut config: config::Config,
    action: RegistryAction,
    http: &Http,
) -> Result<bool, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let set_registry = |config: &mut config::Config, path: Option<PathBuf>| {
        if let Some(path) = path {
            config.registry = config::normalize(&cwd.join(path));
        }
    };
    // `--catalogue` overrides the configured one and implies `--from catalogue`.
    let apply = |config: &mut config::Config, source: RegistrySource| {
        set_registry(config, source.registry);
        match source.catalogue {
            Some(path) => {
                config.catalogue = Some(config::normalize(&cwd.join(path)));
                source.from.or(Some(registry_cmd::From::Catalogue))
            }
            None => source.from,
        }
    };
    match action {
        RegistryAction::Build { source, dry_run } => {
            let from = apply(&mut config, source);
            let from = from.unwrap_or_else(|| registry_cmd::default_from(&config));
            registry_cmd::build(&config, from, dry_run)
        }
        RegistryAction::Check { source, format } => {
            let from = apply(&mut config, source);
            registry_cmd::check(&config, from, format)
        }
        RegistryAction::Attach {
            key,
            text,
            origin,
            retrieved,
            extraction,
            registry,
        } => {
            set_registry(&mut config, registry);
            if !config.registry.is_file() {
                return Err(format!(
                    "attach needs a registry; none at {}",
                    config.display(&config.registry)
                ));
            }
            let (mut text, file) = local_text(&config, &cwd, &text, origin)?;
            text.retrieved = retrieved;
            if let Some(mode) = extraction {
                text.set_extraction(mode);
            }
            let (text, manifest) = record_text(&config, &key, text, Some(&file), || {
                format!(
                    "{}: no entry with key {key}",
                    config.display(&config.registry)
                )
            })?;
            println!(
                "recorded in {} (key {key})",
                config.display(&config.registry)
            );
            if let Some(manifest) = manifest {
                println!("manifest: {} updated", manifest.display());
            }
            println!("{}", serde_json::to_string(&text).expect("texts serialize"));
            Ok(true)
        }
        RegistryAction::Abstracts {
            keys,
            limit,
            dry_run,
            registry,
        } => {
            set_registry(&mut config, registry);
            let client = openalex::Client::new(http);
            let lookup = |r: &registry::Reference| -> Result<Option<abstracts::Found>, String> {
                let work = match (&r.ids.openalex, &r.ids.doi) {
                    (Some(id), _) => client.by_id(id)?,
                    (None, Some(doi)) => {
                        let doi = Doi::parse(doi).ok_or_else(|| format!("not a DOI: {doi}"))?;
                        client.by_doi(&doi)?
                    }
                    (None, None) => None,
                };
                Ok(work.and_then(|w| {
                    let source = format!("openalex:{}", w.id);
                    w.abstract_text.map(|text| (text, source))
                }))
            };
            let got =
                abstracts::backfill(&config, &Client::new(http), &lookup, &keys, limit, dry_run)?;
            for line in &got.failed {
                eprintln!("papers: {line}");
            }
            println!(
                "{} abstracts {} ({} from arXiv, {} from OpenAlex), {} without an abstract at the \
                 source, {} failed, {} without an arXiv id, DOI or OpenAlex id, {} had one before",
                got.arxiv.len() + got.openalex.len(),
                if dry_run { "to add" } else { "added" },
                got.arxiv.len(),
                got.openalex.len(),
                got.without.len(),
                got.failed.len(),
                got.no_id,
                got.already
            );
            if !got.without.is_empty() {
                println!("without an abstract: {}", got.without.join(" "));
            }
            Ok(got.failed.is_empty())
        }
        RegistryAction::RecordPdfs {
            dir,
            dry_run,
            registry,
        } => {
            set_registry(&mut config, registry);
            let dir = match dir {
                Some(dir) => config::normalize(&cwd.join(dir)),
                None => config.root.join("sources/text"),
            };
            let found = record_pdfs(&config, &dir, dry_run, pdf::PDFTOTEXT)?;
            println!(
                "{} {}, {} not reproduced, {} already with a pdf_sha256, {} without a saved PDF",
                found.recorded.len(),
                if dry_run { "to record" } else { "recorded" },
                found.not_reproduced.len(),
                found.already,
                found.no_pdf
            );
            if !found.not_reproduced.is_empty() {
                println!("not reproduced: {}", found.not_reproduced.join(" "));
            }
            Ok(true)
        }
    }
}

fn load_config(cli: &Cli) -> Result<config::Config, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let mut config = config::Config::load(cli.config.as_deref(), &cwd)?;
    if let Some(notes) = &cli.papers {
        config.set_notes(cwd.join(notes));
    }
    Ok(config)
}

fn show_config(c: &config::Config, json: bool) {
    let path = |p: &Path| p.display().to_string();
    let fields = [
        ("file", c.file.as_deref().map(path)),
        ("root", Some(path(&c.root))),
        ("notes", Some(c.display(&c.notes))),
        ("template", Some(c.display(&c.template))),
        ("registry", Some(c.display(&c.registry))),
        ("catalogue", c.catalogue.as_deref().map(|p| c.display(p))),
        ("search_log", Some(c.display(&c.search_log))),
        ("cache_dir", Some(c.display(&c.cache_dir))),
        ("docling", Some(c.docling.join(" "))),
        ("cards", Some(c.cards.to_string())),
        ("card_dir", Some(c.display(&c.card_dir))),
        ("claude", Some(c.claude.join(" "))),
        ("card_model", Some(c.card_model.clone())),
    ];
    if json {
        let map: serde_json::Map<String, serde_json::Value> = fields
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.map_or(serde_json::Value::Null, Into::into)))
            .collect();
        println!("{}", serde_json::Value::Object(map));
    } else {
        for (k, v) in fields {
            println!("{k:<10} {}", v.as_deref().unwrap_or("(none)"));
        }
    }
}

/// The note citing an arXiv paper, by its id or its journal DOI.
fn paper_note<'a>(index: &'a seen::Index, paper: &Paper) -> Option<&'a str> {
    index.note(&format!("arxiv:{}", paper.id)).or_else(|| {
        let doi = Doi::parse(paper.doi.as_deref()?)?;
        doi_note(index, &doi)
    })
}

fn doi_note<'a>(index: &'a seen::Index, doi: &Doi) -> Option<&'a str> {
    index.note(&Ref::from_doi(doi.clone()).key())
}

/// An arXiv paper as `fetch` and `search` print it. `seen`: the archive keys
/// that hold it, which `search` gives and `fetch` (which prints the registered
/// block instead) does not.
fn paper_text(
    paper: &Paper,
    note: Option<&str>,
    seen: Option<&[String]>,
    with_abstract: bool,
    json: bool,
) -> String {
    if json {
        #[derive(Serialize)]
        struct Line<'a> {
            #[serde(flatten)]
            paper: &'a Paper,
            note: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            seen: Option<&'a [String]>,
        }
        let line = Line {
            paper,
            note: note.map(str::to_owned),
            seen,
        };
        return serde_json::to_string(&line).expect("Paper serializes to JSON");
    }
    let mut out = seen
        .and_then(registered::seen_line)
        .map(|line| line + "\n")
        .unwrap_or_default();
    out += &format!(
        "{}v{} · published {} · updated {} · {}\n{}\n{}\n{}",
        paper.id,
        paper.version,
        paper.published,
        paper.updated,
        paper.primary_category,
        paper.title,
        paper.authors.join(", "),
        paper.abs_url
    );
    if let Some(j) = &paper.journal_ref {
        out += &format!("  [journal: {j}]");
    }
    if let Some(d) = &paper.doi {
        out += &format!("  [doi: {d}]");
    }
    if let Some(p) = note {
        out += &format!("\nnote: {p}");
    }
    if with_abstract {
        out += &format!("\n{}", paper.abstract_text);
    }
    out + "\n"
}

/// Prints the registry's entries for `input`, each as the registered block
/// and a blank line, or as `{"input", "registered"}` in JSON.
fn print_registered(input: &str, entries: &[registered::Registered], json: bool) {
    for r in entries {
        if json {
            println!("{}", serde_json::json!({ "input": input, "registered": r }));
        } else {
            println!("{}\n", r.block());
        }
    }
}

/// The registry's entries that match `query`, as the commands report them.
fn registered_for(
    config: &config::Config,
    index: &seen::Index,
    query: &seen::Query,
) -> Vec<registered::Registered> {
    index
        .registered(query)
        .into_iter()
        .map(|r| registered::Registered::new(config, r))
        .collect()
}

/// The line `search` prints when the log holds the same query and source.
fn earlier_line(e: &searchlog::Earlier, source: &str, log: &str) -> String {
    format!(
        "papers: searched before: last run {}, {} results ({} of this {source} query in {log})",
        e.at,
        e.results,
        match e.runs {
            1 => "1 run".to_owned(),
            n => format!("{n} runs"),
        }
    )
}

/// Says when the log holds the same search. A log that cannot be read is
/// said and does not stop the search.
fn report_earlier(config: &config::Config, same: &searchlog::Same) {
    let log = config.display(&config.search_log);
    match searchlog::earlier(&config.search_log, same) {
        Ok(Some(e)) => eprintln!("{}", earlier_line(&e, same.source, &log)),
        Ok(None) => {}
        Err(e) => eprintln!("papers: earlier runs not checked: {e}"),
    }
}

/// Unified DOI metadata: bibliographic fields from Crossref when it has the
/// DOI (else OpenAlex), abstract and open access from OpenAlex.
#[derive(Serialize)]
struct DoiLine<'a> {
    doi: &'a str,
    metadata_source: &'static str,
    title: String,
    authors: Vec<String>,
    /// `(field, date)` pairs: Crossref's posted/published/issued dates, or
    /// OpenAlex's publication date.
    dates: Vec<(String, String)>,
    earliest_year: Option<i64>,
    #[serde(rename = "type")]
    kind: Option<String>,
    venue: Option<String>,
    publisher: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<&'a str>,
    openalex_id: Option<&'a str>,
    oa_status: Option<&'a str>,
    oa_locations: &'a [openalex::Location],
    url: String,
    note: Option<String>,
}

impl<'a> DoiLine<'a> {
    fn new(r: &'a Resolved, note: Option<&str>) -> Self {
        let rec: Record = r.record();
        let oa = r.openalex.as_ref();
        let (dates, kind, publisher) = match (&r.crossref, oa) {
            (Some(c), _) => (c.dates.clone(), Some(c.kind.clone()), c.publisher.clone()),
            (None, Some(o)) => (
                o.publication_date
                    .iter()
                    .map(|d| ("publication_date".to_owned(), d.clone()))
                    .collect(),
                o.kind.clone(),
                None,
            ),
            (None, None) => (Vec::new(), None, None),
        };
        DoiLine {
            doi: r.doi.as_str(),
            metadata_source: rec.source,
            title: rec.titles.last().cloned().unwrap_or_default(),
            authors: rec.authors,
            dates,
            earliest_year: r.earliest_year().0,
            kind,
            venue: r.venue(),
            publisher,
            abstract_text: r.abstract_text(),
            openalex_id: oa.map(|o| o.id.as_str()),
            oa_status: oa.and_then(|o| o.oa_status.as_deref()),
            oa_locations: oa.map_or(&[], |o| o.oa_locations.as_slice()),
            url: r.doi.url(),
            note: note.map(str::to_owned),
        }
    }
}

fn print_resolved(r: &Resolved, note: Option<&str>, with_abstract: bool, json: bool) {
    let line = DoiLine::new(r, note);
    if json {
        println!(
            "{}",
            serde_json::to_string(&line).expect("DOI metadata serializes to JSON")
        );
        return;
    }
    let mut head = format!("doi:{}", line.doi);
    if let Some(kind) = &line.kind {
        head += &format!(" · {kind}");
    }
    head += &format!(" · {}", line.metadata_source);
    if r.crossref.is_none() {
        head += " (not in Crossref)";
    }
    let mut out = format!("{head}\n{}\n{}", line.title, line.authors.join(", "));
    let publisher = line
        .publisher
        .as_deref()
        .filter(|p| !line.venue.as_deref().is_some_and(|v| v.starts_with(p)));
    let venue = [line.venue.as_deref(), publisher]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    if !venue.is_empty() {
        out += &format!("\n{venue}");
    }
    let (earliest, years) = r.earliest_year();
    let dates: Vec<String> = line.dates.iter().map(|(f, d)| format!("{f} {d}")).collect();
    out += &format!("\ndates: {}", dates.join(", "));
    if let Some(y) = earliest {
        out += &format!(" · earliest year {y} ({years})");
    }
    out += &format!("\n{}", line.url);
    match &r.openalex {
        Some(o) => {
            out += &format!(
                "\nOpenAlex {} · OA {}",
                o.id,
                o.oa_status.as_deref().unwrap_or("unknown")
            );
            for loc in &o.oa_locations {
                out += &format!("\n  open access: {}", describe_location(loc));
            }
        }
        None => out += "\nOpenAlex: no record",
    }
    if let Some(p) = &line.note {
        out += &format!("\nnote: {p}");
    }
    if with_abstract {
        out += &format!(
            "\n{}",
            line.abstract_text
                .unwrap_or("(no abstract from OpenAlex or Crossref)")
        );
    }
    println!("{out}\n");
}

fn describe_location(loc: &openalex::Location) -> String {
    let (url, kind) = match (&loc.pdf_url, &loc.landing_page_url) {
        (Some(pdf), _) => (pdf.as_str(), "pdf"),
        (None, Some(page)) => (page.as_str(), "landing page"),
        (None, None) => ("(no URL)", "?"),
    };
    let detail = [Some(kind), loc.version.as_deref(), loc.source.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ");
    format!("{url} ({detail})")
}

fn report_error(input: &str, message: &str, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({ "input": input, "error": message })
        );
    } else {
        eprintln!("papers: {input}: {message}");
    }
}

/// How lookups print and whether they are logged.
#[derive(Clone, Copy)]
struct Output {
    with_abstract: bool,
    json: bool,
    log: bool,
}

fn paper_found(p: &Paper) -> searchlog::Found {
    searchlog::Found {
        id: format!("arxiv:{}v{}", p.id, p.version),
        title: Some(p.title.clone()),
        year: p.published.get(..4).and_then(|y| y.parse().ok()),
        doi: p.doi.clone(),
        arxiv: Some(p.id.clone()),
        openalex: None,
        url: Some(p.abs_url.clone()),
    }
}

fn work_found(w: &openalex::Work) -> searchlog::Found {
    searchlog::Found {
        id: format!("openalex:{}", w.id),
        title: Some(w.title.clone()),
        year: w.publication_year,
        doi: w.doi.as_ref().map(|d| d.as_str().to_owned()),
        openalex: Some(w.id.clone()),
        ..searchlog::Found::default()
    }
}

/// Appends log entries unless logging is off; `entries` builds them from the index.
fn log_searches(
    config: &config::Config,
    out: Output,
    index: &seen::Index,
    entries: impl FnOnce(&seen::Index) -> Vec<searchlog::Entry>,
) -> Result<(), String> {
    if !out.log {
        return Ok(());
    }
    searchlog::append(&config.search_log, &entries(index))
}

/// Prints metadata for `inputs`. A registered input prints the registered
/// block first and, unless `refresh` (`--refresh`), is not fetched.
fn fetch(
    arxiv: &Client,
    http: &Http,
    config: &config::Config,
    inputs: &[String],
    out: Output,
    refresh: bool,
) -> Result<bool, String> {
    let (with_abstract, json) = (out.with_abstract, out.json);
    let index = seen::Index::load(config)?;
    let mut logged: Vec<(String, &'static str, Vec<searchlog::Found>)> = Vec::new();
    let parsed: Vec<Result<Ref, String>> = inputs.iter().map(|s| Ref::parse(s)).collect();
    let registered: Vec<Vec<registered::Registered>> = inputs
        .iter()
        .map(|s| match seen::parse_input(s) {
            Ok(query) => registered_for(config, &index, &query),
            Err(_) => Vec::new(),
        })
        .collect();
    let skip = |i: usize| !refresh && !registered[i].is_empty();
    let arxiv_ids: Vec<ArxivId> = parsed
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Ok(Ref::Arxiv(id)) if !skip(i) => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut looked_up = if arxiv_ids.is_empty() {
        Vec::new()
    } else {
        arxiv.fetch(&arxiv_ids)
    }
    .into_iter();
    let mut all_ok = true;
    for (i, (input, reference)) in inputs.iter().zip(parsed).enumerate() {
        print_registered(input, &registered[i], json);
        if skip(i) {
            eprintln!("papers: {input}: registered; not fetched (--refresh to fetch)");
            continue;
        }
        let result = match reference {
            Err(message) => Err(message),
            Ok(Ref::Arxiv(_)) => match looked_up.next().expect("one result per arXiv id") {
                Ok(paper) => {
                    let note = paper_note(&index, &paper);
                    println!("{}", paper_text(&paper, note, None, with_abstract, json));
                    logged.push((input.clone(), "arxiv", vec![paper_found(&paper)]));
                    Ok(())
                }
                Err(e) => {
                    if e.starts_with("not found") {
                        logged.push((input.clone(), "arxiv", Vec::new()));
                    }
                    Err(e)
                }
            },
            Ok(Ref::Doi(doi)) => match Resolved::fetch(http, &doi) {
                Ok(Some(r)) => {
                    print_resolved(&r, doi_note(&index, &doi), with_abstract, json);
                    let source = if r.crossref.is_some() {
                        "crossref"
                    } else {
                        "openalex"
                    };
                    let rec = r.record();
                    logged.push((
                        input.clone(),
                        source,
                        vec![searchlog::Found {
                            id: format!("doi:{doi}"),
                            title: rec.titles.first().cloned(),
                            year: r.earliest_year().0,
                            doi: Some(doi.as_str().to_owned()),
                            ..searchlog::Found::default()
                        }],
                    ));
                    Ok(())
                }
                Ok(None) => {
                    logged.push((input.clone(), "crossref", Vec::new()));
                    Err(format!(
                        "not found: neither Crossref nor OpenAlex knows doi:{doi}"
                    ))
                }
                Err(e) => Err(e),
            },
        };
        if let Err(message) = result {
            all_ok = false;
            report_error(input, &message, json);
        }
    }
    log_searches(config, out, &index, |index| {
        logged
            .iter()
            .map(|(query, source, found)| {
                searchlog::Entry::new(
                    source,
                    query,
                    serde_json::json!({"command": "fetch"}),
                    found,
                    index,
                )
            })
            .collect()
    })?;
    Ok(all_ok)
}

fn fetch_title(
    http: &Http,
    config: &config::Config,
    title: &str,
    out: Output,
    refresh: bool,
) -> Result<bool, String> {
    let (with_abstract, json) = (out.with_abstract, out.json);
    let index = seen::Index::load(config)?;
    let registered = registered_for(
        config,
        &index,
        &seen::Query {
            title: Some(title.to_owned()),
            ..seen::Query::default()
        },
    );
    print_registered(title, &registered, json);
    if !refresh && !registered.is_empty() {
        eprintln!("papers: {title:?}: registered; not looked up (--refresh to look it up)");
        return Ok(true);
    }
    let results = openalex::Client::new(http).search_title(title, TITLE_RESULTS)?;
    log_searches(config, out, &index, |index| {
        let found: Vec<searchlog::Found> = results.iter().map(work_found).collect();
        vec![searchlog::Entry::new(
            "openalex",
            title,
            serde_json::json!({"command": "fetch", "by": "title", "per_page": TITLE_RESULTS}),
            &found,
            index,
        )]
    })?;
    if results.is_empty() {
        eprintln!("papers: OpenAlex found no works for title {title:?}");
        return Ok(false);
    }
    for w in &results {
        let score = text::similarity(title, &w.title);
        let note = w
            .doi
            .as_ref()
            .and_then(|d| doi_note(&index, d))
            .map(str::to_owned);
        if json {
            #[derive(Serialize)]
            struct Line<'a> {
                similarity: f64,
                #[serde(flatten)]
                work: &'a openalex::Work,
                note: Option<String>,
            }
            let line = Line {
                similarity: (score * 100.0).round() / 100.0,
                work: w,
                note,
            };
            println!(
                "{}",
                serde_json::to_string(&line).expect("OpenAlex work serializes to JSON")
            );
            continue;
        }
        let mut head = format!("similarity {score:.2} · OpenAlex {}", w.id);
        if let Some(d) = &w.doi {
            head += &format!(" · doi:{d}");
        }
        if let Some(y) = w.publication_year {
            head += &format!(" · {y}");
        }
        if let Some(k) = &w.kind {
            head += &format!(" · {k}");
        }
        let mut out = format!("{head}\n{}\n{}", w.title, w.authors.join(", "));
        if let Some(v) = &w.venue {
            out += &format!("\n{v}");
        }
        if let Some(n) = &note {
            out += &format!("\nnote: {n}");
        }
        if with_abstract {
            out += &format!(
                "\n{}",
                w.abstract_text
                    .as_deref()
                    .unwrap_or("(no abstract from OpenAlex)")
            );
        }
        println!("{out}\n");
    }
    Ok(true)
}

fn search(
    client: &Client,
    config: &config::Config,
    (words, query): (&str, &str),
    sort: Sort,
    max: usize,
    params: serde_json::Value,
    out: Output,
) -> Result<bool, String> {
    let (with_abstract, json) = (out.with_abstract, out.json);
    if query.is_empty() {
        return Err("empty search query".to_owned());
    }
    report_earlier(
        config,
        &searchlog::Same {
            source: "arxiv",
            query: words,
            arxiv_query: Some(query),
            since: None,
        },
    );
    let (total, results) = client.search(query, sort, max)?;
    let index = seen::Index::load(config)?;
    log_searches(config, out, &index, |index| {
        let found: Vec<searchlog::Found> = results.iter().map(paper_found).collect();
        let mut params = params;
        params["total"] = total.into();
        vec![searchlog::Entry::new("arxiv", words, params, &found, index)]
    })?;
    eprintln!(
        "papers: {total} results for {query} (showing {})",
        results.len()
    );
    for paper in &results {
        let seen = searchlog::seen_keys(&index, &paper_found(paper));
        let note = paper_note(&index, paper);
        println!(
            "{}",
            paper_text(paper, note, Some(&seen), with_abstract, json)
        );
    }
    Ok(true)
}

fn search_source(
    http: &Http,
    config: &config::Config,
    source: sources::Source,
    query: &str,
    max: usize,
    since: Option<Date>,
    out: Output,
) -> Result<bool, String> {
    use sources::Source;
    if query.trim().is_empty() {
        return Err("empty search query".to_owned());
    }
    let since_text = since.map(|d| d.to_string());
    report_earlier(
        config,
        &searchlog::Same {
            source: source.name(),
            query,
            arxiv_query: None,
            since: since_text.as_deref(),
        },
    );
    let since_year = since.map(|d| i64::from(d.year()));
    let (total, hits) = match source {
        Source::Openalex => sources::openalex(http, query, since_text.as_deref(), max)?,
        Source::Dblp => {
            let (total, mut hits) = sources::dblp(http, query, max)?;
            if let Some(y) = since_year {
                hits.retain(|h| h.year.is_some_and(|hy| hy >= y));
            }
            (total, hits)
        }
        Source::S2 => sources::s2(http, query, since_year, max)?,
        Source::Cinii => sources::cinii(http, query, since_year, max)?,
        Source::Arxiv => unreachable!("arXiv searches take the arXiv path"),
    };
    let total_text = total.map_or_else(|| "?".to_owned(), |t| t.to_string());
    eprintln!(
        "papers: {total_text} results from {} for {query:?} (showing {})",
        source.name(),
        hits.len()
    );
    let index = seen::Index::load(config)?;
    let found: Vec<searchlog::Found> = hits.iter().map(sources::Hit::found).collect();
    for (hit, f) in hits.iter().zip(&found) {
        println!("{}", hit_text(hit, &searchlog::seen_keys(&index, f), out));
    }
    if out.log {
        // Date filters other than OpenAlex's and CiNii's are applied to the year here.
        let params = serde_json::json!({
            "max": max,
            "since": since_text,
            "since_applied": match source {
                Source::Openalex => "server, publication date",
                Source::Cinii => "server, year",
                _ => "client, year",
            },
            "total": total,
        });
        let entry = searchlog::Entry::new(source.name(), query, params, &found, &index);
        searchlog::append(&config.search_log, &[entry])?;
    }
    Ok(true)
}

fn citations(
    http: &Http,
    config: &config::Config,
    direction: graph::Direction,
    args: &GraphArgs,
) -> Result<bool, String> {
    let work = graph::Work::parse(&args.reference)?;
    let links = graph::links(http, &work, direction, args.source, args.max.into())?;
    let (total, hits) = &links.page;
    let index = seen::Index::load(config)?;
    let found: Vec<searchlog::Found> = hits.iter().map(sources::Hit::found).collect();
    let out = Output {
        with_abstract: false,
        json: args.json,
        log: !args.no_log,
    };
    let mut in_archive = 0;
    for (hit, f) in hits.iter().zip(&found) {
        let keys = searchlog::seen_keys(&index, f);
        in_archive += usize::from(!keys.is_empty());
        println!("{}", hit_text(hit, &keys, out));
    }
    let total = total.map_or_else(String::new, |t| format!(" of {t}"));
    eprintln!(
        "papers: {} {}{total} for {} from {}; {in_archive} already in the archive",
        hits.len(),
        match direction {
            graph::Direction::Refs => "references",
            graph::Direction::CitedBy => "citing works",
        },
        links.work,
        links.source
    );
    if out.log {
        let params = serde_json::json!({
            "command": direction.command(),
            "work": links.work,
            "max": args.max,
            "total": links.page.0,
        });
        let entry = searchlog::Entry::new(links.source, &args.reference, params, &found, &index);
        searchlog::append(&config.search_log, &[entry])?;
    }
    Ok(true)
}

/// A search or citation result as printed; a result the archive holds starts
/// with its `seen:` line.
fn hit_text(hit: &sources::Hit, seen: &[String], out: Output) -> String {
    if out.json {
        #[derive(Serialize)]
        struct Line<'a> {
            #[serde(flatten)]
            hit: &'a sources::Hit,
            seen: &'a [String],
        }
        return serde_json::to_string(&Line { hit, seen }).expect("hits serialize to JSON");
    }
    let mut head = registered::seen_line(seen)
        .map(|line| line + "\n")
        .unwrap_or_default();
    head += &hit.id;
    if let Some(y) = hit.year {
        head += &format!(" · {y}");
    }
    if let Some(d) = &hit.doi {
        head += &format!(" · doi:{d}");
    }
    if let Some(a) = &hit.arxiv {
        head += &format!(" · arXiv:{a}");
    }
    let mut text = format!("{head}\n{}\n{}", hit.title, hit.authors.join(", "));
    if let Some(v) = &hit.venue {
        text += &format!("\n{v}");
    }
    if let Some(u) = &hit.url {
        text += &format!("\n{u}");
    }
    if out.with_abstract
        && let Some(a) = &hit.abstract_text
    {
        text += &format!("\n{a}");
    }
    text + "\n"
}

/// A place `read` may find the full text.
#[derive(Debug, Clone, PartialEq)]
enum Candidate {
    Pdf(String),
    /// An open-access landing page without a PDF URL; scanned for PDF links.
    Landing(String),
}

impl Candidate {
    fn url(&self) -> &str {
        match self {
            Candidate::Pdf(u) | Candidate::Landing(u) => u,
        }
    }
}

/// PDF candidates from OpenAlex's open-access locations: PDF URLs first, then
/// landing pages, deduplicated, in OpenAlex's order.
fn oa_candidates(locations: &[openalex::Location]) -> Vec<Candidate> {
    let pdfs = locations
        .iter()
        .filter_map(|l| l.pdf_url.clone().map(Candidate::Pdf));
    let pages = locations.iter().filter_map(|l| match l.pdf_url {
        None => l.landing_page_url.clone().map(Candidate::Landing),
        Some(_) => None,
    });
    let mut out: Vec<Candidate> = Vec::new();
    for c in pdfs.chain(pages) {
        if http::host(c.url()).is_ok() && !out.iter().any(|o| o.url() == c.url()) {
            out.push(c);
        }
    }
    out
}

struct ReadOptions {
    reference: Option<String>,
    key: Option<String>,
    out: Option<PathBuf>,
    record: bool,
    force: bool,
    /// `None`: the recorded mode, else plain.
    extraction: Option<registry::Extraction>,
    browser: bool,
}

/// Where a downloaded PDF and its text go.
struct Target<'a> {
    config: &'a config::Config,
    dir: PathBuf,
    /// File name stem.
    name: String,
    title: String,
    extraction: registry::Extraction,
    /// The `pdftotext` program: `pdf::PDFTOTEXT`, or a stand-in in tests.
    pdftotext: &'a str,
    /// The command that runs Python with Docling: `[papers] docling`.
    docling: &'a [String],
}

/// What a successful download produced.
enum Saved {
    Text(registry::Text),
    /// A PDF with no extractable text.
    PdfOnly,
}

/// A file-name stem for a URL without a paper id: its host and last path
/// segment, plus a short hash of the whole URL so that different URLs never
/// share files. `None` for anything that is not an http(s) URL.
fn url_file_name(url: &str) -> Option<String> {
    let normal = text::normalize_url(url)?;
    let host_path = normal.split('?').next().unwrap_or_default();
    let (host, path) = host_path.split_once('/').unwrap_or((host_path, ""));
    let last = path.rsplit('/').find(|p| !p.is_empty()).unwrap_or_default();
    let stem = match last.rsplit_once('.') {
        Some((stem, ext))
            if ["pdf", "html", "htm", "php", "aspx"]
                .contains(&ext.to_ascii_lowercase().as_str()) =>
        {
            stem
        }
        _ => last,
    };
    let stem: String = stem.chars().take(60).collect();
    let stem = stem.trim_end_matches(['-', '_', '.']);
    let hash = &http::sha256_hex(normal.as_bytes())[..8];
    Some(file_name_key(&format!("{host}_{stem}_{hash}")))
}

/// A file-name-safe form of a registry key.
fn file_name_key(key: &str) -> String {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.starts_with('.') || safe.is_empty() {
        format!("_{safe}")
    } else {
        safe
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| http::sha256_hex(&b))
}

/// A recorded text whose file still has the recorded hash. When there is none,
/// says which recorded files changed since.
fn existing_text(config: &config::Config, recorded: &[registry::Text]) -> Option<registry::Text> {
    let mut changed: Vec<&str> = Vec::new();
    for t in recorded {
        match sha256_file(&config.root.join(&t.path)) {
            Some(sha) if sha == t.sha256 => return Some(t.clone()),
            Some(_) if !changed.contains(&t.path.as_str()) => changed.push(&t.path),
            _ => {}
        }
    }
    for path in changed {
        println!("{path} changed since its sha256 was recorded; downloading again");
    }
    None
}

/// Reads `o`'s paper; `pdftotext` is `pdf::PDFTOTEXT`, or a stand-in in tests.
fn read(
    http: &Http,
    config: &config::Config,
    o: &ReadOptions,
    pdftotext: &str,
) -> Result<bool, String> {
    if o.record && o.key.is_none() {
        return Err("--record adds the text to a registry entry; name it with --key".to_owned());
    }
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let out_dir = match &o.out {
        Some(dir) => config::normalize(&cwd.join(dir)),
        None => config.root.join("sources/text"),
    };
    let (input, entry) = match &o.key {
        Some(key) => {
            let all = registry::load(&config.registry)?.ok_or_else(|| {
                format!(
                    "--key needs a registry; none at {}",
                    config.display(&config.registry)
                )
            })?;
            let r = all.iter().find(|r| &r.key == key).cloned().ok_or_else(|| {
                format!(
                    "{}: no entry with key {key}",
                    config.display(&config.registry)
                )
            })?;
            let input = match (&r.ids.arxiv, &r.ids.doi, &r.ids.url) {
                (Some(id), _, _) => format!("{id}{}", r.version.as_deref().unwrap_or("")),
                (None, Some(doi), _) => doi.clone(),
                (None, None, Some(url)) => url.clone(),
                (None, None, None) => {
                    return Err(format!("entry {key} has no arXiv id, DOI, or URL to read"));
                }
            };
            (input, Some(r))
        }
        None => (
            o.reference
                .as_deref()
                .expect("clap requires REF or --key")
                .trim()
                .to_owned(),
            None,
        ),
    };
    // The registry's entries for the paper come first: the one `--key` names,
    // else those with the reference's arXiv id, DOI or URL.
    let registered: Vec<registry::Reference> = match &entry {
        Some(r) => vec![r.clone()],
        None => match seen::parse_input(&input) {
            // A registry that does not load is said and does not stop the read,
            // which does not need it.
            Ok(query) => match registry::load(&config.registry) {
                Ok(records) => seen::Index::of_registry(records.unwrap_or_default())
                    .registered(&query)
                    .into_iter()
                    .cloned()
                    .collect(),
                Err(e) => {
                    eprintln!("papers: registry not checked for this paper: {e}");
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        },
    };
    for r in &registered {
        println!("{}", registered::Registered::new(config, r).block());
    }
    // Without `--key`, a recorded text of a registered paper is the answer:
    // nothing is downloaded and no file is written.
    if entry.is_none()
        && let Some(first) = registered.first()
    {
        // Only texts in the asked mode answer the run.
        let texts: Vec<registry::Text> = registered
            .iter()
            .flat_map(|r| r.texts.clone())
            .filter(|t| keeps_mode(o.extraction, t))
            .collect();
        let kept = match o.force {
            true => None,
            false => existing_text(config, &texts),
        };
        let Some(text) = kept else {
            if !o.force {
                println!(
                    "no text recorded in the entry answers this run \
                     (`papers read --key {} --record` records one)",
                    first.key
                );
            }
            return read_from(http, config, o, pdftotext, out_dir, input, entry);
        };
        println!(
            "already have {} (sha256 matches); not downloading (--force to fetch again)",
            text.path
        );
        println!("{}", serde_json::to_string(&text).expect("texts serialize"));
        return Ok(true);
    }
    read_from(http, config, o, pdftotext, out_dir, input, entry)
}

/// True for a text recorded as Docling's Markdown, the entry's second text.
fn is_docling(text: &registry::Text) -> bool {
    text.extraction() == Ok(Some(registry::Extraction::Docling))
}

/// True when a recorded text answers a run that asks for mode `asked`. A text
/// without a recorded mode counts as plain.
fn keeps_mode(asked: Option<registry::Extraction>, text: &registry::Text) -> bool {
    match (asked, text.extraction()) {
        // A Docling text is a second text; only `--extraction docling` asks for it.
        (None, Ok(Some(registry::Extraction::Docling))) => false,
        (None, _) => true,
        (Some(asked), Ok(mode)) => mode.unwrap_or(registry::Extraction::Plain) == asked,
        (Some(_), Err(_)) => false,
    }
}

/// `read` after the registered report: finds a recorded copy named for this
/// run, else downloads.
fn read_from(
    http: &Http,
    config: &config::Config,
    o: &ReadOptions,
    pdftotext: &str,
    out_dir: PathBuf,
    input: String,
    entry: Option<registry::Reference>,
) -> Result<bool, String> {
    // `None`: an http(s) URL without a paper id, read as a PDF or landing page.
    let reference = match Ref::parse(&input) {
        Ok(r) => Some(r),
        Err(_) if text::normalize_url(&input).is_some() => None,
        Err(_) => {
            return Err(format!(
                "no paper id in {input:?}: read takes an arXiv id or URL, a DOI, an SSRN \
                 abstract URL, an NBER paper URL, or the http(s) URL of an open PDF or \
                 landing page"
            ));
        }
    };
    let name = match (&entry, &reference) {
        (Some(r), _) => file_name_key(&r.key),
        (None, Some(Ref::Arxiv(id))) => id.to_string().replace('/', "_"),
        (None, Some(Ref::Doi(doi))) => doi.file_key(),
        (None, None) => url_file_name(&input).expect("normalize_url accepted the input"),
    };
    if o.extraction == Some(registry::Extraction::Docling) {
        return read_docling(
            http,
            config,
            o,
            pdftotext,
            out_dir,
            &input,
            reference.as_ref(),
            name,
            entry,
        );
    }
    let sidecar = out_dir.join(format!("{name}.json"));
    let txt_file = out_dir.join(format!("{name}.txt"));
    // The sidecar first: it keeps the download time, which a text
    // attached by hand to the entry does not have.
    let mut recorded: Vec<registry::Text> = Vec::new();
    if let Ok(json) = std::fs::read_to_string(&sidecar)
        && let Ok(t) = serde_json::from_str::<registry::Text>(&json)
    {
        recorded.push(t);
    }
    // The plain or layout text only: a Docling text is a second text.
    recorded.extend(
        entry
            .iter()
            .flat_map(|r| r.texts.clone())
            .filter(|t| !is_docling(t)),
    );
    let current = existing_text(config, &recorded);
    let txt_path = config.display(&out_dir.join(format!("{name}.txt")));
    // The mode of the text this run would replace: the current one, else one
    // recorded at the same path. Texts recorded before modes were, or attached
    // without one, count as plain.
    let recorded_mode = match &current {
        Some(t) => t.extraction(),
        None => recorded
            .iter()
            .filter(|t| t.path == txt_path)
            .map(registry::Text::extraction)
            .find(|m| m != &Ok(None))
            .unwrap_or(Ok(None)),
    };
    // For a mode change: the saved PDF, when it has the sha256 recorded for
    // the text, and that text. Its text is extracted again without a download.
    let mut local = None;
    if !o.force
        && let Some(mut t) = current
    {
        let mode = t.extraction();
        if keeps_mode(o.extraction, &t) {
            if let (Some(asked), Ok(None)) = (o.extraction, &mode) {
                t.set_extraction(asked);
            }
            println!(
                "already have {} (sha256 matches); not downloading (--force to fetch again)",
                t.path
            );
            return finish_text(config, o, entry.as_ref(), &sidecar, &txt_file, t, false);
        }
        let pdf_path = out_dir.join(format!("{name}.pdf"));
        let pdf = match (t.pdf_sha256(), std::fs::read(&pdf_path)) {
            (Some(sha), Ok(bytes)) if http::sha256_hex(&bytes) == sha => Some(bytes),
            (Some(_), Ok(_)) => {
                println!(
                    "{} changed since its sha256 was recorded",
                    config.display(&pdf_path)
                );
                None
            }
            _ => None,
        };
        println!(
            "{} was extracted in {} mode; {} for {} mode",
            t.path,
            match &mode {
                Ok(m) => m.unwrap_or(registry::Extraction::Plain).name().to_owned(),
                Err(value) => value.clone(),
            },
            match pdf {
                Some(_) => format!(
                    "extracting it again from {} (sha256 matches)",
                    config.display(&pdf_path)
                ),
                None => "downloading it again".to_owned(),
            },
            o.extraction.expect("kept unless a mode is asked").name()
        );
        local = pdf.map(|bytes| (bytes, t));
    }
    // A text extracted in layout mode stays so unless `--extraction` says
    // otherwise: a plain re-extraction would silently lose its layout.
    let extraction = match (o.extraction, recorded_mode) {
        (Some(asked), _) => asked,
        (None, Ok(mode)) => mode.unwrap_or(registry::Extraction::Plain),
        (None, Err(value)) => {
            return Err(format!(
                "{txt_path} was extracted in mode {value}, which this version of papers \
                 does not know; pass --extraction plain or layout"
            ));
        }
    };
    if o.extraction.is_none() && extraction == registry::Extraction::Layout {
        println!("extraction: layout, as recorded (--extraction plain to change)");
    }
    // Only pdftotext extracts layout text: check it before any download.
    if extraction == registry::Extraction::Layout {
        pdf::check_pdftotext(pdftotext).map_err(|e| {
            format!(
                "layout extraction needs pdftotext from Poppler ({e}); install Poppler \
                 or pass --extraction plain"
            )
        })?;
    }
    let target = Target {
        config,
        dir: out_dir,
        name,
        // A paper id's lookup replaces it with the indexed title.
        title: entry
            .as_ref()
            .and_then(|r| r.title.clone())
            .unwrap_or_default(),
        extraction,
        pdftotext,
        docling: &config.docling,
    };
    if let Some((bytes, t)) = local {
        // The PDF's origin and download time stay those recorded for it.
        return match extract_pdf(&target, &bytes, t.origin, t.retrieved)? {
            Saved::Text(t) => finish_text(config, o, entry.as_ref(), &sidecar, &txt_file, t, true),
            Saved::PdfOnly => Ok(false),
        };
    }
    let mut browse = |pages: &[String], target: &Target| {
        if o.browser {
            try_in_browser(pages, target)
        } else {
            println!("not retrying the JavaScript check page(s) in a browser (--no-browser)");
            Ok(None)
        }
    };
    let saved = download(http, &input, reference.as_ref(), target, &mut browse)?;
    match saved {
        Some(Saved::Text(t)) => {
            finish_text(config, o, entry.as_ref(), &sidecar, &txt_file, t, true)
        }
        Some(Saved::PdfOnly) => Ok(false),
        None => {
            println!("no candidate yielded a PDF; open the landing page in a browser");
            Ok(false)
        }
    }
}

/// Records a read text in `entry`, applied to the registry as it is now, not
/// the copy loaded before the download.
fn record_read(
    config: &config::Config,
    entry: &registry::Reference,
    text: registry::Text,
) -> Result<(), String> {
    record_text(config, &entry.key, text, None, || {
        format!("entry {} left the registry during the download", entry.key)
    })?;
    println!(
        "recorded in {} (key {})",
        config.display(&config.registry),
        entry.key
    );
    Ok(())
}

/// Writes the structure sidecar of a read text, from the registry as it is
/// now. A registry that does not load gives no citation edges, which is said.
/// The sidecar can be built again, so a failure here does not fail the read.
fn structure_after_read(
    config: &config::Config,
    entry: Option<&registry::Reference>,
    saved: registry::Text,
) {
    let path = saved.path.clone();
    let mut records = registry::load(&config.registry)
        .unwrap_or_else(|e| {
            eprintln!("papers: registry not read for the structure's citations: {e}");
            None
        })
        .unwrap_or_default();
    // The text as read, also where the registry does not record it (yet).
    if let Some(entry) = entry
        && let Some(r) = records.iter_mut().find(|r| r.key == entry.key)
    {
        r.texts.retain(|t| t.path != path);
        r.texts.push(saved);
    } else if !records
        .iter()
        .any(|r| r.texts.iter().any(|t| t.path == path))
    {
        let mut unregistered = registry::Reference::new(String::new());
        unregistered.texts.push(saved);
        records.push(unregistered);
    }
    match structure_of(config, &records, &path).and_then(|b| write_structure(config, &b)) {
        Ok(written) => println!("structure: {}", config.display(&written)),
        Err(e) => eprintln!(
            "papers: structure sidecar not written (`papers structure {path}` builds it): {e}"
        ),
    }
}

/// `read --extraction docling`: Docling's Markdown of the saved PDF, as a
/// second text `<name>.docling.md` with its manifest `<name>.docling.json`.
/// The plain text, its manifest and its entry stay as they are. The PDF is
/// downloaded only when none is saved (or with `--force`).
#[allow(clippy::too_many_arguments)]
fn read_docling(
    http: &Http,
    config: &config::Config,
    o: &ReadOptions,
    pdftotext: &str,
    out_dir: PathBuf,
    input: &str,
    reference: Option<&Ref>,
    name: String,
    entry: Option<registry::Reference>,
) -> Result<bool, String> {
    let md_path = out_dir.join(format!("{name}{}", docling::TEXT_SUFFIX));
    let manifest = out_dir.join(format!("{name}{}", docling::MANIFEST_SUFFIX));
    let pdf_path = out_dir.join(format!("{name}.pdf"));
    let read_text = |path: &Path| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str::<registry::Text>(&json).ok())
    };
    // The Docling texts recorded for this paper: its manifest first, then the
    // entry's.
    let entry_texts = entry.iter().flat_map(|r| r.texts.iter().cloned());
    let recorded: Vec<registry::Text> = read_text(&manifest)
        .into_iter()
        .chain(entry_texts.clone().filter(is_docling))
        .collect();
    let saved_pdf = std::fs::read(&pdf_path).ok();
    let pdf_sha = saved_pdf.as_deref().map(http::sha256_hex);
    if !o.force
        && let Some(t) = recorded.iter().find(|t| {
            sha256_file(&config.root.join(&t.path)).as_deref() == Some(t.sha256.as_str())
                && (pdf_sha.is_none() || pdf_sha.as_deref() == t.pdf_sha256())
        })
    {
        println!(
            "already have {} (sha256 matches); not converting (--force to convert again)",
            t.path
        );
        return finish_text(
            config,
            o,
            entry.as_ref(),
            &manifest,
            &md_path,
            t.clone(),
            false,
        );
    }
    // Every text that names the PDF it was made from: the PDF's origin and
    // download time stay those recorded for it.
    let known: Vec<registry::Text> = recorded
        .iter()
        .cloned()
        .chain(read_text(&out_dir.join(format!("{name}.json"))))
        .chain(entry_texts)
        .filter(|t| t.pdf_sha256().is_some())
        .collect();
    let local = match (saved_pdf, &pdf_sha) {
        (Some(bytes), Some(sha)) => match known.iter().find(|t| t.pdf_sha256() == Some(sha)) {
            Some(t) => Some((bytes, t.origin.clone(), t.retrieved.clone())),
            None if o.force => None,
            None => {
                return Err(format!(
                    "{} has no sha256 recorded for this paper's texts, so it may not be the \
                     PDF they were read from; `papers registry record-pdfs` records it when it \
                     reproduces the text, or --force downloads the PDF again",
                    config.display(&pdf_path)
                ));
            }
        },
        _ => None,
    };
    // Checked before any download.
    let version = docling::version(&config.docling).map_err(|e| {
        format!(
            "Docling extraction needs Docling ({e}); install uv, or set [papers] docling \
             to a command that runs Python with Docling"
        )
    })?;
    println!("docling: {version}");
    let target = Target {
        config,
        dir: out_dir,
        name,
        title: entry
            .as_ref()
            .and_then(|r| r.title.clone())
            .unwrap_or_default(),
        extraction: registry::Extraction::Docling,
        pdftotext,
        docling: &config.docling,
    };
    let saved = match local {
        Some((bytes, origin, retrieved)) => {
            println!("converting {} (sha256 matches)", config.display(&pdf_path));
            Some(extract_pdf(&target, &bytes, origin, retrieved)?)
        }
        None => {
            let mut browse = |pages: &[String], target: &Target| {
                if o.browser {
                    try_in_browser(pages, target)
                } else {
                    println!(
                        "not retrying the JavaScript check page(s) in a browser (--no-browser)"
                    );
                    Ok(None)
                }
            };
            download(http, input, reference, target, &mut browse)?
        }
    };
    match saved {
        Some(Saved::Text(t)) => {
            finish_text(config, o, entry.as_ref(), &manifest, &md_path, t, true)
        }
        Some(Saved::PdfOnly) => Ok(false),
        None => {
            println!("no candidate yielded a PDF; open the landing page in a browser");
            Ok(false)
        }
    }
}

/// Records a read text, writes its manifest, then moves the staged text
/// (see [`derivative::pending`]) into place when `staged`: a reader never
/// finds the text without its record. When a step fails, the steps before it
/// are undone: the staged text is removed, the old manifest is put back, and
/// the entry gets back the text it had. The structure sidecar comes last.
fn finish_text(
    config: &config::Config,
    o: &ReadOptions,
    entry: Option<&registry::Reference>,
    manifest: &Path,
    md_path: &Path,
    text: registry::Text,
    staged: bool,
) -> Result<bool, String> {
    let line = serde_json::to_string(&text).expect("texts serialize");
    let old_manifest = std::fs::read(manifest).ok();
    let mut recorded = None;
    let mut manifest_written = false;
    let written = (|| {
        if o.record
            && let Some(entry) = entry
        {
            record_read(config, entry, text.clone())?;
            recorded = Some(entry);
        }
        derivative::write(manifest, format!("{line}\n").as_bytes())?;
        manifest_written = true;
        if staged {
            derivative::commit(md_path)?;
        }
        Ok::<_, String>(())
    })();
    if let Err(e) = written {
        if staged {
            derivative::discard(md_path);
        }
        let mut undo = Vec::new();
        if manifest_written {
            let restored = match &old_manifest {
                Some(bytes) => derivative::write(manifest, bytes),
                None => std::fs::remove_file(manifest)
                    .map_err(|e| format!("{}: {e}", manifest.display())),
            };
            undo.extend(restored.err());
        }
        if let Some(entry) = recorded {
            undo.extend(unrecord_text(config, entry, &text).err());
        }
        return Err(match undo.is_empty() {
            true => e,
            false => format!("{e}; undoing it also failed: {}", undo.join("; ")),
        });
    }
    // A plain or layout text's path was printed when it was extracted.
    if staged && is_docling(&text) {
        println!("text: {}", config.display(md_path));
    }
    structure_after_read(config, entry, text);
    println!("{line}");
    Ok(true)
}

/// Takes the read text `text` out of `entry` in the registry and puts back
/// the text it replaced when `read` started, if any (see `record_text`).
fn unrecord_text(
    config: &config::Config,
    entry: &registry::Reference,
    text: &registry::Text,
) -> Result<(), String> {
    let docling = is_docling(text);
    let previous = entry
        .texts
        .iter()
        .find(|t| t.path == text.path || (docling && is_docling(t)))
        .cloned();
    registry::update(&config.registry, |records| {
        if let Some(r) = records.iter_mut().find(|r| r.key == entry.key)
            && let Some(at) = r
                .texts
                .iter()
                .position(|t| t.path == text.path && t.sha256 == text.sha256)
        {
            r.texts.remove(at);
            if let Some(previous) = previous {
                r.texts.insert(at, previous);
            }
        }
        Ok(())
    })
}

/// Retries JavaScript check pages in a browser: `try_in_browser`, or a
/// stand-in in tests.
type Browse<'b> = dyn FnMut(&[String], &Target) -> Result<Option<Saved>, String> + 'b;

/// Finds and downloads an open-access PDF; `None` when no candidate was one.
fn download(
    http: &Http,
    input: &str,
    reference: Option<&Ref>,
    mut target: Target,
    browse: &mut Browse,
) -> Result<Option<Saved>, String> {
    let candidates = match reference {
        Some(Ref::Arxiv(id)) => {
            let paper = Client::new(http)
                .fetch(std::slice::from_ref(id))
                .remove(0)
                .map_err(|e| format!("{input}: {e}"))?;
            println!("{}", paper_text(&paper, None, None, true, false));
            target.title = paper.title;
            vec![Candidate::Pdf(format!("https://arxiv.org/pdf/{id}"))]
        }
        Some(Ref::Doi(doi)) => {
            let (title, candidates) = doi_candidates(http, doi)?.ok_or_else(|| {
                format!("not found: neither Crossref nor OpenAlex knows doi:{doi}")
            })?;
            target.title = title;
            candidates
        }
        None if input.to_ascii_lowercase().ends_with(".pdf") => {
            vec![Candidate::Pdf(input.to_owned())]
        }
        None => vec![Candidate::Landing(input.to_owned())],
    };
    let mut tried = Vec::new();
    // Candidates whose plain-HTTP reply was a JavaScript check page.
    let mut challenged = Vec::new();
    // For a URL without a paper id: the DOI its landing page names.
    let mut page_doi = None;
    if candidates.is_empty() {
        println!("no open-access full-text locations known for {input}");
    } else if let Some(saved) = try_candidates(
        http,
        candidates,
        &mut tried,
        &mut challenged,
        reference.is_none().then_some(&mut page_doi),
        &target,
    )? {
        return Ok(Some(saved));
    }
    let mut doi = match reference {
        Some(Ref::Doi(doi)) => Some(doi.clone()),
        _ => None,
    };
    if reference.is_none() {
        // The copy the user named comes first: its JavaScript check pages are
        // retried in the browser before other copies are looked for. A browser
        // failure here leaves those other copies to try.
        match retry_challenged(&mut challenged, &target, browse) {
            Ok(Some(saved)) => return Ok(Some(saved)),
            Ok(None) => {}
            Err(e) => println!("browser: {e}"),
        }
    }
    // A URL whose landing page names a DOI: try the DOI's open-access
    // locations, and its other versions below.
    if let Some(page_doi) = page_doi {
        println!("the landing page names doi:{page_doi}");
        match doi_candidates(http, &page_doi) {
            Ok(Some((title, found))) => {
                if target.title.is_empty() {
                    target.title = title;
                }
                let more: Vec<Candidate> =
                    found.into_iter().filter(|c| !tried.contains(c)).collect();
                if !more.is_empty()
                    && let Some(saved) =
                        try_candidates(http, more, &mut tried, &mut challenged, None, &target)?
                {
                    return Ok(Some(saved));
                }
                doi = Some(page_doi);
            }
            Ok(None) => println!("neither Crossref nor OpenAlex knows doi:{page_doi}"),
            Err(e) => println!("doi:{page_doi}: {e}"),
        }
    }
    let title = target.title.clone();
    // A closed journal version often has an open working-paper or preprint
    // version under the same title, indexed as a separate work.
    if let Some(doi) = &doi
        && !title.is_empty()
    {
        // The search is best effort: its failure leaves the browser retry below.
        match openalex::Client::new(http).search_title(&title, OTHER_VERSIONS) {
            Err(e) => println!("other versions: the OpenAlex title search failed: {e}"),
            Ok(found) => {
                let others: Vec<openalex::Work> = found
                    .into_iter()
                    .filter(|w| {
                        notes::title_matches(&title, &w.title) && w.doi.as_ref() != Some(doi)
                    })
                    .collect();
                let alternates: Vec<Candidate> = others
                    .iter()
                    .flat_map(|w| oa_candidates(&w.oa_locations))
                    .filter(|c| !tried.contains(c))
                    .collect();
                if alternates.is_empty() {
                    println!("no other open-access version with the same title found");
                } else {
                    println!("other versions with the same title:");
                    for w in &others {
                        let doi = w
                            .doi
                            .as_ref()
                            .map(|d| format!(" doi:{d}"))
                            .unwrap_or_default();
                        println!("  {}{doi}", w.id);
                    }
                    if let Some(saved) = try_candidates(
                        http,
                        alternates,
                        &mut tried,
                        &mut challenged,
                        None,
                        &target,
                    )? {
                        return Ok(Some(saved));
                    }
                }
            }
        }
    }
    retry_challenged(&mut challenged, &target, browse)
}

/// Looks `doi` up and prints it. Returns its title and the open-access
/// candidates OpenAlex lists for it; `None` when neither Crossref nor OpenAlex
/// knows it.
fn doi_candidates(http: &Http, doi: &Doi) -> Result<Option<(String, Vec<Candidate>)>, String> {
    let Some(r) = Resolved::fetch(http, doi)? else {
        return Ok(None);
    };
    print_resolved(&r, None, true, false);
    let locations = r.openalex.as_ref().map_or(&[][..], |o| &o.oa_locations);
    Ok(Some((
        r.record().titles[0].clone(),
        oa_candidates(locations),
    )))
}

/// Retries the JavaScript check pages in `challenged` with `browse`, and
/// empties `challenged`.
fn retry_challenged(
    challenged: &mut Vec<String>,
    target: &Target,
    browse: &mut Browse,
) -> Result<Option<Saved>, String> {
    if challenged.is_empty() {
        return Ok(None);
    }
    browse(&std::mem::take(challenged), target)
}

/// Retries in headless Chromium the candidates that answered with a JavaScript
/// check page (see `pdf::is_js_challenge`), following PDF links on the pages it
/// loads. URLs refused with 403, 429, or an error never come here. Returns like
/// `try_candidates`.
fn try_in_browser(challenged: &[String], target: &Target) -> Result<Option<Saved>, String> {
    let mut queue: Vec<String> = Vec::new();
    for url in challenged {
        if !queue.contains(url) && queue.len() < BROWSER_URLS {
            queue.push(url.clone());
        }
    }
    println!("retrying in headless Chromium:");
    let browser = browser::Browser::launch()?;
    let mut i = 0;
    while i < queue.len() {
        let url = queue[i].clone();
        i += 1;
        let loaded = http::throttled(&http::host(&url)?, || browser.load(&url, PDF_LIMIT))?;
        match loaded {
            Ok(browser::Loaded::Download(bytes)) if bytes.starts_with(b"%PDF-") => {
                return save_pdf(target, &bytes, &url).map(Some);
            }
            Ok(browser::Loaded::Download(_)) => println!("browser: {url}: download is not a PDF"),
            Ok(browser::Loaded::Page { html, url: page }) => {
                let links: Vec<String> = pdf::page_pdf_links(&html, &page)
                    .into_iter()
                    .filter(|l| !queue.contains(l))
                    .collect();
                println!(
                    "browser: {url}: page loaded; {} PDF link(s) on it",
                    links.len()
                );
                for (n, link) in links.into_iter().enumerate() {
                    println!("  {link}");
                    queue.insert(i + n, link);
                }
            }
            Ok(browser::Loaded::Blocked(why)) => {
                println!("browser: {url}: the host blocks automated browsers ({why})")
            }
            Err(e) => println!("browser: {url}: {e}"),
        }
    }
    Ok(None)
}

/// Tries `candidates` in order, expanding a landing page into the PDF links it
/// carries, until one is a PDF. Saves it and returns whether text was extracted;
/// `None` when no candidate was a PDF. Every URL attempted is added to `tried`,
/// those that answered with a JavaScript check page to `challenged`. When
/// `page_doi` is given and empty, it is set to the first DOI a landing page
/// names in its metadata.
fn try_candidates(
    http: &Http,
    mut candidates: Vec<Candidate>,
    tried: &mut Vec<Candidate>,
    challenged: &mut Vec<String>,
    mut page_doi: Option<&mut Option<Doi>>,
    target: &Target,
) -> Result<Option<Saved>, String> {
    println!("full-text candidates:");
    for c in &candidates {
        println!("  {}", c.url());
    }
    let mut i = 0;
    while i < candidates.len() {
        let candidate = candidates[i].clone();
        i += 1;
        tried.push(candidate.clone());
        let url = candidate.url();
        let reply = match http.get(&Get {
            url,
            // 202 is how AWS WAF serves its JavaScript challenge.
            accept: &[200, 202],
            attempts: PDF_ATTEMPTS,
            retry_429: false,
            body_limit: PDF_LIMIT,
            headers: &[],
            headers_host: "",
            cache: false,
        }) {
            Ok(reply) => reply,
            Err(e) => {
                println!("skip {url}: {e}");
                continue;
            }
        };
        if pdf::is_pdf(&reply.content_type, &reply.body) {
            return save_pdf(target, &reply.body, &reply.url).map(Some);
        }
        let html = reply.content_type.contains("html");
        let found: Vec<Candidate> = if matches!(candidate, Candidate::Landing(_)) && html {
            let page = reply.text();
            if let Some(slot) = page_doi.as_deref_mut()
                && slot.is_none()
            {
                *slot = pdf::page_doi(&page);
            }
            pdf::page_pdf_links(&page, &reply.url)
                .into_iter()
                .map(Candidate::Pdf)
                .filter(|c| !candidates.contains(c) && !tried.contains(c))
                .collect()
        } else {
            Vec::new()
        };
        if found.is_empty() && pdf::is_js_challenge(reply.status, &reply.content_type, &reply.body)
        {
            println!("skip {url}: JavaScript check page (HTTP {})", reply.status);
            challenged.push(url.to_owned());
        } else if matches!(candidate, Candidate::Landing(_)) && html {
            println!(
                "skip {url}: landing page; {} PDF link(s) found on it",
                found.len()
            );
            for (n, c) in found.into_iter().enumerate() {
                println!("  {}", c.url());
                candidates.insert(i + n, c);
            }
        } else {
            println!(
                "skip {url}: not a PDF (content type {:?})",
                reply.content_type
            );
        }
    }
    Ok(None)
}

/// Writes `<dir>/<name>.pdf` and stages its extracted text `<dir>/<name>.txt`
/// (see [`extract_pdf`]); an unchanged PDF is not rewritten.
fn save_pdf(target: &Target, body: &[u8], source: &str) -> Result<Saved, String> {
    let (dir, name) = (&target.dir, &target.name);
    debug_assert!(!name.contains(['/', '\\']) && !name.starts_with('.'));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let pdf_path = dir.join(format!("{name}.pdf"));
    println!("source: {source}");
    if sha256_file(&pdf_path).as_deref() == Some(http::sha256_hex(body).as_str()) {
        println!("pdf: {} (unchanged)", pdf_path.display());
    } else {
        std::fs::write(&pdf_path, body).map_err(|e| format!("{}: {e}", pdf_path.display()))?;
        println!("pdf: {}", pdf_path.display());
    }
    extract_pdf(target, body, Some(source.to_owned()), Some(finding::now()))
}

/// Converts `<dir>/<name>.pdf`, whose bytes are `body`, with Docling into the
/// staged `<dir>/<name>.docling.md` (see [`derivative::pending`]); the caller
/// moves it into place once its record is written. NUL bytes are dropped.
fn docling_text(
    target: &Target,
    body: &[u8],
    origin: Option<String>,
    retrieved: Option<String>,
) -> Result<Saved, String> {
    let pdf_path = target.dir.join(format!("{}.pdf", target.name));
    let md_path = target
        .dir
        .join(format!("{}{}", target.name, docling::TEXT_SUFFIX));
    let tmp = derivative::pending(&md_path);
    let staged = docling::convert(target.docling, &pdf_path, &tmp).and_then(|version| {
        let raw = std::fs::read(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let text = String::from_utf8_lossy(&raw).replace('\0', "");
        if text.as_bytes() != raw {
            std::fs::write(&tmp, &text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        }
        Ok((version, text))
    });
    let (version, text) = staged.map_err(|e| {
        derivative::discard(&md_path);
        format!("Docling did not convert {}: {e}", pdf_path.display())
    })?;
    println!(
        "text: {} words, extracted by docling {version}",
        text.split_whitespace().count()
    );
    if !pdf::has_title(&text, &target.title) {
        println!(
            "warning: the title does not appear in the extracted text; check this is the right paper"
        );
    }
    let mut saved = registry::Text {
        path: target.config.display(&md_path),
        sha256: http::sha256_hex(text.as_bytes()),
        origin,
        retrieved,
        extra: Default::default(),
    };
    saved.set_extraction(registry::Extraction::Docling);
    saved.set_pdf_sha256(http::sha256_hex(body));
    saved
        .extra
        .insert("extractor".to_owned(), format!("docling {version}").into());
    Ok(Saved::Text(saved))
}

/// Extracts the text of `<dir>/<name>.pdf`, whose bytes are `body`, into the
/// staged `<dir>/<name>.txt` (see [`derivative::pending`]), and returns its record with the PDF's `origin`, download
/// time and sha256.
fn extract_pdf(
    target: &Target,
    body: &[u8],
    origin: Option<String>,
    retrieved: Option<String>,
) -> Result<Saved, String> {
    if target.extraction == registry::Extraction::Docling {
        return docling_text(target, body, origin, retrieved);
    }
    let pdf_path = target.dir.join(format!("{}.pdf", target.name));
    let txt_path = target.dir.join(format!("{}.txt", target.name));
    let chosen = match target.extraction {
        registry::Extraction::Docling => unreachable!("handled above"),
        registry::Extraction::Plain => pdf::choose_text(
            pdf::extract_text(body),
            &pdf_path,
            &target.title,
            target.pdftotext,
            pdf::PDFTOTEXT_TIMEOUT,
            pdf::PDFTOTEXT_MAX_OUTPUT,
        ),
        // Only pdftotext keeps the layout: no fallback to pdf-extract.
        registry::Extraction::Layout => pdf::pdftotext(
            target.pdftotext,
            &pdf_path,
            true,
            pdf::PDFTOTEXT_TIMEOUT,
            pdf::PDFTOTEXT_MAX_OUTPUT,
        )
        .map(|text| pdf::Chosen {
            text,
            extractor: "pdftotext -layout",
            note: None,
        }),
    };
    let pdf::Chosen {
        text,
        extractor,
        note,
    } = match chosen {
        Ok(chosen) => chosen,
        Err(e) => {
            println!("text: not written ({e})");
            return Ok(Saved::PdfOnly);
        }
    };
    if let Some(note) = note {
        println!("text: {note}");
    }
    // Staged: the caller moves it into place once its record is written.
    derivative::stage(&txt_path, text.as_bytes())?;
    println!(
        "text: {} ({} words, extracted by {extractor})",
        txt_path.display(),
        text.split_whitespace().count()
    );
    if !pdf::has_title(&text, &target.title) {
        println!(
            "warning: the title does not appear in the extracted text; check this is the right paper"
        );
    }
    let mut saved = registry::Text {
        path: target.config.display(&txt_path),
        sha256: http::sha256_hex(text.as_bytes()),
        origin,
        retrieved,
        extra: Default::default(),
    };
    saved.set_extraction(target.extraction);
    saved.set_pdf_sha256(http::sha256_hex(body));
    Ok(Saved::Text(saved))
}

struct VerifyOptions {
    keys: Vec<String>,
    unverified: bool,
    limit: Option<usize>,
    record: bool,
    notes: bool,
    format: finding::Format,
}

fn verify(http: &Http, config: &config::Config, o: &VerifyOptions) -> Result<bool, String> {
    let registry = if o.notes {
        None
    } else {
        registry::load(&config.registry)?
    };
    if o.record && registry.is_none() {
        return Err(format!(
            "--record writes the registry, and there is none at {}",
            config.display(&config.registry)
        ));
    }
    let mut subjects: Vec<verify::Subject> = match &registry {
        Some(records) => records
            .iter()
            .map(verify::Subject::from_reference)
            .collect(),
        None => notes::load(&config.notes)?
            .ok_or_else(|| {
                format!(
                    "nothing to verify: no registry {} and no notes directory {}",
                    config.display(&config.registry),
                    config.display(&config.notes)
                )
            })?
            .iter()
            .map(|n| verify::Subject::from_note(n, &config.display(&n.path)))
            .collect(),
    };
    let unknown: Vec<&str> = o
        .keys
        .iter()
        .filter(|k| !subjects.iter().any(|s| &s.key == *k))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!("unknown keys: {}", unknown.join(", ")));
    }
    if !o.keys.is_empty() {
        subjects.retain(|s| o.keys.contains(&s.key));
    }
    if o.unverified
        && let Some(records) = &registry
    {
        subjects.retain(|s| {
            records
                .iter()
                .any(|r| r.key == s.key && r.verified.is_none())
        });
    }
    if let Some(n) = o.limit {
        subjects.truncate(n);
    }
    let outcomes = verify::run(http, &subjects);
    let findings: Vec<finding::Finding> = subjects
        .iter()
        .zip(&outcomes)
        .flat_map(|(s, outcome)| outcome.findings(s))
        .collect();
    let ok = finding::emit(&findings, o.format);
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for outcome in &outcomes {
        *counts.entry(outcome.label()).or_default() += 1;
    }
    let counts: Vec<String> = counts.iter().map(|(l, n)| format!("{n} {l}")).collect();
    eprintln!(
        "papers: verified {} entries: {}",
        subjects.len(),
        counts.join(", ")
    );
    if o.record && registry.is_some() {
        let today = jiff::Zoned::now().date().to_string();
        let results: HashMap<&str, registry::Verified> = subjects
            .iter()
            .zip(&outcomes)
            .filter_map(|(s, outcome)| {
                let (against, status) = outcome.verified()?;
                Some((
                    s.key.as_str(),
                    registry::Verified {
                        at: today.clone(),
                        against: against.to_lowercase(),
                        status: status.to_owned(),
                    },
                ))
            })
            .collect();
        // Applied to the registry as it is now, not the copy loaded at the start.
        let written = registry::update(&config.registry, |records| {
            let mut written = 0;
            for r in records.iter_mut() {
                if let Some(v) = results.get(r.key.as_str()) {
                    r.verified = Some(v.clone());
                    written += 1;
                }
            }
            Ok(written)
        })?;
        eprintln!(
            "papers: recorded {written} results in {}",
            config.display(&config.registry)
        );
    }
    let failed = outcomes
        .iter()
        .filter(|o| matches!(o, verify::Outcome::Failed { .. }))
        .count();
    if failed > 0 {
        return Err(format!(
            "{failed} lookups failed; rerun to retry them (successful responses are cached)"
        ));
    }
    Ok(ok)
}

/// Frontmatter values for a DOI stub.
fn doi_stub_fields(r: &Resolved) -> Result<notes::StubFields, String> {
    let rec = r.record();
    let (year, detail) = r.earliest_year();
    let year = year.ok_or_else(|| format!("doi:{}: no source reports a year", r.doi))?;
    Ok(notes::StubFields {
        title: rec.titles.last().cloned().unwrap_or_default(),
        authors: rec.authors,
        year,
        year_comment: Some(format!(
            "TODO: confirm the first public version's year (sources report {detail})"
        )),
        venue: r.venue().unwrap_or_default(),
        url: r.doi.url(),
    })
}

fn stub(http: &Http, config: &config::Config, input: &str, write: bool) -> Result<bool, String> {
    let papers = config.notes.as_path();
    let fields = match Ref::parse(input)? {
        Ref::Arxiv(id) => {
            let paper = Client::new(http)
                .fetch(std::slice::from_ref(&id))
                .remove(0)
                .map_err(|e| format!("{input}: {e}"))?;
            notes::StubFields::from_paper(&paper)
        }
        Ref::Doi(doi) => {
            let r = Resolved::fetch(http, &doi)?.ok_or_else(|| {
                format!("{input}: not found: neither Crossref nor OpenAlex knows doi:{doi}")
            })?;
            doi_stub_fields(&r)?
        }
    };
    let template_path = &config.template;
    let template = std::fs::read_to_string(template_path)
        .map_err(|e| format!("template {}: {e}", template_path.display()))?;
    let added = jiff::Zoned::now().date().to_string();
    let note = notes::stub_fields(&fields, &template, &added)?;
    if !write {
        print!("{note}");
        return Ok(true);
    }
    let path = papers.join(format!("{}.md", fields.note_id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                format!("{} exists; refusing to overwrite", path.display())
            }
            _ => format!("{}: {e}", path.display()),
        })?;
    file.write_all(note.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!("{}", config.display(&path));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn note_index_paths_are_relative_to_the_project_root() {
        let dir = std::env::temp_dir().join(format!("papers-note-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("research/papers")).unwrap();
        // An explicit config, so a research.toml above the temp dir is not used.
        std::fs::write(dir.join("research.toml"), "[papers]\n").unwrap();
        std::fs::write(
            dir.join("research/papers/2026-lee-synthetic.md"),
            "---\nid: 2026-lee-synthetic\ntitle: A Synthetic Title\nauthors: [Ann Lee]\nyear: 2026\nurl: https://doi.org/10.1234/synthetic\n---\n",
        )
        .unwrap();
        let config = config::Config::load(Some(&dir.join("research.toml")), &dir).unwrap();
        let index = seen::Index::load(&config);
        std::fs::remove_dir_all(&dir).unwrap();
        let index = index.unwrap();
        let doi = Doi::parse("10.1234/synthetic").unwrap();
        assert_eq!(
            doi_note(&index, &doi),
            Some("research/papers/2026-lee-synthetic.md")
        );
        // A registry entry with the same DOI is not a note.
        assert_eq!(index.note("doi:10.9999/none"), None);
    }

    /// A project whose registry holds key 87 for arXiv 2601.99901 (the first
    /// paper of the arXiv fixture), with one text, and its config.
    fn registered_project(name: &str) -> (PathBuf, config::Config) {
        let dir = std::env::temp_dir().join(format!("papers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("texts")).unwrap();
        std::fs::write(dir.join("research.toml"), "[papers]\n").unwrap();
        std::fs::write(dir.join("texts/87.txt"), "the recorded text").unwrap();
        let mut r = registry::Reference::new("87".into());
        r.title = Some("Rebalancing Costs of a Synthetic Market Maker".into());
        r.depth = Some("full".into());
        r.ids.arxiv = Some("2601.99901".into());
        r.ids.doi = Some("10.1234/Synthetic.87".into());
        r.ids.url = Some("https://example.org/papers/87/".into());
        r.texts = vec![registry::Text {
            path: "texts/87.txt".into(),
            sha256: http::sha256_hex(b"the recorded text"),
            origin: Some("https://arxiv.org/pdf/2601.99901v4".into()),
            retrieved: None,
            extra: serde_json::Map::new(),
        }];
        std::fs::write(dir.join("references.jsonl"), registry::render(&[r])).unwrap();
        let config = config::Config::load(Some(&dir.join("research.toml")), &dir).unwrap();
        (dir, config)
    }

    const QUIET: Output = Output {
        with_abstract: false,
        json: false,
        log: false,
    };

    const ARXIV_FEED: &str = include_str!("../tests/fixtures/arxiv-entries.xml");

    fn arxiv_feed() -> String {
        http::response(
            "200 OK",
            &["Content-Type: application/atom+xml"],
            ARXIV_FEED,
        )
    }

    #[test]
    fn fetch_skips_a_registered_paper_unless_refreshed() {
        let (dir, config) = registered_project("fetch-registered");
        let http = Http::new();
        let ids = ["arXiv:2601.99901v4".to_owned()];
        // No response is queued: a request would fail the fetch.
        let (base, server) = http::serve(Vec::new());
        let endpoint = format!("{base}/api/query");
        let arxiv = Client::at(&http, &endpoint);
        assert_eq!(fetch(&arxiv, &http, &config, &ids, QUIET, false), Ok(true));
        assert!(server.join().unwrap().is_empty());
        // By DOI and by URL too: a lookup of this DOI would not find it, and a
        // URL is not an id that `fetch` can look up.
        let other = [
            "https://doi.org/10.1234/synthetic.87".to_owned(),
            "http://example.org/papers/87".to_owned(),
        ];
        assert_eq!(
            fetch(&arxiv, &http, &config, &other, QUIET, false),
            Ok(true)
        );
        // `--refresh` asks arXiv.
        let (base, server) = http::serve(vec![arxiv_feed()]);
        let endpoint = format!("{base}/api/query");
        let arxiv = Client::at(&http, &endpoint);
        assert_eq!(fetch(&arxiv, &http, &config, &ids, QUIET, true), Ok(true));
        let heads = server.join().unwrap();
        assert_eq!(heads.len(), 1);
        assert!(heads[0].contains("id_list=2601.99901v4"), "{}", heads[0]);
        // A paper the registry does not hold is fetched without `--refresh`.
        let (base, server) = http::serve(vec![arxiv_feed()]);
        let endpoint = format!("{base}/api/query");
        let arxiv = Client::at(&http, &endpoint);
        let both = ["2601.99901".to_owned(), "2601.99902".to_owned()];
        assert_eq!(fetch(&arxiv, &http, &config, &both, QUIET, false), Ok(true));
        let heads = server.join().unwrap();
        assert!(heads[0].contains("id_list=2601.99902&"), "{}", heads[0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fetch_by_title_skips_a_registered_title() {
        let (dir, config) = registered_project("fetch-title-registered");
        // A lookup would go to OpenAlex and fail or differ; none is made.
        let got = fetch_title(
            &Http::new(),
            &config,
            "rebalancing costs of a synthetic market-maker",
            QUIET,
            false,
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(got, Ok(true));
    }

    #[test]
    fn read_gives_the_recorded_text_of_a_registered_paper_without_a_download() {
        let (dir, config) = registered_project("read-registered");
        let out = dir.join("out");
        let options = |extraction| ReadOptions {
            reference: Some("https://arxiv.org/abs/2601.99901".into()),
            key: None,
            out: Some(out.clone()),
            record: false,
            force: false,
            extraction,
            browser: false,
        };
        let absent = pdf::tests::fake_program("absent");
        // A download would ask arXiv and write into `out`; nothing is written.
        assert_eq!(
            read(&Http::new(), &config, &options(None), &absent),
            Ok(true)
        );
        assert_eq!(
            read(
                &Http::new(),
                &config,
                &options(Some(registry::Extraction::Plain)),
                &absent
            ),
            Ok(true)
        );
        assert!(!out.exists());
        // A registry that does not load does not stop a read; this one then
        // stops at the missing pdftotext, before any request.
        let registry = std::fs::read_to_string(&config.registry).unwrap();
        std::fs::write(&config.registry, "not json\n").unwrap();
        let err = read(
            &Http::new(),
            &config,
            &options(Some(registry::Extraction::Layout)),
            &absent,
        )
        .unwrap_err();
        assert!(err.contains("layout extraction needs pdftotext"), "{err}");
        // A later recorded text in the asked mode is the one reused.
        std::fs::write(dir.join("texts/87-layout.txt"), "the  layout  text").unwrap();
        let mut entries = registry::parse(&registry, "registry").unwrap();
        let mut layout = entries[0].texts[0].clone();
        layout.path = "texts/87-layout.txt".into();
        layout.sha256 = http::sha256_hex(b"the  layout  text");
        layout.set_extraction(registry::Extraction::Layout);
        entries[0].texts.push(layout);
        std::fs::write(&config.registry, registry::render(&entries)).unwrap();
        assert_eq!(
            read(
                &Http::new(),
                &config,
                &options(Some(registry::Extraction::Layout)),
                &absent
            ),
            Ok(true)
        );
        std::fs::write(&config.registry, registry).unwrap();
        // Another mode is not answered by the recorded text: the run goes on to
        // extract, and stops at the missing pdftotext before any request.
        let err = read(
            &Http::new(),
            &config,
            &options(Some(registry::Extraction::Layout)),
            &absent,
        )
        .unwrap_err();
        assert!(err.contains("layout extraction needs pdftotext"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_marks_registered_results_and_finds_the_earlier_run() {
        let (dir, config) = registered_project("search-registered");
        let http = Http::new();
        let logging = Output { log: true, ..QUIET };
        let run = || {
            let (base, server) = http::serve(vec![arxiv_feed()]);
            let endpoint = format!("{base}/api/query");
            let got = search(
                &Client::at(&http, &endpoint),
                &config,
                ("Synthetic  Market", "all:synthetic AND all:market"),
                Sort::Submitted,
                3,
                serde_json::json!({"arxiv_query": "all:synthetic AND all:market"}),
                logging,
            );
            server.join().unwrap();
            got
        };
        let same = searchlog::Same {
            source: "arxiv",
            query: "synthetic market",
            arxiv_query: Some("all:synthetic AND all:market"),
            since: None,
        };
        assert_eq!(searchlog::earlier(&config.search_log, &same), Ok(None));
        assert_eq!(run(), Ok(true));
        let first = searchlog::earlier(&config.search_log, &same)
            .unwrap()
            .unwrap();
        assert_eq!((first.results, first.runs), (3, 1));
        assert_eq!(
            earlier_line(&first, "arxiv", ".research/papers/search.jsonl"),
            format!(
                "papers: searched before: last run {}, 3 results (1 run of this arxiv \
                 query in .research/papers/search.jsonl)",
                first.at
            )
        );
        assert_eq!(run(), Ok(true));
        let second = searchlog::earlier(&config.search_log, &same)
            .unwrap()
            .unwrap();
        assert_eq!(second.runs, 2);
        // The registered paper's result starts with its key; another has no mark.
        let index = seen::Index::load(&config).unwrap();
        let papers = atom::parse_feed(ARXIV_FEED).unwrap().papers;
        let text = |p: &Paper, json: bool| {
            let seen = searchlog::seen_keys(&index, &paper_found(p));
            paper_text(p, None, Some(&seen), false, json)
        };
        assert!(
            text(&papers[0], false).starts_with("seen: key 87\n2601.99901v4 · published "),
            "{}",
            text(&papers[0], false)
        );
        assert!(text(&papers[1], false).starts_with("2601.99902v1 · "));
        let json: serde_json::Value = serde_json::from_str(&text(&papers[0], true)).unwrap();
        assert_eq!(json["seen"], serde_json::json!(["87"]));
        let hit = sources::Hit {
            id: "openalex:W1".into(),
            title: "Rebalancing costs of a synthetic market maker".into(),
            authors: vec!["Ann Lee".into()],
            year: Some(2026),
            venue: None,
            doi: None,
            arxiv: None,
            url: None,
            abstract_text: None,
        };
        let seen = searchlog::seen_keys(&index, &hit.found());
        assert_eq!(
            hit_text(&hit, &seen, QUIET),
            "seen: key 87\nopenalex:W1 · 2026\nRebalancing costs of a synthetic market maker\nAnn Lee\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_javascript_check_pages_are_queued_for_the_browser() {
        use crate::http::response;
        let html = ["Content-Type: text/html"];
        let notice = "<html>JavaScript is required to view this page.</html>";
        let (base, server) = http::serve(vec![
            response("403 Forbidden", &html, notice),
            response("429 Too Many Requests", &html, notice),
            response(
                "202 Accepted",
                &html,
                "<html><script src=c.js></script></html>",
            ),
            response(
                "200 OK",
                &html,
                "<html>An ordinary page without links</html>",
            ),
        ]);
        let candidates: Vec<Candidate> = ["blocked.pdf", "limited.pdf", "challenge", "plain"]
            .iter()
            .map(|p| Candidate::Landing(format!("{base}/{p}")))
            .collect();
        let dir = std::env::temp_dir().join(format!("papers-browser-test-{}", std::process::id()));
        let config = config::Config::load(None, &dir).unwrap();
        let absent = pdf::tests::fake_program("absent");
        let target = Target {
            config: &config,
            dir: dir.clone(),
            name: "x".into(),
            title: "X".into(),
            extraction: registry::Extraction::Plain,
            pdftotext: &absent,
            docling: &[],
        };
        let (mut tried, mut challenged) = (Vec::new(), Vec::new());
        let saved = try_candidates(
            &Http::new(),
            candidates,
            &mut tried,
            &mut challenged,
            None,
            &target,
        );
        server.join().unwrap();
        assert!(saved.unwrap().is_none());
        assert_eq!(tried.len(), 4);
        assert_eq!(challenged, [format!("{base}/challenge")]);
        assert!(!dir.exists(), "nothing was saved");
    }

    /// A one-page PDF whose text is `text` (ASCII, no parentheses).
    fn tiny_pdf(text: &str) -> String {
        let stream = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out += &format!("{} 0 obj\n{body}\nendobj\n", i + 1);
        }
        let xref = out.len();
        out += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
        for o in offsets {
            out += &format!("{o:010} 00000 n \n");
        }
        out + &format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
    }

    fn read_url(url: String, out: &Path) -> Result<bool, String> {
        read_url_with(url, out, false, None, &pdf::tests::fake_program("absent"))
    }

    fn read_url_with(
        url: String,
        out: &Path,
        force: bool,
        extraction: Option<registry::Extraction>,
        pdftotext: &str,
    ) -> Result<bool, String> {
        let config = config::Config::load(None, out).unwrap();
        read(
            &Http::new(),
            &config,
            &ReadOptions {
                reference: Some(url),
                key: None,
                out: Some(out.to_path_buf()),
                record: false,
                force,
                extraction,
                browser: false,
            },
            pdftotext,
        )
    }

    /// The manifest sidecar `read` wrote for `url` in `dir`.
    fn manifest(dir: &Path, url: &str) -> registry::Text {
        let name = url_file_name(url).unwrap();
        let json = std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn reads_a_pdf_url_without_a_paper_id() {
        use crate::http::response;
        let pdf = tiny_pdf("Synthetic Working Paper");
        let (base, server) = http::serve(vec![response(
            "200 OK",
            &["Content-Type: application/pdf"],
            &pdf,
        )]);
        let dir = std::env::temp_dir().join(format!("papers-read-pdf-url-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let url = format!("{base}/files/working-paper.pdf");
        assert_eq!(read_url(url.clone(), &dir), Ok(true));
        server.join().unwrap();
        let text = manifest(&dir, &url);
        assert_eq!(text.origin.as_deref(), Some(url.as_str()));
        let retrieved: jiff::Timestamp = text.retrieved.as_deref().unwrap().parse().unwrap();
        assert!(jiff::Timestamp::now().duration_since(retrieved).as_secs() < 600);
        let saved =
            std::fs::read_to_string(dir.join(text.path.rsplit('/').next().unwrap())).unwrap();
        assert!(saved.contains("Synthetic Working Paper"), "{saved}");
        assert_eq!(text.sha256, http::sha256_hex(saved.as_bytes()));
        let pdf_sha256 = http::sha256_hex(pdf.as_bytes());
        assert_eq!(text.pdf_sha256(), Some(pdf_sha256.as_str()));
        // The structure sidecar lies beside the text and names its sha256.
        let name = text.path.rsplit('/').next().unwrap();
        let sidecar = std::fs::read_to_string(dir.join(format!("{name}.structure.json"))).unwrap();
        let sidecar: serde_json::Value = serde_json::from_str(&sidecar).unwrap();
        assert_eq!(sidecar["schema"], structure::SCHEMA);
        assert_eq!(sidecar["text"]["sha256"], text.sha256.as_str());
        assert_eq!(sidecar["text"]["path"], text.path.as_str());
        // The mode `read` used, though no registry entry records the text.
        assert_eq!(sidecar["text"]["extraction"], "plain");
        assert_eq!(
            sidecar["warnings"].as_array().unwrap().len(),
            1,
            "{sidecar}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A text recorded in layout mode is kept, and a later run does not
    /// replace it with plain text unless `--extraction plain` asks for that.
    #[test]
    fn read_keeps_a_layout_text_unless_told_otherwise() {
        use crate::http::response;
        use registry::Extraction::{Layout, Plain};
        let pdf = tiny_pdf("Synthetic Layout Paper");
        let reply = || response("200 OK", &["Content-Type: application/pdf"], &pdf);
        // One download for --force with pdftotext, one for the switch to plain.
        let (base, server) = http::serve(vec![reply(), reply()]);
        let dir = std::env::temp_dir().join(format!("papers-read-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("{base}/files/layout.pdf");
        let name = url_file_name(&url).unwrap();
        let read_with = |force, extraction, program: &str| {
            read_url_with(
                url.clone(),
                &dir,
                force,
                extraction,
                &pdf::tests::fake_program(program),
            )
        };
        // A layout text, as `read --extraction layout` or attach would leave it.
        let layout = "Synthetic      Layout      Paper\n";
        std::fs::write(dir.join(format!("{name}.txt")), layout).unwrap();
        let mut recorded = registry::Text {
            path: format!("{name}.txt"),
            sha256: http::sha256_hex(layout.as_bytes()),
            origin: Some(url.clone()),
            retrieved: Some("2026-11-07T10:00:00Z".into()),
            extra: Default::default(),
        };
        recorded.set_extraction(Layout);
        let sidecar = dir.join(format!("{name}.json"));
        std::fs::write(&sidecar, serde_json::to_string(&recorded).unwrap()).unwrap();

        // Without --extraction the recorded text is kept, with no request,
        // and with no need for pdftotext.
        assert_eq!(read_with(false, None, "absent"), Ok(true));
        assert_eq!(manifest(&dir, &url), recorded);
        // An explicit layout request is satisfied by it too.
        assert_eq!(read_with(false, Some(Layout), "absent"), Ok(true));

        // --force extracts again in the recorded layout mode. Without
        // pdftotext it fails before any request, and the layout text stays.
        let err = read_with(true, None, "absent").unwrap_err();
        assert!(err.contains("pdftotext"), "{err}");
        assert_eq!(manifest(&dir, &url), recorded);
        let kept = std::fs::read_to_string(dir.join(&recorded.path)).unwrap();
        assert_eq!(kept, layout);
        // With pdftotext it downloads and writes a layout text.
        assert_eq!(read_with(true, None, "prints"), Ok(true));
        let text = manifest(&dir, &url);
        assert_eq!(text.extraction(), Ok(Some(Layout)));
        assert_ne!(text.retrieved, recorded.retrieved);
        let written = std::fs::read_to_string(dir.join(&text.path)).unwrap();
        // The fake echoes its arguments: "text of $3 with $1 $2 $4".
        assert!(
            written.starts_with("text of UTF-8 with -layout -enc "),
            "{written}"
        );
        // Without the PDF's sha256 the switch to plain downloads again.
        let mut unmarked = text.clone();
        unmarked.extra.remove("pdf_sha256");
        std::fs::write(&sidecar, serde_json::to_string(&unmarked).unwrap()).unwrap();

        // Asking for plain extracts plain text over it, without pdftotext.
        assert_eq!(read_with(false, Some(Plain), "absent"), Ok(true));
        let heads = server.join().unwrap();
        assert_eq!(heads.len(), 2, "{heads:?}");
        let text = manifest(&dir, &url);
        assert_eq!(text.extraction(), Ok(Some(Plain)));
        let saved = std::fs::read_to_string(dir.join(&text.path)).unwrap();
        assert!(saved.contains("Synthetic Layout Paper"), "{saved}");
        assert_eq!(text.sha256, http::sha256_hex(saved.as_bytes()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A text replaced by hand and attached to the entry, beside a stale
    /// sidecar: `read --key` keeps the attached text and rewrites the sidecar.
    #[test]
    fn read_by_key_keeps_an_attached_layout_text_over_a_stale_sidecar() {
        let root =
            std::env::temp_dir().join(format!("papers-read-key-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sources/text")).unwrap();
        std::fs::write(
            root.join("research.toml"),
            "[papers]\nregistry = \"refs.jsonl\"\n",
        )
        .unwrap();
        let layout = "Synthetic      Layout\n";
        std::fs::write(root.join("sources/text/k.txt"), layout).unwrap();
        let mut attached = registry::Text {
            path: "sources/text/k.txt".into(),
            sha256: http::sha256_hex(layout.as_bytes()),
            origin: Some("https://example.org/k.pdf".into()),
            retrieved: Some("2026-11-07T10:00:00Z".into()),
            extra: Default::default(),
        };
        let stale = registry::Text {
            sha256: "0".repeat(64),
            origin: Some("https://example.org/old.pdf".into()),
            ..attached.clone()
        };
        attached.set_extraction(registry::Extraction::Layout);
        let mut entry = registry::Reference::new("k".into());
        // Never fetched: the recorded text is reused.
        entry.ids.url = Some("https://example.invalid/k.pdf".into());
        entry.texts.push(attached.clone());
        std::fs::write(
            root.join("refs.jsonl"),
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        let sidecar = root.join("sources/text/k.json");
        std::fs::write(&sidecar, serde_json::to_string(&stale).unwrap()).unwrap();

        let config = config::Config::load(None, &root).unwrap();
        let options = ReadOptions {
            reference: None,
            key: Some("k".into()),
            out: None,
            record: false,
            force: false,
            extraction: None,
            browser: false,
        };
        assert_eq!(
            read(
                &Http::new(),
                &config,
                &options,
                &pdf::tests::fake_program("absent")
            ),
            Ok(true)
        );
        let json = std::fs::read_to_string(&sidecar).unwrap();
        let text: registry::Text = serde_json::from_str(&json).unwrap();
        assert_eq!(text, attached);
        assert_eq!(
            std::fs::read_to_string(root.join("sources/text/k.txt")).unwrap(),
            layout
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A project with a registry entry `k` whose URL is `url`, and its text
    /// `sources/text/k.txt` with a manifest that records no mode, as `read`
    /// left it before modes were recorded.
    fn unmarked_project(name: &str, url: &str) -> (PathBuf, registry::Text) {
        let root = std::env::temp_dir().join(format!("papers-read-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sources/text")).unwrap();
        std::fs::write(
            root.join("research.toml"),
            "[papers]\nregistry = \"refs.jsonl\"\n",
        )
        .unwrap();
        let body = "synthetic plain text\n";
        std::fs::write(root.join("sources/text/k.txt"), body).unwrap();
        let text = registry::Text {
            path: "sources/text/k.txt".into(),
            sha256: http::sha256_hex(body.as_bytes()),
            origin: Some(url.to_owned()),
            retrieved: Some("2026-11-07T10:00:00Z".into()),
            extra: Default::default(),
        };
        std::fs::write(
            root.join("sources/text/k.json"),
            serde_json::to_string(&text).unwrap(),
        )
        .unwrap();
        // Another text of the entry, at another path, in layout mode.
        let mut tables = registry::Text {
            path: "sources/text/k-tables.txt".into(),
            sha256: "0".repeat(64),
            ..text.clone()
        };
        tables.set_extraction(registry::Extraction::Layout);
        let mut entry = registry::Reference::new("k".into());
        entry.ids.url = Some(url.to_owned());
        entry.texts.push(tables);
        std::fs::write(
            root.join("refs.jsonl"),
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        (root, text)
    }

    fn read_key(
        root: &Path,
        force: bool,
        extraction: Option<registry::Extraction>,
    ) -> Result<bool, String> {
        read_key_with(root, force, extraction, &pdf::tests::fake_program("absent"))
    }

    fn read_key_with(
        root: &Path,
        force: bool,
        extraction: Option<registry::Extraction>,
        pdftotext: &str,
    ) -> Result<bool, String> {
        let config = config::Config::load(None, root).unwrap();
        let options = ReadOptions {
            reference: None,
            key: Some("k".into()),
            out: None,
            record: false,
            force,
            extraction,
            browser: false,
        };
        read(&Http::new(), &config, &options, pdftotext)
    }

    fn sidecar_of(root: &Path) -> registry::Text {
        let json = std::fs::read_to_string(root.join("sources/text/k.json")).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    /// A text without a recorded mode counts as plain: an explicit plain
    /// request keeps it and records the mode, and --force extracts plain text,
    /// whatever mode the entry's other texts have.
    #[test]
    fn read_takes_the_mode_only_from_the_text_it_replaces() {
        use crate::http::response;
        let pdf = tiny_pdf("Synthetic Unmarked Paper");
        let (base, server) = http::serve(vec![response(
            "200 OK",
            &["Content-Type: application/pdf"],
            &pdf,
        )]);
        let url = format!("{base}/k.pdf");
        let (root, _) = unmarked_project("unmarked", &url);

        // The entry's layout text at another path does not set the mode.
        assert_eq!(read_key(&root, true, None), Ok(true));
        server.join().unwrap();
        let mut forced = sidecar_of(&root);
        assert_eq!(forced.extraction(), Ok(Some(registry::Extraction::Plain)));
        let saved = std::fs::read_to_string(root.join("sources/text/k.txt")).unwrap();
        assert!(saved.contains("Synthetic Unmarked Paper"), "{saved}");

        // An explicit plain request keeps an unmarked text and records its mode.
        forced.extra.remove("extraction");
        std::fs::write(
            root.join("sources/text/k.json"),
            serde_json::to_string(&forced).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_key(&root, false, Some(registry::Extraction::Plain)),
            Ok(true)
        );
        let kept = sidecar_of(&root);
        assert_eq!(kept.sha256, forced.sha256, "not downloaded");
        assert_eq!(kept.extraction(), Ok(Some(registry::Extraction::Plain)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A mode this version does not know is kept, and never guessed.
    #[test]
    fn read_does_not_guess_an_unknown_recorded_mode() {
        let url = "https://example.invalid/k.pdf";
        let (root, mut text) = unmarked_project("unknown-mode", url);
        text.extra.insert("extraction".into(), "ocr".into());
        let sidecar = root.join("sources/text/k.json");
        std::fs::write(&sidecar, serde_json::to_string(&text).unwrap()).unwrap();
        let before = std::fs::read_to_string(&sidecar).unwrap();

        // Kept as it is without --force, with no request.
        assert_eq!(read_key(&root, false, None), Ok(true));
        assert_eq!(sidecar_of(&root), text);
        // Refused with --force and no mode, before any request.
        let err = read_key(&root, true, None).unwrap_err();
        assert!(err.contains("mode \"ocr\""), "{err}");
        assert!(err.contains("--extraction"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap().trim(),
            before.trim()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A mode change extracts the text again from the saved PDF when it has
    /// the recorded sha256. The entry's URL does not resolve, so a download
    /// would find no PDF.
    #[test]
    fn read_switches_mode_from_the_saved_pdf() {
        use registry::Extraction::{Layout, Plain};
        let (root, mut text) = unmarked_project("local-pdf", "https://example.invalid/k.pdf");
        let pdf = tiny_pdf("Synthetic Local Paper");
        std::fs::write(root.join("sources/text/k.pdf"), &pdf).unwrap();
        text.set_pdf_sha256(http::sha256_hex(pdf.as_bytes()));
        std::fs::write(
            root.join("sources/text/k.json"),
            serde_json::to_string(&text).unwrap(),
        )
        .unwrap();
        let prints = pdf::tests::fake_program("prints");

        assert_eq!(read_key_with(&root, false, Some(Layout), &prints), Ok(true));
        let layout = sidecar_of(&root);
        let written = std::fs::read_to_string(root.join("sources/text/k.txt")).unwrap();
        // The fake echoes its arguments: "text of $3 with $1 $2 $4".
        assert!(
            written.starts_with("text of UTF-8 with -layout -enc "),
            "{written}"
        );
        assert_eq!(layout.sha256, http::sha256_hex(written.as_bytes()));
        assert_eq!(layout.extraction(), Ok(Some(Layout)));
        assert_eq!(layout.pdf_sha256(), text.pdf_sha256());
        // The PDF's origin and download time are kept.
        assert_eq!(
            (&layout.origin, &layout.retrieved),
            (&text.origin, &text.retrieved)
        );

        // And back to plain, from the same PDF.
        assert_eq!(read_key_with(&root, false, Some(Plain), &prints), Ok(true));
        let plain = sidecar_of(&root);
        let written = std::fs::read_to_string(root.join("sources/text/k.txt")).unwrap();
        assert!(written.contains("Synthetic Local Paper"), "{written}");
        assert_eq!(plain.extraction(), Ok(Some(Plain)));
        assert_eq!(plain.pdf_sha256(), text.pdf_sha256());
        assert_eq!(plain.retrieved, text.retrieved);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Without a recorded PDF sha256, or when the saved PDF does not have it,
    /// a mode change downloads the PDF again.
    #[test]
    fn read_downloads_for_a_mode_change_without_a_matching_pdf_sha256() {
        use crate::http::response;
        use registry::Extraction::{Layout, Plain};
        let pdf = tiny_pdf("Synthetic Served Paper");
        let reply = || response("200 OK", &["Content-Type: application/pdf"], &pdf);
        let (base, server) = http::serve(vec![reply(), reply()]);
        let (root, mut text) = unmarked_project("pdf-mismatch", &format!("{base}/k.pdf"));
        let pdf_path = root.join("sources/text/k.pdf");
        let sidecar = root.join("sources/text/k.json");
        let prints = pdf::tests::fake_program("prints");

        // A recorded sha256 that the saved PDF does not have.
        std::fs::write(&pdf_path, tiny_pdf("Another Paper")).unwrap();
        text.set_pdf_sha256(http::sha256_hex(pdf.as_bytes()));
        std::fs::write(&sidecar, serde_json::to_string(&text).unwrap()).unwrap();
        assert_eq!(read_key_with(&root, false, Some(Layout), &prints), Ok(true));
        let downloaded = sidecar_of(&root);
        assert_eq!(downloaded.extraction(), Ok(Some(Layout)));
        assert_ne!(downloaded.retrieved, text.retrieved, "downloaded again");
        assert_eq!(std::fs::read(&pdf_path).unwrap(), pdf.as_bytes());

        // No recorded sha256, beside the PDF as the download left it.
        let mut unmarked = downloaded.clone();
        unmarked.extra.remove("pdf_sha256");
        unmarked.retrieved = text.retrieved.clone();
        std::fs::write(&sidecar, serde_json::to_string(&unmarked).unwrap()).unwrap();
        assert_eq!(read_key_with(&root, false, Some(Plain), &prints), Ok(true));
        let heads = server.join().unwrap();
        assert_eq!(heads.len(), 2, "{heads:?}");
        let plain = sidecar_of(&root);
        assert_eq!(plain.extraction(), Ok(Some(Plain)));
        assert_ne!(plain.retrieved, text.retrieved, "downloaded again");
        let pdf_sha256 = http::sha256_hex(pdf.as_bytes());
        assert_eq!(plain.pdf_sha256(), Some(pdf_sha256.as_str()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Without pdftotext, layout mode fails before any request, whether it is
    /// asked for or recorded, and before extracting from a saved PDF. Plain
    /// mode does not need pdftotext.
    #[test]
    fn read_in_layout_mode_needs_pdftotext_before_any_request() {
        use crate::http::response;
        use registry::Extraction::{Layout, Plain};
        let absent = pdf::tests::fake_program("absent");
        let pdf = tiny_pdf("Synthetic Precheck Paper");
        let reply = || response("200 OK", &["Content-Type: application/pdf"], &pdf);
        let (base, server) = http::serve_within(vec![reply()], std::time::Duration::from_secs(1));
        let (root, mut text) = unmarked_project("no-pdftotext", &format!("{base}/k.pdf"));
        let sidecar = root.join("sources/text/k.json");
        let refused = |err: String| {
            assert!(err.contains("pdftotext"), "{err}");
            assert!(err.contains("Poppler"), "{err}");
            assert!(err.contains("absent is not on PATH"), "{err}");
        };

        // Asked for: a download is needed, as the text is plain.
        refused(read_key_with(&root, false, Some(Layout), &absent).unwrap_err());
        // Recorded: --force downloads again in the recorded mode.
        text.set_extraction(Layout);
        std::fs::write(&sidecar, serde_json::to_string(&text).unwrap()).unwrap();
        refused(read_key_with(&root, true, None, &absent).unwrap_err());
        let panic = server.join().unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("got 0 of 1 requests"), "{message}");

        // From the saved PDF, which has the recorded sha256.
        std::fs::write(root.join("sources/text/k.pdf"), &pdf).unwrap();
        text.set_extraction(Plain);
        text.set_pdf_sha256(http::sha256_hex(pdf.as_bytes()));
        std::fs::write(&sidecar, serde_json::to_string(&text).unwrap()).unwrap();
        refused(read_key_with(&root, false, Some(Layout), &absent).unwrap_err());
        assert_eq!(sidecar_of(&root), text);

        // Plain mode runs without it, from the same PDF.
        text.set_extraction(Layout);
        std::fs::write(&sidecar, serde_json::to_string(&text).unwrap()).unwrap();
        assert_eq!(read_key_with(&root, false, Some(Plain), &absent), Ok(true));
        let plain = sidecar_of(&root);
        assert_eq!(plain.extraction(), Ok(Some(Plain)));
        let written = std::fs::read_to_string(root.join("sources/text/k.txt")).unwrap();
        assert!(written.contains("Synthetic Precheck Paper"), "{written}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Writes `text` as `sources/text/<name>.txt` in `root`, with its manifest
    /// as `read` left it before it recorded `pdf_sha256`, and returns that.
    fn old_text(
        root: &Path,
        name: &str,
        text: &str,
        mode: Option<registry::Extraction>,
    ) -> registry::Text {
        let dir = root.join("sources/text");
        std::fs::write(dir.join(format!("{name}.txt")), text).unwrap();
        let mut manifest = registry::Text {
            path: format!("sources/text/{name}.txt"),
            sha256: http::sha256_hex(text.as_bytes()),
            origin: Some(format!("https://example.invalid/{name}.pdf")),
            retrieved: Some("2026-11-07T10:00:00Z".into()),
            extra: Default::default(),
        };
        if let Some(mode) = mode {
            manifest.set_extraction(mode);
        }
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        manifest
    }

    /// `record-pdfs` records the sha256 of a PDF that reproduces its text, in
    /// the manifest and the registry, and leaves the other texts alone. Then a
    /// mode change extracts from the saved PDF. Neither makes a request: the
    /// entry's URL is a server that fails when it gets none.
    #[test]
    fn record_pdfs_records_only_a_pdf_that_reproduces_its_text() {
        use crate::http::response;
        let absent = pdf::tests::fake_program("absent");
        let pdf = tiny_pdf("Synthetic Recorded Paper");
        let reply = response("200 OK", &["Content-Type: application/pdf"], &pdf);
        let (base, server) = http::serve_within(vec![reply], std::time::Duration::from_secs(1));
        let (root, _) = unmarked_project("record-pdfs", &format!("{base}/k.pdf"));
        let dir = root.join("sources/text");
        let config = config::Config::load(None, &root).unwrap();
        let registry_path = root.join("refs.jsonl");

        // `k`: the text pdf-extract gives for its PDF, recorded in the entry.
        std::fs::write(dir.join("k.pdf"), &pdf).unwrap();
        let plain = pdf::extract_text(pdf.as_bytes()).unwrap();
        let text = old_text(&root, "k", &plain, None);
        registry::update(&registry_path, |records| {
            records[0].texts.push(text.clone());
            Ok(())
        })
        .unwrap();
        // `other`: a PDF of another paper beside the text, and no entry.
        std::fs::write(dir.join("other.pdf"), tiny_pdf("Another Paper")).unwrap();
        old_text(&root, "other", "text of some paper\n", None);
        // A text without a PDF, and a JSON file that is no manifest.
        old_text(&root, "bare", "text without a PDF\n", None);
        std::fs::write(dir.join("notes.json"), "{\"a\":1}").unwrap();
        let files = || {
            ["k.json", "other.json", "bare.json", "notes.json"]
                .map(|name| std::fs::read_to_string(dir.join(name)).unwrap())
        };
        let (before, registry_before) = (files(), std::fs::read(&registry_path).unwrap());
        let expected = PdfRecords {
            recorded: vec!["k".into()],
            not_reproduced: vec!["other".into()],
            already: 0,
            no_pdf: 1,
        };

        // A dry run finds the same and writes nothing.
        assert_eq!(record_pdfs(&config, &dir, true, &absent), Ok(expected));
        assert_eq!(files(), before);
        assert_eq!(std::fs::read(&registry_path).unwrap(), registry_before);

        let found = record_pdfs(&config, &dir, false, &absent).unwrap();
        assert_eq!(found.recorded, ["k"]);
        assert_eq!(found.not_reproduced, ["other"]);
        let pdf_sha256 = http::sha256_hex(pdf.as_bytes());
        let mut recorded = text.clone();
        recorded.set_pdf_sha256(pdf_sha256.clone());
        assert_eq!(sidecar_of(&root), recorded);
        let entry = registry::load(&registry_path).unwrap().unwrap().remove(0);
        assert_eq!(entry.texts.len(), 2);
        // The entry's other text, at another path, is not touched.
        assert_eq!(entry.texts[0].pdf_sha256(), None);
        assert_eq!(entry.texts[1], recorded);
        assert_eq!(files()[1..], before[1..]);

        // A second run has nothing left to record.
        let again = PdfRecords {
            recorded: vec![],
            not_reproduced: vec!["other".into()],
            already: 1,
            no_pdf: 1,
        };
        assert_eq!(record_pdfs(&config, &dir, false, &absent), Ok(again));

        // The mode change now extracts from the saved PDF.
        let prints = pdf::tests::fake_program("prints");
        let layout = Some(registry::Extraction::Layout);
        assert_eq!(read_key_with(&root, false, layout, &prints), Ok(true));
        let written = std::fs::read_to_string(dir.join("k.txt")).unwrap();
        assert!(
            written.starts_with("text of UTF-8 with -layout -enc "),
            "{written}"
        );
        let switched = sidecar_of(&root);
        assert_eq!(switched.extraction(), Ok(layout));
        assert_eq!(switched.pdf_sha256(), Some(pdf_sha256.as_str()));
        assert_eq!(switched.retrieved, text.retrieved, "not downloaded");

        let panic = server.join().unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("got 0 of 1 requests"), "{message}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A plain text that pdf-extract does not reproduce is tried with
    /// pdftotext, and a layout text with `pdftotext -layout` only. Without
    /// pdftotext, or in a mode this version does not know, nothing is
    /// recorded. Without a registry, only the manifests are written.
    #[test]
    fn record_pdfs_tries_pdftotext_in_the_recorded_mode() {
        use registry::Extraction::Layout;
        let root =
            std::env::temp_dir().join(format!("papers-record-pdfs-modes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sources/text")).unwrap();
        std::fs::write(root.join("research.toml"), "").unwrap();
        let config = config::Config::load(None, &root).unwrap();
        let dir = config.root.join("sources/text");
        let pdf = tiny_pdf("Synthetic Fallback Paper");
        // What the fake pdftotext prints for a PDF: "text of $3 with $1 $2 $4".
        let fake = |name: &str, layout: bool| {
            let path = dir.join(format!("{name}.pdf"));
            std::fs::write(&path, &pdf).unwrap();
            match layout {
                true => format!("text of UTF-8 with -layout -enc {}\n", path.display()),
                false => format!("text of {} with -enc UTF-8 -\n", path.display()),
            }
        };
        let texts = [
            old_text(&root, "a", &fake("a", false), None),
            old_text(&root, "b", &fake("b", true), Some(Layout)),
            // A plain text recorded as layout, and a layout text as plain.
            old_text(&root, "c", &fake("c", false), Some(Layout)),
            old_text(&root, "d", &fake("d", true), None),
        ];
        let mut unknown = old_text(&root, "e", &fake("e", false), None);
        unknown.extra.insert("extraction".into(), "ocr".into());
        std::fs::write(dir.join("e.json"), serde_json::to_string(&unknown).unwrap()).unwrap();
        let manifest = |name: &str| -> registry::Text {
            let json = std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
            serde_json::from_str(&json).unwrap()
        };

        let absent = pdf::tests::fake_program("absent");
        let found = record_pdfs(&config, &dir, false, &absent).unwrap();
        assert_eq!(found.recorded, [""; 0]);
        assert_eq!(found.not_reproduced, ["a", "b", "c", "d", "e"]);
        assert_eq!(manifest("a"), texts[0]);

        let prints = pdf::tests::fake_program("prints");
        let found = record_pdfs(&config, &dir, false, &prints).unwrap();
        assert_eq!(found.recorded, ["a", "b"]);
        assert_eq!(found.not_reproduced, ["c", "d", "e"]);
        let pdf_sha256 = http::sha256_hex(pdf.as_bytes());
        for (name, text) in ["a", "b"].into_iter().zip(&texts) {
            let mut recorded = text.clone();
            recorded.set_pdf_sha256(pdf_sha256.clone());
            assert_eq!(manifest(name), recorded);
        }
        assert_eq!(manifest("c"), texts[2]);
        assert_eq!(manifest("d"), texts[3]);
        assert_eq!(manifest("e"), unknown);
        assert!(!config.registry.exists(), "no registry was created");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Layout mode runs `pdftotext -layout` and records the mode; without
    /// pdftotext it writes no text, and never falls back to pdf-extract.
    #[test]
    fn save_pdf_extracts_layout_text_with_pdftotext_only() {
        let dir = std::env::temp_dir().join(format!("papers-save-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = config::Config::load(None, &dir).unwrap();
        let prints = pdf::tests::fake_program("prints");
        let target = |program| Target {
            config: &config,
            dir: dir.clone(),
            name: "lay".into(),
            title: String::new(),
            extraction: registry::Extraction::Layout,
            pdftotext: program,
            docling: &[],
        };
        let pdf = tiny_pdf("Synthetic Layout Paper");
        let saved = save_pdf(
            &target(&prints),
            pdf.as_bytes(),
            "https://example.org/lay.pdf",
        );
        let Ok(Saved::Text(text)) = saved else {
            panic!("expected a text");
        };
        assert_eq!(text.extraction(), Ok(Some(registry::Extraction::Layout)));
        assert!(!dir.join("lay.txt").exists(), "staged until it is recorded");
        let written = std::fs::read_to_string(derivative::pending(&dir.join("lay.txt"))).unwrap();
        // The fake echoes its arguments: "text of $3 with $1 $2 $4".
        assert!(
            written.starts_with("text of UTF-8 with -layout -enc "),
            "{written}"
        );
        assert_eq!(text.sha256, http::sha256_hex(written.as_bytes()));

        derivative::discard(&dir.join("lay.txt"));
        let absent = pdf::tests::fake_program("absent");
        let saved = save_pdf(
            &target(&absent),
            pdf.as_bytes(),
            "https://example.org/lay.pdf",
        );
        assert!(matches!(saved, Ok(Saved::PdfOnly)));
        assert!(!dir.join("lay.txt").exists(), "no pdf-extract fallback");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A plain text is staged, then recorded, then moved into place. When
    /// `read` stops before its record is written, no text is left without one.
    #[test]
    fn a_read_text_appears_only_after_its_record() {
        let dir = std::env::temp_dir().join(format!("papers-text-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = config::Config::load(None, &dir).unwrap();
        let target = Target {
            config: &config,
            dir: dir.join("texts"),
            name: "5".into(),
            title: String::new(),
            extraction: registry::Extraction::Plain,
            pdftotext: "papers-no-such-pdftotext",
            docling: &[],
        };
        let pdf = tiny_pdf("Synthetic Order Paper");
        let Ok(Saved::Text(text)) = save_pdf(&target, pdf.as_bytes(), "https://example.org/5.pdf")
        else {
            panic!("expected a text");
        };
        let txt = dir.join("texts/5.txt");
        let sidecar = dir.join("texts/5.json");
        assert!(!txt.exists(), "the text waits for its record");
        assert!(derivative::pending(&txt).exists());
        let entry = registry::Reference::new("5".into());
        let options = ReadOptions {
            reference: None,
            key: Some("5".into()),
            out: None,
            record: true,
            force: false,
            extraction: None,
            browser: false,
        };
        // The registry cannot be written: read stops between the writes.
        std::fs::write(&config.registry, "not json\n").unwrap();
        let stopped = finish_text(
            &config,
            &options,
            Some(&entry),
            &sidecar,
            &txt,
            text.clone(),
            true,
        );
        assert!(stopped.is_err());
        assert!(!txt.exists(), "no text without a record");
        assert!(
            !derivative::pending(&txt).exists(),
            "the staged text is removed"
        );
        assert!(!sidecar.exists());

        let Ok(Saved::Text(text)) = save_pdf(&target, pdf.as_bytes(), "https://example.org/5.pdf")
        else {
            panic!("expected a text");
        };
        std::fs::write(
            &config.registry,
            registry::render(std::slice::from_ref(&entry)),
        )
        .unwrap();
        assert_eq!(
            finish_text(
                &config,
                &options,
                Some(&entry),
                &sidecar,
                &txt,
                text.clone(),
                true
            ),
            Ok(true)
        );
        assert_eq!(
            http::sha256_hex(&std::fs::read(&txt).unwrap()),
            text.sha256,
            "moved into place"
        );
        let recorded = registry::load(&config.registry).unwrap().unwrap();
        assert_eq!(recorded[0].texts[0].path, text.path);
        assert!(sidecar.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_a_landing_page_through_its_pdf_link() {
        use crate::http::response;
        let html = ["Content-Type: text/html"];
        let pdf = tiny_pdf("Synthetic Bulletin");
        // The server's address is only known once it runs, so the meta tag
        // links the PDF relative to the page.
        let page = r#"<html><head><meta name="citation_doi" content="10.5555/synthetic.1">
            <meta name="citation_pdf_url" content="/bulletin/b90.pdf"></head></html>"#;
        let (base, server) = http::serve(vec![
            response("200 OK", &html, page),
            response("200 OK", &["Content-Type: application/pdf"], &pdf),
        ]);
        let dir = std::env::temp_dir().join(format!("papers-read-landing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let url = format!("{base}/publications/bulletin90");
        assert_eq!(read_url(url.clone(), &dir), Ok(true));
        let heads = server.join().unwrap();
        assert!(heads[1].starts_with("GET /bulletin/b90.pdf "), "{heads:?}");
        assert_eq!(
            manifest(&dir, &url).origin,
            Some(format!("{base}/bulletin/b90.pdf"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A landing page without a PDF link that names a DOI: `read` falls back
    /// to the DOI's open-access locations. Crossref and OpenAlex answer from a
    /// seeded response cache.
    #[test]
    fn reads_a_landing_page_through_the_doi_it_names() {
        use crate::http::{response, seed_cache};
        let dir = std::env::temp_dir().join(format!("papers-read-page-doi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let page = r#"<html><head><meta name="citation_doi" content="10.5555/synthetic.2">
            </head></html>"#;
        let pdf = tiny_pdf("Synthetic Landing Paper");
        let (base, server) = http::serve(vec![
            response("200 OK", &["Content-Type: text/html"], page),
            response("200 OK", &["Content-Type: application/pdf"], &pdf),
        ]);
        let cache = dir.join("cache");
        seed_cache(
            &cache,
            "https://api.crossref.org/works/10.5555/synthetic.2",
            404,
            "",
        );
        let work = serde_json::json!({
            "id": "https://openalex.org/W9000000002",
            "doi": "https://doi.org/10.5555/synthetic.2",
            "title": "Synthetic Landing Paper",
            "display_name": "Synthetic Landing Paper",
            "publication_year": 2026,
            "type": "article",
            "authorships": [],
            "locations": [{
                "is_oa": true,
                "landing_page_url": format!("{base}/repo/2"),
                "pdf_url": format!("{base}/repo/2/paper.pdf"),
            }],
        });
        seed_cache(
            &cache,
            "https://api.openalex.org/works/doi:10.5555/synthetic.2",
            200,
            &work.to_string(),
        );
        let http = Http::new().with_cache(http::Cache {
            dir: cache,
            refresh: false,
        });
        let config = config::Config::load(None, &dir).unwrap();
        let url = format!("{base}/item/2");
        let read_ok = read(
            &http,
            &config,
            &ReadOptions {
                reference: Some(format!("  {url}\n")),
                key: None,
                out: Some(dir.clone()),
                record: false,
                force: false,
                extraction: None,
                browser: false,
            },
            &pdf::tests::fake_program("absent"),
        );
        let heads = server.join().unwrap();
        assert_eq!(read_ok, Ok(true), "{heads:?}");
        assert!(heads[0].starts_with("GET /item/2 "), "{heads:?}");
        assert!(heads[1].starts_with("GET /repo/2/paper.pdf "), "{heads:?}");
        let text = manifest(&dir, &url);
        assert_eq!(text.origin, Some(format!("{base}/repo/2/paper.pdf")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A URL's own copy comes first: when its PDF link answers with a
    /// JavaScript check page, the browser retry runs before the DOI that the
    /// landing page names is looked up.
    #[test]
    fn retries_a_urls_challenge_page_before_the_doi_it_names() {
        use crate::http::{response, seed_cache};
        use std::io::{Read, Write};
        let dir = std::env::temp_dir().join(format!("papers-read-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let page = r#"<html><head><meta name="citation_doi" content="10.5555/synthetic.3">
            <meta name="citation_pdf_url" content="/files/paper.pdf"></head></html>"#;
        let html = ["Content-Type: text/html"];
        let (base, server) = http::serve(vec![
            response("200 OK", &html, page),
            response(
                "202 Accepted",
                &html,
                "<html><script src=x></script></html>",
            ),
            // Requested only if the DOI's copy is tried before the browser.
            response(
                "200 OK",
                &["Content-Type: application/pdf"],
                &tiny_pdf("Synthetic DOI Copy"),
            ),
        ]);
        let cache = dir.join("cache");
        seed_cache(
            &cache,
            "https://api.crossref.org/works/10.5555/synthetic.3",
            404,
            "",
        );
        let work = serde_json::json!({
            "id": "https://openalex.org/W9000000003",
            "doi": "https://doi.org/10.5555/synthetic.3",
            "title": "Synthetic Challenged Paper",
            "display_name": "Synthetic Challenged Paper",
            "publication_year": 2026,
            "type": "article",
            "authorships": [],
            "locations": [{
                "is_oa": true,
                "landing_page_url": format!("{base}/repo/3"),
                "pdf_url": format!("{base}/repo/3/paper.pdf"),
            }],
        });
        seed_cache(
            &cache,
            "https://api.openalex.org/works/doi:10.5555/synthetic.3",
            200,
            &work.to_string(),
        );
        let http = Http::new().with_cache(http::Cache {
            dir: cache,
            refresh: false,
        });
        let config = config::Config::load(None, &dir).unwrap();
        let absent = pdf::tests::fake_program("absent");
        let target = Target {
            config: &config,
            dir: dir.clone(),
            name: "order".into(),
            title: String::new(),
            extraction: registry::Extraction::Plain,
            pdftotext: &absent,
            docling: &[],
        };
        let mut retried = Vec::new();
        let mut browse = |pages: &[String], _: &Target| {
            retried.extend_from_slice(pages);
            Ok(Some(Saved::PdfOnly))
        };
        let url = format!("{base}/item/3");
        let saved = download(&http, &url, None, target, &mut browse);
        // The server answers a fixed number of connections. Take its last
        // canned response if nothing did, so it finishes either way; the read
        // timeout covers a server that already has.
        if let Ok(mut s) = std::net::TcpStream::connect(base.trim_start_matches("http://")) {
            s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let _ = s.write_all(b"GET /done HTTP/1.1\r\n\r\n");
            let _ = s.read_to_end(&mut Vec::new());
        }
        let heads = server.join().unwrap();
        assert!(heads[1].starts_with("GET /files/paper.pdf "), "{heads:?}");
        assert!(heads[2].starts_with("GET /done "), "{heads:?}");
        assert!(matches!(saved, Ok(Some(Saved::PdfOnly))), "{heads:?}");
        assert_eq!(retried, [format!("{base}/files/paper.pdf")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn urls_without_a_paper_id_get_distinct_file_names() {
        let a = url_file_name("https://www.bis.org/publ/bisbull90.pdf").unwrap();
        assert!(a.starts_with("bis.org_bisbull90_"), "{a}");
        assert_eq!(a.len(), "bis.org_bisbull90_".len() + 8);
        // The same URL, written differently, keeps its name; another URL does not.
        assert_eq!(
            url_file_name("https://bis.org/publ/bisbull90.pdf#page=2").unwrap(),
            a
        );
        assert_ne!(
            url_file_name("https://www.bis.org/other/bisbull90.pdf").unwrap(),
            a
        );
        let landing = url_file_name("https://figshare.com/articles/x/25886773/").unwrap();
        assert!(landing.starts_with("figshare.com_25886773_"), "{landing}");
        let bare = url_file_name("https://example.org").unwrap();
        assert!(bare.starts_with("example.org__"), "{bare}");
        assert_eq!(url_file_name("10.1234/abc"), None);
    }

    #[test]
    fn registry_keys_become_safe_file_names() {
        assert_eq!(file_name_key("177"), "177");
        assert_eq!(file_name_key("2026-doe-widgets"), "2026-doe-widgets");
        assert_eq!(file_name_key("a/b c"), "a_b_c");
        assert_eq!(file_name_key(".."), "_..");
    }

    #[test]
    fn recorded_texts_are_reused_only_with_their_hash() {
        let root = std::env::temp_dir().join(format!("papers-read-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sources/text")).unwrap();
        std::fs::write(root.join("sources/text/7.txt"), "synthetic text").unwrap();
        std::fs::write(root.join("research.toml"), "").unwrap();
        let config = config::Config::load(None, &root).unwrap();
        let text = |sha: String| registry::Text {
            path: "sources/text/7.txt".into(),
            sha256: sha,
            origin: Some("https://example.org/7.pdf".into()),
            retrieved: None,
            extra: Default::default(),
        };
        let good = text(http::sha256_hex(b"synthetic text"));
        assert_eq!(
            existing_text(&config, std::slice::from_ref(&good)),
            Some(good)
        );
        assert_eq!(existing_text(&config, &[text("00".into())]), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_search_options() {
        let cli = Cli::try_parse_from([
            "papers",
            "search",
            "funding rate",
            "--max",
            "5",
            "--since",
            "2026-01-01",
            "--category",
            "q-fin.TR",
            "--category",
            "q-fin.MF",
            "--sort",
            "relevance",
            "--papers",
            "x",
        ])
        .unwrap();
        assert_eq!(cli.papers, Some(PathBuf::from("x")));
        let Command::Search {
            query,
            max,
            since,
            categories,
            sort,
            ..
        } = cli.command
        else {
            panic!("expected search");
        };
        assert_eq!(
            (query.as_str(), max, sort),
            ("funding rate", 5, Some(Sort::Relevance))
        );
        assert_eq!(since, Date::new(2026, 1, 1).ok());
        assert_eq!(categories, ["q-fin.TR", "q-fin.MF"]);
        assert!(Cli::try_parse_from(["papers", "search", "x", "--max", "201"]).is_err());
        assert!(Cli::try_parse_from(["papers", "search", "x", "--since", "2026-1-1"]).is_err());
        assert!(Cli::try_parse_from(["papers", "fetch"]).is_err());
        let cli = Cli::try_parse_from(["papers", "search", "x", "--source", "s2"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Search {
                source: sources::Source::S2,
                sort: None,
                ..
            }
        ));
    }

    #[test]
    fn parses_fetch_title_and_read() {
        let cli =
            Cli::try_parse_from(["papers", "fetch", "--title", "Crypto Carry", "--json"]).unwrap();
        let Command::Fetch {
            refs, title, json, ..
        } = cli.command
        else {
            panic!("expected fetch");
        };
        assert!(refs.is_empty() && json);
        assert_eq!(title.as_deref(), Some("Crypto Carry"));
        assert!(Cli::try_parse_from(["papers", "fetch", "10.1/x", "--title", "y"]).is_err());
        let cli = Cli::try_parse_from(["papers", "read", "2208.06046", "--out", "d"]).unwrap();
        assert!(
            matches!(cli.command, Command::Read { ref out, .. } if out.as_deref() == Some(Path::new("d")))
        );
        assert!(Cli::try_parse_from(["papers", "read"]).is_err());
        // cite never rewrites its inputs, so their citation keys survive.
        assert!(Cli::try_parse_from(["papers", "cite", "a.md", "--in-place"]).is_err());
        assert!(Cli::try_parse_from(["papers", "cite", "a.md", "--out", "d"]).is_ok());
        assert!(Cli::try_parse_from(["papers", "read", "x", "--key", "7"]).is_err());
        assert!(Cli::try_parse_from(["papers", "read", "--key", "7", "--record"]).is_ok());
        let cli = Cli::try_parse_from(["papers", "read", "x", "--extraction", "layout"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Read {
                extraction: Some(registry::Extraction::Layout),
                ..
            }
        ));
        assert!(Cli::try_parse_from(["papers", "read", "x", "--extraction", "raw"]).is_err());
    }

    #[test]
    fn oa_candidates_order_pdfs_first() {
        let loc = |pdf: Option<&str>, page: Option<&str>| openalex::Location {
            pdf_url: pdf.map(str::to_owned),
            landing_page_url: page.map(str::to_owned),
            version: None,
            source: None,
        };
        let got = oa_candidates(&[
            loc(None, Some("https://doi.org/10.5555/sap-2026-13")),
            loc(Some("https://x.org/a.pdf"), Some("https://x.org/a")),
            loc(Some("https://x.org/a.pdf"), None),
            // Malformed URLs from OpenAlex are dropped.
            loc(None, Some("https://orcid.org/0009-0006-7557-449X>")),
            loc(None, Some("not a url")),
        ]);
        assert_eq!(
            got,
            [
                Candidate::Pdf("https://x.org/a.pdf".into()),
                Candidate::Landing("https://doi.org/10.5555/sap-2026-13".into()),
            ]
        );
    }
}
