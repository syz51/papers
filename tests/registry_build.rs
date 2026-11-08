//! `papers registry build` writes the registry despite parse errors, prints
//! each error, and exits 1 only when an error-severity issue exists.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn project(name: &str, config: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("papers-build-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("notes")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        format!("[papers]\nregistry = \"refs.jsonl\"\n{config}"),
    )
    .unwrap();
    dir
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run papers")
}

/// Each registry entry's key and title, in registry order.
fn titles(dir: &Path) -> Vec<(String, Option<String>)> {
    std::fs::read_to_string(dir.join("refs.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                entry["key"].as_str().unwrap().to_owned(),
                entry["title"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

#[test]
fn an_error_issue_is_printed_and_exits_1_but_the_registry_is_written() {
    let dir = project("error", "notes = \"notes\"\n");
    std::fs::write(
        dir.join("notes/a.md"),
        "---\nid: a\ntitle: Good\nurl: https://example.org/a\n---\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("notes/b.md"),
        "---\nid: c\ntitle: Misnamed\nurl: https://example.org/b\n---\n",
    )
    .unwrap();

    let dry = papers(&dir, &["registry", "build", "--dry-run"]);
    assert_eq!(dry.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&dry.stderr);
    assert!(
        stderr.contains(
            "papers: error: notes/b.md:1: key-mismatch b: the file is named b.md but its id is \"c\""
        ),
        "{stderr}"
    );
    assert!(!dir.join("refs.jsonl").exists());

    let build = papers(&dir, &["registry", "build"]);
    assert_eq!(build.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&build.stderr),
        stderr.replace("papers: parsed ", "papers: wrote ")
    );
    assert_eq!(
        titles(&dir),
        [
            ("a".to_owned(), Some("Good".to_owned())),
            ("b".to_owned(), Some("Misnamed".to_owned())),
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn warnings_alone_exit_0() {
    let dir = project("warn", "catalogue = \"refs.md\"\n");
    std::fs::write(
        dir.join("refs.md"),
        "## Reference 1\n\nAnn Lee. **A title**. https://example.org/a\n",
    )
    .unwrap();
    let build = papers(&dir, &["registry", "build"]);
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(build.status.success(), "{stderr}");
    assert!(stderr.contains("entry-unnumbered 1"), "{stderr}");
    assert!(!stderr.contains("papers: error"), "{stderr}");
    assert_eq!(titles(&dir), [("1".to_owned(), Some("A title".to_owned()))]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The registry line for `key`.
fn entry(dir: &Path, key: &str) -> String {
    std::fs::read_to_string(dir.join("refs.jsonl"))
        .unwrap()
        .lines()
        .find(|line| line.contains(&format!("\"key\":\"{key}\"")))
        .unwrap()
        .to_owned()
}

#[test]
fn a_note_with_invalid_frontmatter_keeps_its_previous_entry() {
    let dir = project("frontmatter", "notes = \"notes\"\n");
    let note = |key: &str, title: &str| {
        std::fs::write(
            dir.join(format!("notes/{key}.md")),
            format!(
                "---\nid: {key}\ntitle: {title}\nauthors: [Ann Lee]\nyear: 2026\n\
                 url: https://example.org/{key}\nstatus: read\n---\n"
            ),
        )
        .unwrap()
    };
    note("a", "First");
    note("b", "Second");
    let first = papers(&dir, &["registry", "build"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let before = entry(&dir, "a");
    std::fs::write(
        dir.join("notes/a.md"),
        "---\nid: a\ntitle: [unclosed\n---\n",
    )
    .unwrap();
    note("b", "Second, revised");

    let build = papers(&dir, &["registry", "build"]);
    assert_eq!(build.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("papers: error: notes/a.md:1: frontmatter-invalid a: "),
        "{stderr}"
    );
    assert_eq!(entry(&dir, "a"), before);
    assert_eq!(
        titles(&dir),
        [
            ("a".to_owned(), Some("First".to_owned())),
            ("b".to_owned(), Some("Second, revised".to_owned())),
        ]
    );

    // check agrees: the kept entry is not reported as stale.
    let check = papers(&dir, &["registry", "check", "--format", "jsonl"]);
    let rules: Vec<String> = String::from_utf8_lossy(&check.stdout)
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["rule"].to_string())
        .collect();
    assert_eq!(rules, ["\"frontmatter-invalid\""]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_catalogue_section_without_an_entry_line_keeps_its_previous_entry() {
    let dir = project("entry-missing", "catalogue = \"refs.md\"\n");
    let second = "## Reference 2\n\n[2] Bo Chen. **Second**. https://example.org/b\n";
    std::fs::write(
        dir.join("refs.md"),
        format!("## Reference 1\n\n[1] Ann Lee. **First**. https://example.org/a\n\n{second}"),
    )
    .unwrap();
    let first = papers(&dir, &["registry", "build"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let before = entry(&dir, "1");
    std::fs::write(dir.join("refs.md"), format!("## Reference 1\n\n{second}")).unwrap();

    let build = papers(&dir, &["registry", "build"]);
    assert_eq!(build.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("papers: error: refs.md:1: entry-missing 1: Reference 1 has no entry line"),
        "{stderr}"
    );
    assert_eq!(entry(&dir, "1"), before);
    std::fs::remove_dir_all(&dir).unwrap();
}
