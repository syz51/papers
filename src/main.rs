//! `papers`: paper lookups (arXiv, Crossref, OpenAlex), arXiv search,
//! open-access reading, note verification, and note stubs for a research
//! archive. Requests are throttled per host across processes (arXiv ≤1 per 3 s,
//! other hosts ≤1 per second).

mod arxiv;
mod atom;
mod bib;
mod browser;
mod catalogue;
mod check;
mod cite;
mod config;
mod crossref;
mod finding;
mod graph;
mod http;
mod id;
mod notes;
mod openalex;
mod pdf;
mod refs;
mod registry;
mod registry_cmd;
mod searchlog;
mod seen;
mod sources;
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
    /// Ignore cached API responses and fetch them again ([papers] cache_dir).
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
    /// stdout line is the text's manifest {path, sha256, origin, retrieved} as JSON.
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
    /// Build or check the reference registry (rf.reference/1 JSONL), or attach a text to an entry.
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
    /// Parse the catalogue or notes and write the registry, keeping tool-added state.
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
    /// tool) in an entry's `texts`, with its sha256. Replaces a text at the same path.
    Attach {
        /// Registry key of the entry.
        #[arg(long)]
        key: String,
        /// The text file; it must be inside the project root.
        #[arg(long, value_name = "PATH")]
        text: PathBuf,
        /// Where the text came from, such as the URL of the original.
        #[arg(long, value_name = "URL")]
        origin: Option<String>,
        /// Registry file (overrides [papers] registry).
        #[arg(long, value_name = "PATH")]
        registry: Option<PathBuf>,
    },
}

/// The registry entry for a text file inside the project: its root-relative path
/// (with `/` separators) and sha256. Symlinks are resolved before the root check,
/// so a link that leads out of the project is refused and the recorded path is
/// the file's real one.
fn local_text(
    config: &config::Config,
    cwd: &Path,
    path: &Path,
    origin: Option<String>,
) -> Result<registry::Text, String> {
    let given = cwd.join(path);
    let real = std::fs::canonicalize(&given).map_err(|e| format!("{}: {e}", given.display()))?;
    if !real.is_file() {
        return Err(format!("{} is not a file", given.display()));
    }
    let root = std::fs::canonicalize(&config.root)
        .map_err(|e| format!("{}: {e}", config.root.display()))?;
    let rel = real.strip_prefix(&root).map_err(|_| {
        format!(
            "{} is outside the project root {}; texts are recorded relative to it",
            real.display(),
            root.display()
        )
    })?;
    let rel = rel
        .iter()
        .map(|c| c.to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| format!("{}: the path is not valid UTF-8", real.display()))?
        .join("/");
    let bytes = std::fs::read(&real).map_err(|e| format!("{}: {e}", real.display()))?;
    if bytes.is_empty() {
        return Err(format!("{} is empty", given.display()));
    }
    if std::str::from_utf8(&bytes).is_err() {
        return Err(format!(
            "{} is not UTF-8 text; attach the extracted text, not the PDF",
            given.display()
        ));
    }
    Ok(registry::Text {
        path: rel,
        sha256: http::sha256_hex(&bytes),
        origin,
        retrieved: None,
        extra: Default::default(),
    })
}

