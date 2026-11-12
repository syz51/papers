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
        stdout.contains("extraction mode layout is dropped"),
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

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Attaching a text that `read` downloaded, without options, keeps what its
/// manifest records about the same file, in the manifest and the registry.
#[test]
fn attach_keeps_what_the_manifest_of_an_unchanged_text_records() {
    let dir = project("manifest-keep");
    let sha = "fa0343363edb8cb3ed1c098a47c28e56a7d7c8ade8dce7ed408b12b6280dd135";
    let manifest = format!(
        r#"{{"path":"texts/a.txt","sha256":"{sha}","origin":"https://example.org/a.pdf","retrieved":"2026-11-07T10:00:00Z","extraction":"layout"}}"#
    );
    std::fs::write(dir.join("texts/a.json"), &manifest).unwrap();
    ok(&attach(&dir, &["--text", "texts/a.txt"]));
    let t = texts(&dir);
    assert_eq!(t[0]["origin"], "https://example.org/a.pdf");
    assert_eq!(t[0]["retrieved"], "2026-11-07T10:00:00Z");
    assert_eq!(t[0]["extraction"], "layout");
    assert_eq!(read_json(&dir.join("texts/a.json")), t[0]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A mode a later version writes loads, and survives a rewrite.
#[test]
fn attach_keeps_an_unknown_extraction_mode() {
    let dir = project("unknown-mode");
    let sha = "fa0343363edb8cb3ed1c098a47c28e56a7d7c8ade8dce7ed408b12b6280dd135";
    let text =
        format!(r#"{{"path":"texts/a.txt","sha256":"{sha}","origin":null,"extraction":"ocr"}}"#);
    let entry = ENTRY.replace(r#""texts":[]"#, &format!(r#""texts":[{text}]"#));
    std::fs::write(dir.join("refs.jsonl"), format!("{entry}\n")).unwrap();
    ok(&attach(&dir, &["--text", "texts/a.txt"]));
    assert_eq!(texts(&dir)[0]["extraction"], "ocr");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attach_records_the_download_time_in_utc_to_the_second() {
    let dir = project("round");
    ok(&attach(
        &dir,
        &[
            "--text",
            "texts/a.txt",
            "--retrieved",
            "2026-11-07T19:00:00.4+09:00",
        ],
    ));
    assert_eq!(texts(&dir)[0]["retrieved"], "2026-11-07T10:00:00Z");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attach_updates_the_manifest_of_an_upper_case_txt_file() {
    let dir = project("upper");
    std::fs::write(dir.join("texts/Paper.TXT"), "upper text").unwrap();
    let stale =
        r#"{"path":"texts/Paper.TXT","sha256":"00","origin":"https://example.org/old.pdf"}"#;
    std::fs::write(dir.join("texts/Paper.json"), stale).unwrap();
    ok(&attach(&dir, &["--text", "texts/Paper.TXT"]));
    assert_eq!(read_json(&dir.join("texts/Paper.json")), texts(&dir)[0]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A manifest that cannot be read or written fails the attach, and leaves
/// the registry unchanged.
#[test]
fn attach_fails_on_a_manifest_it_cannot_read_or_write() {
    use std::os::unix::fs::PermissionsExt;
    let dir = project("manifest-perm");
    let sidecar = dir.join("texts/a.json");
    std::fs::write(
        &sidecar,
        r#"{"path":"texts/a.txt","sha256":"00","origin":null}"#,
    )
    .unwrap();
    let before = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    for mode in [0o000, 0o444] {
        std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(mode)).unwrap();
        let out = attach(&dir, &["--text", "texts/a.txt"]);
        assert_eq!(out.status.code(), Some(2), "mode {mode:o}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("a.json"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join("refs.jsonl")).unwrap(),
            before,
            "mode {mode:o}"
        );
    }
    std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A symlinked file must point into the project or its own directory; a
/// `..` after a symlinked directory must not name another file.
#[test]
fn attach_refuses_file_links_out_and_dot_dot_through_links() {
    let dir = project("links-out");
    let store = dir
        .parent()
        .unwrap()
        .join(format!("papers-attach-store-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(store.join("a/text")).unwrap();
    std::fs::write(store.join("secret.txt"), "outside").unwrap();
    std::fs::write(store.join("a/text/k.txt"), "stored").unwrap();
    std::os::unix::fs::symlink(store.join("secret.txt"), dir.join("texts/out.txt")).unwrap();
    std::os::unix::fs::symlink(store.join("a"), dir.join("sources")).unwrap();
    // `sources/../secret.txt`: the system reads store/secret.txt; without
    // the link, the path names this file in the project.
    std::fs::write(dir.join("secret.txt"), "inside").unwrap();
    // A link into its own (linked) directory is fine.
    std::os::unix::fs::symlink("k.txt", store.join("a/text/alias.txt")).unwrap();
    let before = std::fs::read_to_string(dir.join("refs.jsonl")).unwrap();
    for (path, message) in [
        ("texts/out.txt", "is a symlink to a file outside"),
        (
            "sources/../secret.txt",
            "a `..` after a symlinked directory",
        ),
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
    ok(&attach(&dir, &["--text", "sources/text/alias.txt"]));
    assert_eq!(texts(&dir)[0]["path"], "sources/text/alias.txt");
    std::fs::remove_dir_all(&store).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The project named through a link, and its `sources` linked elsewhere.
#[test]
fn attach_accepts_a_linked_sources_directory_in_a_linked_project() {
    let dir = project("linked-both");
    let store = dir
        .parent()
        .unwrap()
        .join(format!("papers-attach-store-both-{}", std::process::id()));
    let link = dir
        .parent()
        .unwrap()
        .join(format!("papers-attach-link-both-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    let _ = std::fs::remove_file(&link);
    std::fs::create_dir_all(store.join("text")).unwrap();
    std::fs::write(store.join("text/k.txt"), "stored").unwrap();
    std::os::unix::fs::symlink(&store, dir.join("sources")).unwrap();
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let config = link.join("research.toml");
    ok(&attach(
        &dir,
        &[
            "--config",
            config.to_str().unwrap(),
            "--text",
            "sources/text/k.txt",
        ],
    ));
    assert_eq!(texts(&dir)[0]["path"], "sources/text/k.txt");
    std::fs::remove_file(&link).unwrap();
    std::fs::remove_dir_all(&store).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attach_keeps_the_extractor_and_pdf_sha256_of_a_docling_text() {
    let dir = project("docling");
    let md = "# A Synthetic Title\n\nBody.\n";
    std::fs::write(dir.join("texts/1.docling.md"), md).unwrap();
    let sha = {
        use sha2::Digest;
        sha2::Sha256::digest(md.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    // The manifest `read --extraction docling` leaves without --record.
    let manifest = format!(
        r#"{{"path":"texts/1.docling.md","sha256":"{sha}","origin":"https://arxiv.org/pdf/2601.00001v1","retrieved":"2026-11-07T10:00:00Z","extraction":"docling","pdf_sha256":"ab12","extractor":"docling 2.150.0"}}"#
    );
    std::fs::write(dir.join("texts/1.docling.json"), format!("{manifest}\n")).unwrap();
    let out = attach(
        &dir,
        &["--text", "texts/1.docling.md", "--extraction", "docling"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("manifest: "), "{stdout}");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("texts/1.docling.json")).unwrap())
            .unwrap();
    let t = texts(&dir);
    for record in [&t[0], &written] {
        assert_eq!(record["extractor"], "docling 2.150.0", "{record}");
        assert_eq!(record["pdf_sha256"], "ab12", "{record}");
        assert_eq!(record["extraction"], "docling", "{record}");
        assert_eq!(record["retrieved"], "2026-11-07T10:00:00Z", "{record}");
        assert_eq!(record["origin"], "https://arxiv.org/pdf/2601.00001v1");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
