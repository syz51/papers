//! `papers registry record-pdfs` as a command: its output and what it leaves
//! unchanged. The unit tests in `src/main.rs` cover the texts it records.

use std::path::Path;
use std::process::{Command, Output};

const TEXT: &str = r#"{"path":"sources/text/1.txt","sha256":"c2f5e4bde5ddcb0a6e6f5f6a0a3d9f2f3a1f7a7f4f2f6d4d0b0f6e7a5c3b1a09","origin":"https://example.invalid/1.pdf","retrieved":"2026-11-07T10:00:00Z"}"#;

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        // No pdftotext, whatever the host has.
        .env("PATH", "")
        .output()
        .expect("run papers")
}

#[test]
fn record_pdfs_lists_a_text_its_pdf_does_not_reproduce_and_changes_nothing() {
    let dir = std::env::temp_dir().join(format!("papers-record-pdfs-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let texts = dir.join("sources/text");
    std::fs::create_dir_all(&texts).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        "[papers]\nregistry = \"refs.jsonl\"\n",
    )
    .unwrap();
    let entry = format!(
        r#"{{"schema":"rf.reference/1","key":"12","type":"paper","title":"A Synthetic Title","authors":["Ann Lee"],"year":2026,"venue":null,"ids":{{"doi":null,"arxiv":null,"dblp":null,"openalex":null,"url":null}},"version":null,"accessed":null,"depth":null,"texts":[{TEXT}],"duplicate_of":null,"source":null,"verified":null}}"#
    );
    std::fs::write(dir.join("refs.jsonl"), format!("{entry}\n")).unwrap();
    std::fs::write(texts.join("1.txt"), "a text").unwrap();
    std::fs::write(texts.join("1.json"), TEXT).unwrap();
    std::fs::write(texts.join("1.pdf"), "%PDF-1.4\nnot the paper").unwrap();

    for args in [
        &["registry", "record-pdfs"][..],
        &[
            "registry",
            "record-pdfs",
            "--dir",
            "sources/text",
            "--dry-run",
        ],
    ] {
        let out = papers(&dir, args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}{:?}", out.stderr);
        // Named by the key of the entry that lists the text.
        assert!(
            stdout.contains("not reproduced: 12 (sources/text/1.txt): "),
            "{stdout}"
        );
        assert!(stdout.contains("pdftotext is not on PATH"), "{stdout}");
        assert!(stdout.contains(", 1 not reproduced, "), "{stdout}");
        assert_eq!(stdout.lines().last(), Some("not reproduced: 12"));
        assert_eq!(std::fs::read_to_string(texts.join("1.json")).unwrap(), TEXT);
        assert_eq!(
            std::fs::read_to_string(dir.join("refs.jsonl")).unwrap(),
            format!("{entry}\n")
        );
    }

    let help = papers(&dir, &["registry", "record-pdfs", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("Makes no network request"), "{help}");
    assert!(help.contains("--dry-run"), "{help}");
    std::fs::remove_dir_all(&dir).unwrap();
}
