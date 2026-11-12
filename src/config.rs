//! Project configuration: the shared `research.toml` file.
//!
//! The file is found by walking up from the current directory (or given with
//! `--config`). Relative paths in it resolve against its directory. Only the
//! shared `[project]` table and this tool's `[papers]` table are read; other
//! tools' tables are ignored. Without a file, the defaults below apply relative
//! to the current directory, so archives laid out as `research/papers/` with a
//! `research/_templates/paper.md` template keep working with no configuration.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

pub const FILE_NAME: &str = "research.toml";

#[derive(Debug, Default, Deserialize)]
struct RawFile {
    #[serde(default)]
    project: RawProject,
    #[serde(default)]
    papers: RawPapers,
}

#[derive(Debug, Default, Deserialize)]
struct RawProject {
    state_dir: Option<PathBuf>,
}

/// The `[papers]` table. Unknown keys are rejected so a typo is not silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPapers {
    notes: Option<PathBuf>,
    template: Option<PathBuf>,
    registry: Option<PathBuf>,
    catalogue: Option<PathBuf>,
    search_log: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    /// Note `status` value → registry `depth`, e.g. `{ read = "full" }`; each
    /// entry replaces the default for that status.
    #[serde(default)]
    status_depth: BTreeMap<String, String>,
    /// Citation patterns for `papers cite`; each has a `key` group.
    #[serde(default)]
    cite_patterns: Vec<String>,
    /// Replacement for each citation in renumbered copies (`{n}`, `{key}`, `{citekey}`).
    cite_replace: Option<String>,
    /// The command that runs Python with Docling importable; `read
    /// --extraction docling` appends `-c <script> <args>`.
    docling: Option<Vec<String>>,
    /// The paper-card switch (DR043); off unless set.
    cards: Option<bool>,
    /// Where `papers card` writes `<key>.json`.
    card_dir: Option<PathBuf>,
    /// The command that runs Claude Code in print mode for `papers card`.
    claude: Option<Vec<String>>,
    /// The model `papers card` asks for.
    card_model: Option<String>,
}

/// Resolved settings; every path is absolute or relative to the current directory.
#[derive(Debug, Clone)]
pub struct Config {
    /// The `research.toml` in use, if any.
    pub file: Option<PathBuf>,
    /// The project root: the config file's directory, else the current directory.
    pub root: PathBuf,
    pub notes: PathBuf,
    pub template: PathBuf,
    pub registry: PathBuf,
    pub catalogue: Option<PathBuf>,
    pub search_log: PathBuf,
    pub cache_dir: PathBuf,
    /// Note `status` value → registry `depth` when building from notes:
    /// [`DEFAULT_STATUS_DEPTH`] with `[papers] status_depth` on top.
    pub status_depth: BTreeMap<String, String>,
    /// True when `research.toml` sets `[papers] template`.
    pub template_configured: bool,
    /// `[papers] cite_patterns`; empty means the built-in patterns.
    pub cite_patterns: Vec<String>,
    pub cite_replace: Option<String>,
    /// `[papers] docling`, else [`DEFAULT_DOCLING`].
    pub docling: Vec<String>,
    /// `[papers] cards`: whether `papers card` may run (default off).
    pub cards: bool,
    /// `[papers] card_dir`, else [`DEFAULT_CARD_DIR`].
    pub card_dir: PathBuf,
    /// `[papers] claude`, else [`DEFAULT_CLAUDE`].
    pub claude: Vec<String>,
    /// `[papers] card_model`, else [`DEFAULT_CARD_MODEL`].
    pub card_model: String,
    /// `<state_dir>/papers`: the replies and the empty working directory of
    /// `papers card`.
    pub state: PathBuf,
}

/// Default notes directory (relative to the project root).
pub const DEFAULT_NOTES: &str = "research/papers";
/// Default template, relative to the notes directory.
pub const DEFAULT_TEMPLATE: &str = "../_templates/paper.md";
pub const DEFAULT_REGISTRY: &str = "references.jsonl";
pub const DEFAULT_STATE_DIR: &str = ".research";
/// The command that runs Python with Docling: the version the extractor
/// comparison of November 2026 measured, in a uv-managed environment.
pub const DEFAULT_DOCLING: [&str; 8] = [
    "uv",
    "run",
    "--quiet",
    "--no-project",
    "--with",
    "docling==2.150.0",
    "python",
    "-I",
];
/// Where paper cards go, relative to the project root.
pub const DEFAULT_CARD_DIR: &str = "sources/cards";
/// The command that runs Claude Code; `papers card` appends `-p` and its options.
pub const DEFAULT_CLAUDE: &str = "claude";
/// The card model: Haiku 5.5, measured on the twenty pilot papers
/// (research-factory-213). The pilot of DR043 used a Sonnet model.
pub const DEFAULT_CARD_MODEL: &str = "claude-haiku-5-5";
/// Note status → registry depth unless `[papers] status_depth` says otherwise.
/// `read` is `method`, not `full`: the status does not prove a full-text read.
pub const DEFAULT_STATUS_DEPTH: [(&str, &str); 2] = [("read", "method"), ("skimmed", "abstract")];

impl Config {
    /// Loads `explicit`, else the first `research.toml` found walking up from
    /// `cwd`, else the defaults rooted at `cwd`.
    pub fn load(explicit: Option<&Path>, cwd: &Path) -> Result<Config, String> {
        let file = match explicit {
            Some(path) => {
                let path = normalize(&cwd.join(path));
                if !path.is_file() {
                    return Err(format!("{}: no such config file", path.display()));
                }
                Some(path)
            }
            None => discover(cwd),
        };
        let Some(file) = file else {
            return Ok(Config::resolve(None, cwd.to_owned(), RawFile::default()));
        };
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let raw: RawFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        let root = file
            .parent()
            .map(Path::to_owned)
            .unwrap_or_else(|| cwd.to_owned());
        Ok(Config::resolve(Some(file), root, raw))
    }

