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
fn attach_keeps_the_download_time_and_unknown_fields_of_an_unchanged_text() {
    let dir = project("retrieved");
    let sha = "8d6b0fa5ca0a1a6cbc3b2d5ad21da1e2b0b9d8f43cbd1c9ac1e0d8d9bf9bd8c1";
    let text = format!(
        r#"{{"path":"texts/a.txt","sha256":"{sha}","origin":"https://example.org/a.pdf","retrieved":"2026-11-07T10:00:00Z","future":1}}"#
    );
    // The registry as `read --record` and a later papers version would leave it.
    let entry = ENTRY.replace(r#""texts":[]"#, &format!(r#""texts":[{text}]"#));
    std::fs::write(dir.join("refs.jsonl"), format!("{entry}\n")).unwrap();
    // `attach` hashes the file, so give the entry the file's real hash.
    assert!(attach(&dir, &["--text", "texts/a.txt"]).status.success());
    let real = texts(&dir)[0]["sha256"].as_str().unwrap().to_owned();
    let entry = entry.replace(sha, &real);
    std::fs::write(dir.join("refs.jsonl"), format!("{entry}\n")).unwrap();

    assert!(attach(&dir, &["--text", "texts/a.txt"]).status.success());
    let t = texts(&dir);
    assert_eq!(t[0]["retrieved"], "2026-11-07T10:00:00Z");
    assert_eq!(t[0]["future"], 1);
    assert_eq!(t[0]["origin"], "https://example.org/a.pdf");

    // A changed file is a new download: its time is not known.
    std::fs::write(dir.join("texts/a.txt"), "edited text").unwrap();
    assert!(attach(&dir, &["--text", "texts/a.txt"]).status.success());
    let t = texts(&dir);
    assert!(t[0].get("retrieved").is_none(), "{t}");
    assert!(t[0].get("future").is_none(), "{t}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attach_refuses_paths_out_binary_empty_directories_and_a_missing_registry() {
    let dir = project("refuse-more");
    let name = format!("papers-outside-up-{}.txt", std::process::id());
    let outside = dir.parent().unwrap().join(&name);
    std::fs::write(&outside, "outside").unwrap();
    let up = format!("texts/../../{name}");
    std::fs::write(dir.join("texts/a.pdf"), b"%PDF-1.7\n\xff\xfe\x00binary").unwrap();
    std::fs::write(dir.join("texts/empty.txt"), "").unwrap();
    let before = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    for (path, message) in [
        (up.as_str(), "outside the project root"),
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
    let out = attach(&dir, &["--text", "texts/a.txt", "--retrieved", "yesterday"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("RFC 3339"));
    std::fs::remove_file(outside).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A `sources` directory linked to storage outside the project, as in a
/// project whose texts live on shared storage.
#[test]
fn attach_accepts_a_symlinked_sources_directory_and_updates_its_manifest() {
    let dir = project("linked");
    let store = dir
        .parent()
        .unwrap()
        .join(format!("papers-attach-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(store.join("text")).unwrap();
    std::os::unix::fs::symlink(&store, dir.join("sources")).unwrap();
    std::fs::write(store.join("text/k.txt"), "Synthetic      Layout\n").unwrap();
    // The manifest `read` wrote for the plain text this one replaced.
    let stale = r#"{"path":"sources/text/k.txt","sha256":"00","origin":"https://example.org/old.pdf","retrieved":"2026-11-01T00:00:00Z"}"#;
    std::fs::write(store.join("text/k.json"), stale).unwrap();

    let out = attach(
        &dir,
        &[
            "--text",
            "sources/text/k.txt",
            "--origin",
            "https://example.org/k.pdf",
            "--retrieved",
            "2026-11-07T19:00:00+09:00",
            "--extraction",
            "layout",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = texts(&dir);
    assert_eq!(t[0]["path"], "sources/text/k.txt");
    assert_eq!(t[0]["origin"], "https://example.org/k.pdf");
    assert_eq!(t[0]["retrieved"], "2026-11-07T10:00:00Z");
    assert_eq!(t[0]["extraction"], "layout");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(store.join("text/k.json")).unwrap()).unwrap();
    assert_eq!(manifest, t[0], "the manifest matches the registry");

    // Attached again without the options: an unchanged file keeps them.
    assert!(
        attach(&dir, &["--text", "sources/text/k.txt"])
            .status
            .success()
    );
    assert_eq!(texts(&dir)[0], t[0]);

    // A changed file drops them, and says so.
    std::fs::write(store.join("text/k.txt"), "Synthetic plain\n").unwrap();
    let out = attach(&dir, &["--text", "sources/text/k.txt"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("layout extraction mode is dropped"),
        "{stdout}"
    );
    assert!(stdout.contains("download time"), "{stdout}");
    let t = texts(&dir);
    assert!(t[0].get("extraction").is_none(), "{t}");
    assert!(t[0].get("retrieved").is_none(), "{t}");

    // A JSON file beside the text that is not its manifest is left alone.
    std::fs::write(store.join("text/k.json"), "{\"other\":1}").unwrap();
    let out = attach(&dir, &["--text", "sources/text/k.txt"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("left"));
    assert_eq!(
        std::fs::read_to_string(store.join("text/k.json")).unwrap(),
        "{\"other\":1}"
    );
    std::fs::remove_dir_all(&store).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
