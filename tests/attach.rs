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
