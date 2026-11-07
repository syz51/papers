//! `papers registry attach` records a text already in the project.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ENTRY: &str = r#"{"schema":"rf.reference/1","key":"1","type":"paper","title":"A Synthetic Title","authors":["Ann Lee"],"year":2026,"venue":"arXiv preprint","ids":{"doi":null,"arxiv":"2601.00001","dblp":null,"openalex":null,"url":null},"version":"v1","accessed":null,"depth":null,"texts":[],"duplicate_of":null,"source":{"path":"refs.md","line":1},"verified":null}"#;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("papers-attach-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("texts")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        "[papers]\nregistry = \"refs.jsonl\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("refs.jsonl"), format!("{ENTRY}\n")).unwrap();
    std::fs::write(dir.join("texts/a.txt"), "first text").unwrap();
    dir
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run papers")
}

fn texts(dir: &Path) -> serde_json::Value {
    let line = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    let entry: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    entry["texts"].clone()
}

#[test]
fn attach_records_the_hash_and_replaces_a_text_at_the_same_path() {
    let dir = project("record");
    let attach = |origin: &str| {
        papers(
            &dir,
            &[
                "registry",
                "attach",
                "--key",
                "1",
                "--text",
                "texts/a.txt",
                "--origin",
                origin,
            ],
        )
    };
    let out = attach("https://example.org/a.pdf");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = texts(&dir);
    assert_eq!(t.as_array().unwrap().len(), 1);
    assert_eq!(t[0]["path"], "texts/a.txt");
    assert_eq!(t[0]["origin"], "https://example.org/a.pdf");
    let sha = t[0]["sha256"].as_str().unwrap().to_owned();
    // sha256 of "first text".
    assert_eq!(
        sha,
        "fa0343363edb8cb3ed1c098a47c28e56a7d7c8ade8dce7ed408b12b6280dd135"
    );

    std::fs::write(dir.join("texts/a.txt"), "second text").unwrap();
    assert!(attach("https://example.org/b.pdf").status.success());
    let t = texts(&dir);
    assert_eq!(t.as_array().unwrap().len(), 1, "same path is replaced: {t}");
    assert_ne!(t[0]["sha256"].as_str().unwrap(), sha);
    assert_eq!(t[0]["origin"], "https://example.org/b.pdf");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attach_refuses_texts_outside_the_project_and_unknown_keys() {
    let dir = project("refuse");
    let outside = dir
        .parent()
        .unwrap()
        .join(format!("papers-outside-{}.txt", std::process::id()));
    std::fs::write(&outside, "x").unwrap();
    let before = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();

    let out = papers(
        &dir,
        &[
            "registry",
            "attach",
            "--key",
            "1",
            "--text",
            outside.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("outside the project root"));

    let out = papers(
        &dir,
        &["registry", "attach", "--key", "9", "--text", "texts/a.txt"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no entry with key 9"));

    let out = papers(
        &dir,
        &[
            "registry",
            "attach",
            "--key",
            "1",
            "--text",
            "texts/none.txt",
        ],
    );
    assert_eq!(out.status.code(), Some(2));

    assert_eq!(
        std::fs::read_to_string(dir.join("refs.jsonl")).unwrap(),
        before
    );
    std::fs::remove_file(outside).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

fn attach(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["registry", "attach", "--key", "1"];
    args.extend_from_slice(extra);
    papers(dir, &args)
}

#[test]
fn attach_resolves_dot_dot_absolute_and_symlinked_paths() {
    let dir = project("paths");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let out = attach(
        &dir.join("sub"),
        &["--config", "../research.toml", "--text", "../texts/a.txt"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(texts(&dir)[0]["path"], "texts/a.txt");

    let link = dir
        .parent()
        .unwrap()
        .join(format!("papers-attach-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let abs = link.join("texts/a.txt");
    let out = attach(
        &link,
        &[
            "--config",
            link.join("research.toml").to_str().unwrap(),
            "--text",
            abs.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = texts(&dir);
    assert_eq!(t.as_array().unwrap().len(), 1, "same file, same path: {t}");
    assert_eq!(t[0]["path"], "texts/a.txt");
    std::fs::remove_file(link).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attach_keeps_the_origin_when_reattaching_without_one() {
    let dir = project("origin");
    assert!(
        attach(
            &dir,
            &[
                "--text",
                "texts/a.txt",
                "--origin",
                "https://example.org/a.pdf"
            ]
        )
        .status
        .success()
    );
    std::fs::write(dir.join("texts/a.txt"), "edited text").unwrap();
    assert!(attach(&dir, &["--text", "texts/a.txt"]).status.success());
    let t = texts(&dir);
    assert_eq!(t.as_array().unwrap().len(), 1);
    assert_eq!(t[0]["origin"], "https://example.org/a.pdf");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attach_refuses_links_out_binary_empty_directories_and_a_missing_registry() {
    let dir = project("refuse-more");
    let outside = dir
        .parent()
        .unwrap()
        .join(format!("papers-outside-link-{}.txt", std::process::id()));
    std::fs::write(&outside, "outside").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("texts/out.txt")).unwrap();
    std::fs::write(dir.join("texts/a.pdf"), b"%PDF-1.7\n\xff\xfe\x00binary").unwrap();
    std::fs::write(dir.join("texts/empty.txt"), "").unwrap();
    let before = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    for (path, message) in [
        ("texts/out.txt", "outside the project root"),
        ("texts/a.pdf", "not UTF-8 text"),
        ("texts/empty.txt", "is empty"),
        (".", "is not a file"),
    ] {
        let out = attach(&dir, &["--text", path]);
        assert_eq!(out.status.code(), Some(2), "{path}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(message), "{path}: {err}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("refs.jsonl")).unwrap(),
        before
    );

    let out = attach(
        &dir,
        &["--registry", "typo/refs.jsonl", "--text", "texts/a.txt"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs a registry"));
    assert!(
        !dir.join("typo").exists(),
        "no directory or lock file is created"
    );
    std::fs::remove_file(outside).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
