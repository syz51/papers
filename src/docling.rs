//! Docling, an external program like `pdftotext`: `papers` runs Python with
//! Docling importable (`[papers] docling`) on a small script, which converts a
//! saved PDF to Markdown with page marks. Docling is MIT; its models are
//! Apache-2.0 and CDLA-Permissive-2.0.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// The script `papers` hands to Python (`-c`).
const SCRIPT: &str = include_str!("docling.py");

/// The text file beside `<name>.txt`: `<name>.docling.md`.
pub const TEXT_SUFFIX: &str = ".docling.md";
/// Its manifest: `<name>.docling.json`.
pub const MANIFEST_SUFFIX: &str = ".docling.json";

/// A first run also installs Docling and downloads its models (0.5 GB).
pub const TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// The version check installs Docling on a first run too.
const VERSION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The script prints one short line.
const MAX_OUTPUT: u64 = 64 << 10;

/// The installed Docling version, such as `2.150.0`. An error names Docling
/// and the command, so a missing program is clear before any download.
pub fn version(command: &[String]) -> Result<String, String> {
    invoke(command, &["--version"], VERSION_TIMEOUT)
}

/// Converts `pdf` to Markdown in `out` and gives Docling's version.
pub fn convert(command: &[String], pdf: &Path, out: &Path) -> Result<String, String> {
    let (pdf, out) = (pdf.to_string_lossy(), out.to_string_lossy());
    invoke(command, &[&pdf, &out], TIMEOUT)
}

fn invoke(command: &[String], args: &[&str], timeout: Duration) -> Result<String, String> {
    let (program, prefix) = command
        .split_first()
        .ok_or("[papers] docling is empty; it names the command that runs Python with Docling")?;
    let mut cmd = Command::new(program);
    cmd.args(prefix).arg("-c").arg(SCRIPT).args(args);
    let shown = command.join(" ");
    let out = crate::pdf::run(program, cmd, timeout, MAX_OUTPUT)
        .map_err(|e| format!("Docling did not run through `{shown}`: {e}"))?;
    let out = String::from_utf8_lossy(&out);
    out.lines()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("docling")?
                .as_str()
                .map(str::to_owned)
        })
        .ok_or_else(|| format!("`{shown}` printed no Docling version line"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_command_and_a_missing_program_name_docling() {
        let e = version(&[]).unwrap_err();
        assert!(e.contains("Docling"), "{e}");
        let e = version(&["papers-no-such-program".to_owned()]).unwrap_err();
        assert!(e.contains("Docling") && e.contains("not on PATH"), "{e}");
    }
}
