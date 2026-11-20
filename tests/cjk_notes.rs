//! Notes for works without a DOI: a note sets the registry `type` and
//! `ids.openalex`, and `bib` gives works by Chinese-named authors keys of
//! their own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NOTES: [(&str, &str); 4] = [
    (
        "2012-chen-xue-er",
        "title: “学而时习之”与《论语》的开端\nauthors: [陈赟]\nyear: 2012\nopenalex: openalex:W2345273518\nurl: https://openalex.org/W2345273518\n",
    ),
    (
        "2012-li-sang-jia-gou",
        "title: 丧家狗：我读《论语》\nauthors: [李零]\nyear: 2012\nauthor_variants: [Ling Li]\nurl: https://example.org/li\n",
    ),
    (
        "sbck-lunyu-jijie",
        "title: 論語集解\nauthors: []\nkind: edition\ntype: other\nurl: https://example.org/KR1h0005\n",
    ),
    (
        "bad-fields",
        "title: A Synthetic Title\nauthors: [Ann Lee]\nyear: 2020\ntype: edition\nopenalex: 2345273518\nurl: https://example.org/bad\n",
    ),
];

fn project(name: &str, notes: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("papers-cjk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("notes")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        "[papers]\nregistry = \"refs.jsonl\"\nnotes = \"notes\"\n",
    )
    .unwrap();
    for (id, fields) in notes {
        std::fs::write(
            dir.join(format!("notes/{id}.md")),
            format!("---\nid: {id}\n{fields}---\n"),
        )
        .unwrap();
    }
    dir
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        .env("PAPERS_LOCK_DIR", dir.join("locks"))
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

fn entries(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("refs.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn entry(dir: &Path, key: &str) -> serde_json::Value {
    entries(dir)
        .into_iter()
        .find(|e| e["key"] == key)
        .unwrap_or_else(|| panic!("no entry {key}"))
}

#[test]
fn a_note_sets_the_type_and_the_openalex_id_of_its_entry() {
    let dir = project("fields", &NOTES[..3]);
    let build = papers(&dir, &["registry", "build"]);
    assert!(build.status.success(), "{}", all(&build));
    let edition = entry(&dir, "sbck-lunyu-jijie");
    assert_eq!(edition["type"], "other");
    assert!(edition["ids"]["openalex"].is_null());
    let article = entry(&dir, "2012-chen-xue-er");
    assert_eq!(article["type"], "paper");
    assert_eq!(article["ids"]["openalex"], "W2345273518");

    let check = papers(&dir, &["registry", "check"]);
    let printed = all(&check);
    assert!(check.status.success(), "{printed}");
    assert!(!printed.contains("invalid-value"), "{printed}");
    // A rebuild keeps both values.
    let again = papers(&dir, &["registry", "build"]);
    assert!(again.status.success(), "{}", all(&again));
    assert_eq!(entry(&dir, "sbck-lunyu-jijie")["type"], "other");
    assert_eq!(
        entry(&dir, "2012-chen-xue-er")["ids"]["openalex"],
        "W2345273518"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_type_or_an_openalex_id_outside_the_contract_is_reported() {
    let dir = project("bad", &NOTES[3..]);
    let build = papers(&dir, &["registry", "build"]);
    assert!(build.status.success(), "{}", all(&build));
    let check = papers(&dir, &["registry", "check"]);
    let printed = all(&check);
    assert!(!check.status.success(), "{printed}");
    assert!(
        printed.contains("type \"edition\" is not one of"),
        "{printed}"
    );
    assert!(printed.contains("ids.openalex \"2345273518\""), "{printed}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn works_by_chinese_named_authors_get_keys_of_their_own() {
    let dir = project("bib", &NOTES[..3]);
    let build = papers(&dir, &["registry", "build"]);
    assert!(build.status.success(), "{}", all(&build));
    let bib = papers(&dir, &["bib", "--format", "csl-json"]);
    assert!(bib.status.success(), "{}", all(&bib));
    let list: Vec<serde_json::Value> =
        serde_json::from_slice(&bib.stdout).expect("bib prints CSL JSON");
    let ids: Vec<&str> = list.iter().map(|e| e["id"].as_str().unwrap()).collect();
    // The name's characters; a Latin spelling in author_variants; no author.
    assert!(ids.contains(&"陈赟2012学而时习"), "{ids:?}");
    assert!(ids.contains(&"li2012丧家狗我"), "{ids:?}");
    assert!(ids.contains(&"anonnd論語集解"), "{ids:?}");
    let bibtex = papers(&dir, &["bib", "--format", "bibtex"]);
    let text = all(&bibtex);
    assert!(bibtex.status.success(), "{text}");
    assert!(text.contains("{陈赟2012学而时习,"), "{text}");
    assert!(!text.contains("anon2012"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}
