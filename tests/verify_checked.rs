//! A note's `checked` date reaches the registry entry, `verify --record`
//! reports `verify-manual` for it without a title search, and `registry
//! check` accepts the recorded status.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// An edition that no index lists: no DOI, no arXiv id, no year, no authors.
const NOTE: &str = "---\nid: sbck-lunyu-jijie\ntitle: 論語\nauthors: []\nvenue: 四部叢刊\nurl: https://example.org/KR1h0005\nchecked: 2026-11-20\n---\n";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("papers-checked-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("notes")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        "[papers]\nregistry = \"refs.jsonl\"\nnotes = \"notes\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("notes/sbck-lunyu-jijie.md"), NOTE).unwrap();
    dir
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        // A lookup would need the lock folder; none is made.
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

fn entry(dir: &Path) -> serde_json::Value {
    let registry = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    serde_json::from_str(registry.trim()).unwrap()
}

#[test]
fn a_checked_entry_is_verified_by_hand_and_passes_the_check() {
    let dir = project("manual");
    let build = papers(&dir, &["registry", "build"]);
    assert!(build.status.success(), "{}", all(&build));
    assert_eq!(entry(&dir)["checked"], "2026-11-20");
    assert!(entry(&dir)["verified"].is_null());

    let verify = papers(&dir, &["verify", "--record"]);
    let printed = all(&verify);
    assert!(verify.status.success(), "{printed}");
    assert!(printed.contains("verify-manual"), "{printed}");
    assert!(
        printed.contains("metadata confirmed by hand on 2026-11-20"),
        "{printed}"
    );
    // A title search would end as not found, failed or a match.
    assert!(
        printed.contains("verified 1 entries: 1 manual"),
        "{printed}"
    );
    assert!(!printed.contains("title search"), "{printed}");
    assert!(!dir.join("locks").exists(), "a request was made");
    let verified = &entry(&dir)["verified"];
    assert_eq!(verified["status"], "manual");
    assert_eq!(verified["against"], "note");

    let check = papers(&dir, &["registry", "check"]);
    let printed = all(&check);
    assert!(check.status.success(), "{printed}");
    assert!(
        printed.contains("checked 1 entries: 0 findings"),
        "{printed}"
    );

    // The record survives a rebuild, and a new date clears it.
    assert!(papers(&dir, &["registry", "build"]).status.success());
    assert_eq!(entry(&dir)["verified"]["status"], "manual");
    std::fs::write(
        dir.join("notes/sbck-lunyu-jijie.md"),
        NOTE.replace("2026-11-20", "2026-11-21"),
    )
    .unwrap();
    assert!(papers(&dir, &["registry", "build"]).status.success());
    assert_eq!(entry(&dir)["checked"], "2026-11-21");
    assert!(entry(&dir)["verified"].is_null());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A registry written before `checked` existed still loads, and an entry
/// without the field has none written.
#[test]
fn an_entry_without_a_checked_date_keeps_its_shape() {
    let dir = project("old");
    std::fs::write(
        dir.join("notes/sbck-lunyu-jijie.md"),
        NOTE.replace("checked: 2026-11-20\n", ""),
    )
    .unwrap();
    assert!(papers(&dir, &["registry", "build"]).status.success());
    assert!(entry(&dir).get("checked").is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}