    fn resolve(file: Option<PathBuf>, root: PathBuf, raw: RawFile) -> Config {
        let at = |p: &Path| normalize(&root.join(p));
        let p = raw.papers;
        let notes = at(p.notes.as_deref().unwrap_or(Path::new(DEFAULT_NOTES)));
        let template = match &p.template {
            Some(t) => at(t),
            None => normalize(&notes.join(DEFAULT_TEMPLATE)),
        };
        let state_dir = at(raw
            .project
            .state_dir
            .as_deref()
            .unwrap_or(Path::new(DEFAULT_STATE_DIR)));
        Config {
            registry: at(p.registry.as_deref().unwrap_or(Path::new(DEFAULT_REGISTRY))),
            catalogue: p.catalogue.as_deref().map(at),
            search_log: p
                .search_log
                .as_deref()
                .map(at)
                .unwrap_or_else(|| state_dir.join("papers/search.jsonl")),
            cache_dir: p
                .cache_dir
                .as_deref()
                .map(at)
                .unwrap_or_else(|| state_dir.join("papers/cache")),
            template_configured: p.template.is_some(),
            status_depth: DEFAULT_STATUS_DEPTH
                .iter()
                .map(|&(status, depth)| (status.to_owned(), depth.to_owned()))
                .chain(p.status_depth)
                .collect(),
            cite_patterns: p.cite_patterns,
            cite_replace: p.cite_replace,
            docling: p
                .docling
                .unwrap_or_else(|| DEFAULT_DOCLING.map(str::to_owned).to_vec()),
            cards: p.cards.unwrap_or(false),
            card_dir: at(p.card_dir.as_deref().unwrap_or(Path::new(DEFAULT_CARD_DIR))),
            claude: p.claude.unwrap_or_else(|| vec![DEFAULT_CLAUDE.to_owned()]),
            card_model: p
                .card_model
                .unwrap_or_else(|| DEFAULT_CARD_MODEL.to_owned()),
            state: state_dir.join("papers"),
            file,
            root,
            notes,
            template,
        }
    }

    /// Replaces the notes directory (the `--papers` flag); an unconfigured
    /// template follows it.
    pub fn set_notes(&mut self, notes: PathBuf) {
        if !self.template_configured {
            self.template = normalize(&notes.join(DEFAULT_TEMPLATE));
        }
        self.notes = notes;
    }

    /// `path` relative to the project root when inside it, else as given.
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

/// The first `research.toml` in `start` or one of its ancestors.
fn discover(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(FILE_NAME))
        .find(|candidate| candidate.is_file())
}

/// Removes `.` and resolvable `..` components without touching the file system.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let last_is_normal =
                    matches!(out.components().next_back(), Some(Component::Normal(_)));
                if last_is_normal {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("papers-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_without_a_file() {
        let cwd = Path::new("/work/archive");
        let c = Config::resolve(None, cwd.into(), RawFile::default());
        assert_eq!(c.notes, Path::new("/work/archive/research/papers"));
        assert_eq!(
            c.template,
            Path::new("/work/archive/research/_templates/paper.md")
        );
        assert_eq!(c.registry, Path::new("/work/archive/references.jsonl"));
        assert_eq!(
            c.search_log,
            Path::new("/work/archive/.research/papers/search.jsonl")
        );
        assert_eq!(
            c.cache_dir,
            Path::new("/work/archive/.research/papers/cache")
        );
        assert!(c.catalogue.is_none() && !c.template_configured);
    }

    #[test]
    fn discovers_upwards_and_resolves_against_the_file() {
        let dir = temp("discover");
        std::fs::write(
            dir.join(FILE_NAME),
            "[project]\nname = \"x\"\nstate_dir = \"state\"\n\n[papers]\nnotes = \"notes\"\n\
             catalogue = \"docs/references.md\"\nregistry = \"data/refs.jsonl\"\n\n\
             [claimcheck]\nanything = 1\n",
        )
        .unwrap();
        let sub = dir.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let c = Config::load(None, &sub).unwrap();
        assert_eq!(c.file.as_deref(), Some(dir.join(FILE_NAME).as_path()));
        assert_eq!(c.notes, dir.join("notes"));
        assert_eq!(c.template, dir.join("_templates/paper.md"));
        assert_eq!(c.catalogue, Some(dir.join("docs/references.md")));
        assert_eq!(c.registry, dir.join("data/refs.jsonl"));
        assert_eq!(c.search_log, dir.join("state/papers/search.jsonl"));
        assert_eq!(c.display(&dir.join("data/refs.jsonl")), "data/refs.jsonl");
        assert_eq!(c.display(Path::new("/elsewhere/x")), "/elsewhere/x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unknown_papers_keys_and_missing_explicit_files() {
        let dir = temp("unknown");
        let file = dir.join("custom.toml");
        std::fs::write(&file, "[papers]\nnotez = \"x\"\n").unwrap();
        let err = Config::load(Some(&file), &dir).unwrap_err();
        assert!(
            err.contains("custom.toml") && err.contains("notez"),
            "{err}"
        );
        assert!(Config::load(Some(Path::new("nope.toml")), &dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn normalizes_dot_segments() {
        assert_eq!(
            normalize(Path::new("/a/b/../_t/./p.md")),
            Path::new("/a/_t/p.md")
        );
        assert_eq!(normalize(Path::new("../x")), Path::new("../x"));
    }
}
