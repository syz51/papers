//! Files made from a paper's text or PDF: the Docling text, the structure
//! sidecar and the paper card. Each records the sha256 of the input it was
//! made from; when the input now has another sha256, the derivative is stale.
//! A derivative is written to a temporary file and renamed into place only
//! after its record is written, so a reader never finds one without it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::finding::{Finding, Severity};
use crate::registry::{Extraction, Reference};
use crate::{docling, http, structure};

/// The rule `registry check` reports a stale derivative under.
pub const STALE: &str = "derivative-stale";

/// Where a derivative waits until its record is written: `<file>.tmp`.
pub fn pending(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Writes `bytes` to [`pending`]`(path)`.
pub fn stage(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let tmp = pending(path);
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    Ok(tmp)
}

/// Moves a staged file to `path`; the staged file is removed when that fails.
pub fn commit(path: &Path) -> Result<(), String> {
    let tmp = pending(path);
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// Removes a staged file that will not be committed.
pub fn discard(path: &Path) {
    let _ = std::fs::remove_file(pending(path));
}

/// A derivative that holds its own record (a structure sidecar or a card):
/// written whole to a temporary file, then renamed.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    stage(path, bytes)?;
    commit(path)
}

fn sha256_of(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| http::sha256_hex(&b))
}

/// The input a derivative JSON file records, at `text.path` and
/// `text.sha256`.
pub fn recorded_input(path: &Path) -> Option<(String, String)> {
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let text = json.get("text")?;
    let field = |name: &str| text.get(name)?.as_str().map(str::to_owned);
    Some((field("path")?, field("sha256")?))
}

/// A stale derivative; `evidence` is the file and field that record `was`.
fn stale(
    key: &str,
    derivative: &Path,
    input: &Path,
    was: &str,
    now: &str,
    fix: String,
    evidence: (String, &str),
) -> Finding {
    Finding::new(
        STALE,
        Severity::Warn,
        key.to_owned(),
        format!(
            "{} was made from {} with sha256 {}, which now has {}; {fix}",
            derivative.display(),
            input.display(),
            &was[..was.len().min(12)],
            &now[..now.len().min(12)],
        ),
    )
    .evidence(&evidence.0, evidence.1.to_owned())
}

/// Stale derivatives of the registry's texts and the extra derivative files
/// `others` gives for an entry (such as its card), as `(file, its input)`.
/// Files that do not exist here, such as git-ignored texts in a worktree, are
/// skipped.
pub fn check(
    config: &Config,
    records: &[Reference],
    others: impl Fn(&Reference) -> Vec<(PathBuf, String)>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let shown = |p: &Path| PathBuf::from(config.display(p));
    for r in records {
        for t in &r.texts {
            if !seen.insert(&t.path) {
                continue;
            }
            let file = config.root.join(&t.path);
            let Some(now) = sha256_of(&file) else {
                continue;
            };
            let sidecar = structure::sidecar(&file);
            if let Some((_, was)) = recorded_input(&sidecar)
                && was != now
            {
                out.push(stale(
                    &r.key,
                    &shown(&sidecar),
                    &shown(&file),
                    &was,
                    &now,
                    format!("run papers structure {}", t.path),
                    (config.display(&sidecar), "text.sha256"),
                ));
            }
            // A Docling text is made from the PDF saved beside the plain text.
            if t.extraction() == Ok(Some(Extraction::Docling))
                && let Some(was) = t.pdf_sha256()
                && let Some(name) = t.path.strip_suffix(docling::TEXT_SUFFIX)
            {
                let pdf = config.root.join(format!("{name}.pdf"));
                if let Some(pdf_now) = sha256_of(&pdf)
                    && pdf_now != was
                {
                    out.push(stale(
                        &r.key,
                        &shown(&file),
                        &shown(&pdf),
                        was,
                        &pdf_now,
                        format!(
                            "run papers read --key {} --extraction docling --record",
                            r.key
                        ),
                        (config.display(&config.registry), "texts[].pdf_sha256"),
                    ));
                }
            }
        }
        for (derivative, fix) in others(r) {
            let Some((path, was)) = recorded_input(&derivative) else {
                continue;
            };
            let input = config.root.join(path);
            if let Some(now) = sha256_of(&input)
                && now != was
            {
                out.push(stale(
                    &r.key,
                    &shown(&derivative),
                    &shown(&input),
                    &was,
                    &now,
                    fix,
                    (config.display(&derivative), "text.sha256"),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_commit_leaves_neither_file() {
        let dir = std::env::temp_dir().join(format!("papers-derivative-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("taken")).unwrap();
        // Written and renamed: no temporary file is left.
        write(&dir.join("a.json"), b"{}\n").unwrap();
        assert_eq!(std::fs::read(dir.join("a.json")).unwrap(), b"{}\n");
        assert!(!pending(&dir.join("a.json")).exists());
        // The target is a non-empty directory, so the rename fails.
        std::fs::write(dir.join("taken/x"), b"").unwrap();
        let e = write(&dir.join("taken"), b"{}\n").unwrap_err();
        assert!(e.contains("taken"), "{e}");
        assert!(!pending(&dir.join("taken")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
