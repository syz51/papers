//! `papers registry build` records each note's reading depth.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn project(name: &str, config: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("papers-depth-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("notes")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        format!("[papers]\nnotes = \"notes\"\nregistry = \"refs.jsonl\"\n{config}"),
    )
    .unwrap();
    dir
}

/// Writes `notes/<key>.md` with the given extra frontmatter lines.
fn note(dir: &Path, key: &str, fields: &str) {
    std::fs::write(
        dir.join(format!("notes/{key}.md")),
        format!(
            "---\nid: {key}\ntitle: Title {key}\nauthors: [Ann Lee]\nyear: 2026\n\
             url: https://example.org/{key}\n{fields}---\n\nBody.\n"
        ),
    )
    .unwrap();
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run papers")
}

/// Each registry key with its depth, in registry order.
fn depths(dir: &Path) -> Vec<(String, Option<String>)> {
    std::fs::read_to_string(dir.join("refs.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                entry["key"].as_str().unwrap().to_owned(),
                entry["depth"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn pair(key: &str, depth: Option<&str>) -> (String, Option<String>) {
    (key.to_owned(), depth.map(str::to_owned))
}

fn assert_success(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn build_maps_status_to_depth_and_an_explicit_depth_wins() {
    let dir = project("map", "");
    note(&dir, "a-read", "status: read\n");
    note(&dir, "b-skimmed", "status: skimmed\n");
    note(&dir, "c-override", "status: skimmed\ndepth: full\n");
    note(&dir, "d-unknown", "status: queued\n");
    note(&dir, "e-none", "");
    assert_success(&papers(&dir, &["registry", "build"]));
    assert_eq!(
        depths(&dir),
        [
            pair("a-read", Some("method")),
            pair("b-skimmed", Some("abstract")),
            pair("c-override", Some("full")),
            pair("d-unknown", None),
            pair("e-none", None),
        ]
    );
    let check = papers(&dir, &["registry", "check", "--format", "jsonl"]);
    assert_success(&check);
    assert!(
        check.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unmapped_status_keeps_the_recorded_depth() {
    let dir = project("keep", "");
    note(&dir, "a", "status: read\n");
    note(&dir, "b", "status: read\n");
    assert_success(&papers(&dir, &["registry", "build"]));
    note(&dir, "a", "status: queued\n");
    note(&dir, "b", "status: skimmed\n");
    assert_success(&papers(&dir, &["registry", "build"]));
    assert_eq!(
        depths(&dir),
        [pair("a", Some("method")), pair("b", Some("abstract"))]
    );
    let check = papers(&dir, &["registry", "check", "--format", "jsonl"]);
    assert_success(&check);
    assert!(
        check.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn configured_status_depth_replaces_the_default_for_its_status() {
    let dir = project("config", "status_depth = { read = \"full\" }\n");
    note(&dir, "a", "status: read\n");
    note(&dir, "b", "status: skimmed\n");
    assert_success(&papers(&dir, &["registry", "build"]));
    assert_eq!(
        depths(&dir),
        [pair("a", Some("full")), pair("b", Some("abstract"))]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_invalid_explicit_depth_is_refused() {
    let dir = project("invalid", "");
    note(&dir, "a", "status: read\n");
    assert_success(&papers(&dir, &["registry", "build"]));
    let before = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    note(&dir, "a", "status: read\ndepth: deep\n");

    let build = papers(&dir, &["registry", "build"]);
    assert_eq!(build.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains(
            "notes/a.md: depth \"deep\" is not one of metadata, abstract, method, full, artifact, reproduced"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("nothing written"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(dir.join("refs.jsonl")).unwrap(),
        before
    );

    let check = papers(&dir, &["registry", "check", "--format", "jsonl"]);
    assert_eq!(check.status.code(), Some(1));
    let findings: Vec<serde_json::Value> = String::from_utf8_lossy(&check.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0]["rule"], "depth-invalid");
    assert_eq!(findings[0]["severity"], "error");
    assert_eq!(findings[0]["subject"], "a");
    std::fs::remove_dir_all(&dir).unwrap();
}
