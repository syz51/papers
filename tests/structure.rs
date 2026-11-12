//! `papers structure` writes an `rf.paper-structure/1` sidecar beside a text
//! and leaves the text as it is.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PAPER: &str = include_str!("fixtures/structure-paper.txt");

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("papers-structure-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("text")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        "[papers]\nregistry = \"refs.jsonl\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("text/87.txt"), PAPER).unwrap();
    dir
}

fn entry(key: &str, doi: Option<&str>, texts: &str) -> String {
    let doi = doi.map_or("null".to_owned(), |d| format!("\"{d}\""));
    format!(
        r#"{{"schema":"rf.reference/1","key":"{key}","type":"paper","title":"Title Of Entry Number {key}","authors":null,"year":null,"venue":null,"ids":{{"doi":{doi},"arxiv":null,"dblp":null,"openalex":null,"url":null}},"version":null,"accessed":null,"depth":null,"texts":[{texts}],"duplicate_of":null,"source":null,"verified":null}}"#
    )
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run papers")
}

fn sidecar(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("text/87.txt.structure.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn a_path_gets_a_sidecar_and_the_text_keeps_its_bytes() {
    let dir = project("path");
    let out = papers(&dir, &["structure", "text/87.txt", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!dir.join("text/87.txt.structure.json").exists());
    let out = papers(&dir, &["structure", "text/87.txt"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("1 texts: 1 with a reference list, 1 with an abstract span"),
        "{stdout}"
    );
    let after = std::fs::read(dir.join("text/87.txt")).unwrap();
    let got = sidecar(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(after, PAPER.as_bytes());
    assert_eq!(got["schema"], "rf.paper-structure/1");
    assert_eq!(got["text"]["path"], "text/87.txt");
    assert_eq!(got["text"]["lines"], PAPER.lines().count());
    // No registry entry records this file, so its extraction is not known.
    assert_eq!(got["text"]["extraction"], "unknown");
    assert_eq!(got["pages"], "unknown");
    let sha = got["text"]["sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64);
    assert_eq!(got["references"]["split"], "numbered");
    assert_eq!(got["references"]["entries"].as_array().unwrap().len(), 5);
}

#[test]
fn a_key_names_the_entrys_texts_and_reference_entries_cite_other_entries() {
    let dir = project("key");
    // The sidecar of a path names the text's sha256.
    assert!(papers(&dir, &["structure", "text/87.txt"]).status.success());
    let sha = sidecar(&dir)["text"]["sha256"].as_str().unwrap().to_owned();
    let registry = [
        entry("12", Some("10.1234/synthetic.12"), ""),
        entry(
            "87",
            None,
            &format!(
                r#"{{"path":"text/87.txt","sha256":"{sha}","origin":null,"extraction":"pdftotext-layout"}}"#
            ),
        ),
    ]
    .join("\n")
        + "\n";
    std::fs::write(dir.join("refs.jsonl"), &registry).unwrap();
    let out = papers(&dir, &["structure", "87"]);
    assert!(out.status.success(), "{out:?}");
    let first = std::fs::read(dir.join("text/87.txt.structure.json")).unwrap();
    // A second run over the same text writes the same bytes.
    let out = papers(&dir, &["structure", "--all"]);
    assert!(out.status.success(), "{out:?}");
    let second = std::fs::read(dir.join("text/87.txt.structure.json")).unwrap();
    let got = sidecar(&dir);
    let unchanged = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    let missing = papers(&dir, &["structure", "12"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(first, second);
    assert_eq!(unchanged, registry);
    assert_eq!(got["text"]["extraction"], "pdftotext-layout");
    assert_eq!(got["warnings"], serde_json::json!([]));
    let cited = &got["references"]["entries"][0];
    assert_eq!(cited["key"], "12");
    assert_eq!(cited["by"], "doi");
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("entry 12 records no text"),
        "{missing:?}"
    );
}
