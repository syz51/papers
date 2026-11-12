//! `fetch` and `read` report a registered paper first and make no request.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TEXT: &str = "the recorded text";
const TEXT_SHA256: &str = "5ba9fde034081e7f1e2ebf9599f55faa03b3934553f63aee753204a390141638";

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("papers-registered-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("texts")).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        "[papers]\nregistry = \"refs.jsonl\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("texts/87.txt"), TEXT).unwrap();
    let entry = format!(
        r#"{{"schema":"rf.reference/1","key":"87","type":"paper","title":"A Synthetic Title","authors":["Ann Lee"],"year":2026,"venue":"arXiv preprint","ids":{{"doi":null,"arxiv":"2601.00001","dblp":null,"openalex":null,"url":null}},"version":"v1","accessed":null,"depth":"full","texts":[{{"path":"texts/87.txt","sha256":"{TEXT_SHA256}","origin":"https://arxiv.org/pdf/2601.00001v1"}}],"duplicate_of":null,"source":null,"verified":null}}"#
    );
    std::fs::write(dir.join("refs.jsonl"), format!("{entry}\n")).unwrap();
    dir
}

fn papers(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papers"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run papers")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const BLOCK: &str = "registered: key 87 · depth full · A Synthetic Title\ntext: texts/87.txt\n";

#[test]
fn fetch_prints_the_registered_block_and_exits_0() {
    let dir = project("fetch");
    let out = papers(&dir, &["fetch", "arXiv:2601.00001v3"]);
    assert!(out.status.success());
    // The block and a blank line, and nothing arXiv would add.
    assert_eq!(stdout(&out), format!("{BLOCK}\n"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("registered; not fetched (--refresh to fetch)"),
        "{err}"
    );
    // No request, so no log entry and no cached response.
    assert!(!dir.join(".research").exists());
    let json = papers(&dir, &["fetch", "--json", "2601.00001"]);
    let line: serde_json::Value = serde_json::from_str(stdout(&json).trim()).unwrap();
    assert_eq!(
        line,
        serde_json::json!({"input": "2601.00001", "registered": {
            "key": "87", "title": "A Synthetic Title", "depth": "full", "duplicate_of": null,
            "texts": [{"path": "texts/87.txt", "state": "ok"}],
        }})
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn read_prints_the_block_then_the_recorded_text_and_its_manifest_last() {
    let dir = project("read");
    let out = papers(&dir, &["read", "2601.00001"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(format!("{}\n{}\n", lines[0], lines[1]), BLOCK);
    assert_eq!(
        lines[2],
        "already have texts/87.txt (sha256 matches); not downloading (--force to fetch again)"
    );
    let manifest: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(manifest["path"], "texts/87.txt");
    assert_eq!(manifest["sha256"], TEXT_SHA256);
    // Nothing was downloaded or written.
    assert!(!dir.join("sources").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
