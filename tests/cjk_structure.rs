//! `papers structure` on Chinese texts: characters count as words, a `摘要`
//! line starts the abstract, and `[papers] structure_headings` splits a
//! classical text.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ARTICLE: &str = "“学而时习之”与《论语》的开端\n\n　　摘　要：《论语》以“学而时习之”开端，导向的是人的自修之道。\n\n　　关键词：论语；开端；学\n\n一　《论语》为何以“学而时习”开端\n\n在某种意义上，《论语》彰显的是一个文化生命的深度与高度。\n";
const CLASSIC: &str = "#+TITLE: 論語\n論語學而第一　何晏集解(凢十六章)¶\n子曰學而時習之不亦悦乎¶\n論語爲政第二　何晏集解(凢卄四章)¶\n子曰爲政以徳譬如北辰¶\n";

fn project(name: &str, papers: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "papers-cjk-structure-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("texts")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        format!("[papers]\nregistry = \"refs.jsonl\"\n{papers}"),
    )
    .unwrap();
    std::fs::write(dir.join("refs.jsonl"), "").unwrap();
    std::fs::write(dir.join("texts/article.txt"), ARTICLE).unwrap();
    std::fs::write(dir.join("texts/classic.txt"), CLASSIC).unwrap();
    dir
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run papers")
}

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn sections(dir: &Path, text: &str) -> Vec<(String, Option<String>, u64, u64)> {
    let file = dir.join(format!("texts/{text}.structure.json"));
    let sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    sidecar["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_owned(),
                s["heading"].as_str().map(str::to_owned),
                s["first_line"].as_u64().unwrap(),
                s["last_line"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn span(
    kind: &str,
    heading: Option<&str>,
    first: u64,
    last: u64,
) -> (String, Option<String>, u64, u64) {
    (kind.to_owned(), heading.map(str::to_owned), first, last)
}

#[test]
fn a_chinese_article_has_an_abstract_span_and_a_character_count() {
    let dir = project("article", "");
    let out = papers(&dir, &["structure", "texts/article.txt"]);
    let printed = all(&out);
    assert!(out.status.success(), "{printed}");
    assert!(printed.contains("abstract 1, body 1"), "{printed}");
    // 77 Chinese characters, not the few runs between spaces.
    assert!(printed.contains("short text, 77 words"), "{printed}");
    assert_eq!(
        sections(&dir, "article.txt"),
        [
            span("front", None, 1, 2),
            span("abstract", Some("摘要"), 3, 4),
            span("unknown", None, 5, 6),
            span("body", Some("一 《论语》为何以“学而时习”开端"), 7, 9),
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_configured_heading_pattern_splits_a_classical_text() {
    let dir = project("none", "");
    let out = papers(&dir, &["structure", "texts/classic.txt"]);
    assert!(out.status.success(), "{}", all(&out));
    assert_eq!(sections(&dir, "classic.txt"), [span("unknown", None, 1, 5)]);
    std::fs::remove_dir_all(&dir).unwrap();

    let dir = project(
        "pattern",
        "structure_headings = ['^論語.{2,4}第[一二三四五六七八九十]+\\s']\n",
    );
    let out = papers(&dir, &["structure", "texts/classic.txt"]);
    assert!(out.status.success(), "{}", all(&out));
    assert_eq!(
        sections(&dir, "classic.txt"),
        [
            span("front", None, 1, 1),
            span("body", Some("論語學而第一 何晏集解(凢十六章)¶"), 2, 3),
            span("body", Some("論語爲政第二 何晏集解(凢卄四章)¶"), 4, 5),
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_heading_pattern_that_is_no_regex_is_an_error() {
    let dir = project("bad", "structure_headings = ['第(']\n");
    let out = papers(&dir, &["structure", "texts/classic.txt"]);
    let printed = all(&out);
    assert!(!out.status.success(), "{printed}");
    assert!(printed.contains("[papers] structure_headings"), "{printed}");
    assert!(!dir.join("texts/classic.txt.structure.json").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