/// Records `text` in entry `key`'s `texts`, replacing a text at the same path.
/// The replaced text's origin is kept when `text` has none, and when the file is
/// unchanged (same sha256), so are its download time and fields this version
/// does not know. Returns the text as recorded.
fn record_text(
    config: &config::Config,
    key: &str,
    mut text: registry::Text,
    missing: impl FnOnce() -> String,
) -> Result<registry::Text, String> {
    registry::update(&config.registry, |records| {
        let r = records
            .iter_mut()
            .find(|r| r.key == key)
            .ok_or_else(missing)?;
        if let Some(target) = &r.duplicate_of {
            println!("note: entry {key} is a duplicate of {target}");
        }
        if let Some(old) = r.texts.iter().find(|t| t.path == text.path) {
            if text.origin.is_none() {
                text.origin = old.origin.clone();
            }
            if old.sha256 == text.sha256 {
                if text.retrieved.is_none() {
                    text.retrieved = old.retrieved.clone();
                }
                for (k, v) in &old.extra {
                    text.extra.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        r.texts.retain(|t| t.path != text.path);
        r.texts.push(text.clone());
        Ok(text)
    })
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
                Some(title) => fetch_title(&http, &config, &title, out),
                None => fetch(&http, &config, &refs, out),
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
                browser: !no_browser,
            },
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
        Command::Registry { action } => registry(config, action),
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

fn registry(mut config: config::Config, action: RegistryAction) -> Result<bool, String> {
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
            registry,
        } => {
            set_registry(&mut config, registry);
            if !config.registry.is_file() {
                return Err(format!(
                    "attach needs a registry; none at {}",
                    config.display(&config.registry)
                ));
            }
            let text = local_text(&config, &cwd, &text, origin)?;
            let text = record_text(&config, &key, text, || {
                format!(
                    "{}: no entry with key {key}",
                    config.display(&config.registry)
                )
            })?;
            println!(
                "recorded in {} (key {key})",
                config.display(&config.registry)
            );
            println!("{}", serde_json::to_string(&text).expect("texts serialize"));
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

fn print_paper(paper: &Paper, note: Option<&str>, with_abstract: bool, json: bool) {
    if json {
        #[derive(Serialize)]
        struct Line<'a> {
            #[serde(flatten)]
            paper: &'a Paper,
            note: Option<String>,
        }
        let line = Line {
            paper,
            note: note.map(str::to_owned),
        };
        println!(
            "{}",
            serde_json::to_string(&line).expect("Paper serializes to JSON")
        );
        return;
    }
    let mut out = format!(
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
    println!("{out}\n");
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
        url: Some(p.abs_url.clone()),
    }
}

fn work_found(w: &openalex::Work) -> searchlog::Found {
    searchlog::Found {
        id: format!("openalex:{}", w.id),
        title: Some(w.title.clone()),
        year: w.publication_year,
        doi: w.doi.as_ref().map(|d| d.as_str().to_owned()),
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

fn fetch(
    http: &Http,
    config: &config::Config,
    inputs: &[String],
    out: Output,
) -> Result<bool, String> {
    let (with_abstract, json) = (out.with_abstract, out.json);
    let mut logged: Vec<(String, &'static str, Vec<searchlog::Found>)> = Vec::new();
    let parsed: Vec<Result<Ref, String>> = inputs.iter().map(|s| Ref::parse(s)).collect();
    let arxiv_ids: Vec<ArxivId> = parsed
        .iter()
        .filter_map(|r| match r {
            Ok(Ref::Arxiv(id)) => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut looked_up = if arxiv_ids.is_empty() {
        Vec::new()
    } else {
        Client::new(http).fetch(&arxiv_ids)
    }
    .into_iter();
    let index = seen::Index::load(config)?;
    let mut all_ok = true;
    for (input, reference) in inputs.iter().zip(parsed) {
        let result = match reference {
            Err(message) => Err(message),
            Ok(Ref::Arxiv(_)) => match looked_up.next().expect("one result per arXiv id") {
                Ok(paper) => {
                    print_paper(&paper, paper_note(&index, &paper), with_abstract, json);
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
) -> Result<bool, String> {
    let (with_abstract, json) = (out.with_abstract, out.json);
    let results = openalex::Client::new(http).search_title(title, TITLE_RESULTS)?;
    let index = seen::Index::load(config)?;
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
        print_paper(paper, paper_note(&index, paper), with_abstract, json);
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
        let seen: Vec<String> = {
            let mut keys: Vec<String> = index
                .lookup(&seen::Query {
                    doi: f.doi.clone(),
                    arxiv: f.arxiv.clone(),
                    url: f.url.clone(),
                    title: f.title.clone(),
                })
                .into_iter()
                .map(|h| h.key)
                .collect();
            keys.dedup();
            keys
        };
        print_hit(hit, &seen, out);
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
        let mut keys: Vec<String> = index
            .lookup(&seen::Query {
                doi: f.doi.clone(),
                arxiv: f.arxiv.clone(),
                url: f.url.clone(),
                title: f.title.clone(),
            })
            .into_iter()
            .map(|h| h.key)
            .collect();
        keys.dedup();
        in_archive += usize::from(!keys.is_empty());
        print_hit(hit, &keys, out);
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

fn print_hit(hit: &sources::Hit, seen: &[String], out: Output) {
    if out.json {
        #[derive(Serialize)]
        struct Line<'a> {
            #[serde(flatten)]
            hit: &'a sources::Hit,
            seen: &'a [String],
        }
        println!(
            "{}",
            serde_json::to_string(&Line { hit, seen }).expect("hits serialize to JSON")
        );
        return;
    }
    let mut head = hit.id.clone();
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
    if !seen.is_empty() {
        text += &format!("\nseen: {}", seen.join(", "));
    }
    if out.with_abstract
        && let Some(a) = &hit.abstract_text
    {
        text += &format!("\n{a}");
    }
    println!("{text}\n");
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
    browser: bool,
}

/// Where a downloaded PDF and its text go.
struct Target<'a> {
    config: &'a config::Config,
    dir: PathBuf,
    /// File name stem.
    name: String,
    title: String,
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

/// A recorded text whose file still has the recorded hash.
fn existing_text(config: &config::Config, recorded: &[registry::Text]) -> Option<registry::Text> {
    recorded.iter().find_map(|t| {
        let path = config.root.join(&t.path);
        match sha256_file(&path) {
            Some(sha) if sha == t.sha256 => Some(t.clone()),
            Some(_) => {
                println!(
                    "{} changed since its sha256 was recorded; downloading again",
                    t.path
                );
                None
            }
            None => None,
        }
    })
}

fn read(http: &Http, config: &config::Config, o: &ReadOptions) -> Result<bool, String> {
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
            if let Some(target) = &r.duplicate_of {
                println!("note: entry {key} is a duplicate of {target}");
            }
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
    let sidecar = out_dir.join(format!("{name}.json"));
    if !o.force {
        // The sidecar first: it keeps the download time, which a text
        // attached by hand to the entry does not have.
        let mut recorded: Vec<registry::Text> = Vec::new();
        if let Ok(json) = std::fs::read_to_string(&sidecar)
            && let Ok(t) = serde_json::from_str::<registry::Text>(&json)
        {
            recorded.push(t);
        }
        recorded.extend(entry.iter().flat_map(|r| r.texts.clone()));
        if let Some(t) = existing_text(config, &recorded) {
            println!(
                "already have {} (sha256 matches); not downloading (--force to fetch again)",
                t.path
            );
            return finish_read(config, o, entry.as_ref(), &sidecar, t);
        }
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
    };
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
        Some(Saved::Text(t)) => finish_read(config, o, entry.as_ref(), &sidecar, t),
        Some(Saved::PdfOnly) => Ok(false),
        None => {
            println!("no candidate yielded a PDF; open the landing page in a browser");
            Ok(false)
        }
    }
}

/// Writes the manifest sidecar, records the text when asked, and prints the manifest.
fn finish_read(
    config: &config::Config,
    o: &ReadOptions,
    entry: Option<&registry::Reference>,
    sidecar: &Path,
    text: registry::Text,
) -> Result<bool, String> {
    let line = serde_json::to_string(&text).expect("texts serialize");
    std::fs::write(sidecar, format!("{line}\n"))
        .map_err(|e| format!("{}: {e}", sidecar.display()))?;
    if o.record
        && let Some(entry) = entry
    {
        // Applied to the registry as it is now, not the copy loaded before the download.
        record_text(config, &entry.key, text, || {
            format!("entry {} left the registry during the download", entry.key)
        })?;
        println!(
            "recorded in {} (key {})",
            config.display(&config.registry),
            entry.key
        );
    }
    println!("{line}");
    Ok(true)
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
            print_paper(&paper, None, true, false);
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

/// Writes `<dir>/<name>.pdf` and its extracted text `<dir>/<name>.txt`; an
/// unchanged PDF is not rewritten.
fn save_pdf(target: &Target, body: &[u8], source: &str) -> Result<Saved, String> {
    let (dir, name) = (&target.dir, &target.name);
    debug_assert!(!name.contains(['/', '\\']) && !name.starts_with('.'));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let pdf_path = dir.join(format!("{name}.pdf"));
    let txt_path = dir.join(format!("{name}.txt"));
    println!("source: {source}");
    if sha256_file(&pdf_path).as_deref() == Some(http::sha256_hex(body).as_str()) {
        println!("pdf: {} (unchanged)", pdf_path.display());
    } else {
        std::fs::write(&pdf_path, body).map_err(|e| format!("{}: {e}", pdf_path.display()))?;
        println!("pdf: {}", pdf_path.display());
    }
    let text = match pdf::extract_text(body) {
        Ok(text) => text,
        Err(e) => {
            println!("text: not written ({e})");
            return Ok(Saved::PdfOnly);
        }
    };
    std::fs::write(&txt_path, &text).map_err(|e| format!("{}: {e}", txt_path.display()))?;
    println!(
        "text: {} ({} words)",
        txt_path.display(),
        text.split_whitespace().count()
    );
    if !text::key(&text).contains(&text::key(&target.title)) {
        println!(
            "warning: the title does not appear in the extracted text; check this is the right paper"
        );
    }
    Ok(Saved::Text(registry::Text {
        path: target.config.display(&txt_path),
        sha256: http::sha256_hex(text.as_bytes()),
        origin: Some(source.to_owned()),
        retrieved: Some(finding::now()),
        extra: Default::default(),
    }))
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
        let target = Target {
            config: &config,
            dir: dir.clone(),
            name: "x".into(),
            title: "X".into(),
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
        let config = config::Config::load(None, out).unwrap();
        read(
            &Http::new(),
            &config,
            &ReadOptions {
                reference: Some(url),
                key: None,
                out: Some(out.to_path_buf()),
                record: false,
                force: false,
                browser: false,
            },
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
                browser: false,
            },
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
        let target = Target {
            config: &config,
            dir: dir.clone(),
            name: "order".into(),
            title: String::new(),
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
