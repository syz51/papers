//! `papers read --extraction docling` with a stand-in for Docling: the second
//! text, its manifest and its registry entry, what it leaves unchanged, the
//! write order, the error without Docling, and stale derivatives.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const PDF: &[u8] = b"%PDF-1.4\na stand-in PDF\n";
const PLAIN: &str = "A Synthetic Title\n\nIntroduction\nSome words.\n";

/// Stands in for Python with Docling, called as `sh stub -c <script> <args>`.
/// The Markdown it writes has a NUL byte, which papers drops.
const STUB: &str = r#"[ "$3" = "--version" ] && { echo '{"docling": "9.9.9"}'; exit 0; }
printf '# A Synthetic Title\n\n## 1 Introduction\n\nSome\000 words.\n\n<!-- page -->\n\n## 2 Methods\n\nMore words.\n' > "$4"
echo "$3" >> "$(dirname "$0")/calls"
EXTRA
echo '{"docling": "9.9.9"}'
"#;

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Project {
    dir: PathBuf,
    /// Takes any HTTP request papers makes; none is expected.
    listener: TcpListener,
    plain_text: String,
    entry: String,
}

/// A project with entry 4: a plain text, its manifest and its saved PDF.
/// `extra`: shell lines the stub runs after it writes the Markdown.
fn project(name: &str, docling: &str, extra: &str) -> Project {
    let dir = std::env::temp_dir().join(format!("papers-docling-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let texts = dir.join("sources/text");
    std::fs::create_dir_all(&texts).unwrap();
    let stub = dir.join("docling.sh");
    std::fs::write(&stub, STUB.replace("EXTRA", extra)).unwrap();
    let docling = docling.replace("STUB", &stub.display().to_string());
    std::fs::write(
        dir.join("research.toml"),
        format!("[papers]\nregistry = \"refs.jsonl\"\ndocling = {docling}\n"),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "http://127.0.0.1:{}/paper.pdf",
        listener.local_addr().unwrap().port()
    );
    let plain_text = format!(
        r#"{{"path":"sources/text/4.txt","sha256":"{}","origin":"{url}","retrieved":"2026-11-07T10:00:00Z","extraction":"plain","pdf_sha256":"{}"}}"#,
        sha256(PLAIN.as_bytes()),
        sha256(PDF)
    );
    let entry = format!(
        r#"{{"schema":"rf.reference/1","key":"4","type":"paper","title":"A Synthetic Title","authors":["Ann Lee"],"year":2026,"venue":null,"ids":{{"doi":null,"arxiv":null,"dblp":null,"openalex":null,"url":"{url}"}},"version":null,"accessed":null,"depth":null,"texts":[{plain_text}],"duplicate_of":null,"source":null,"verified":null}}"#
    );
    std::fs::write(dir.join("refs.jsonl"), format!("{entry}\n")).unwrap();
    std::fs::write(texts.join("4.txt"), PLAIN).unwrap();
    std::fs::write(texts.join("4.json"), format!("{plain_text}\n")).unwrap();
    std::fs::write(texts.join("4.pdf"), PDF).unwrap();
    Project {
        dir,
        listener,
        plain_text,
        entry,
    }
}

impl Project {
    fn papers(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_papers"))
            .args(args)
            .current_dir(&self.dir)
            .output()
            .expect("run papers")
    }

    fn file(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.dir.join(path)).ok()
    }

    fn texts(&self) -> Vec<serde_json::Value> {
        let registry = std::fs::read_to_string(self.dir.join("refs.jsonl")).unwrap();
        let entry: serde_json::Value = serde_json::from_str(registry.trim()).unwrap();
        entry["texts"].as_array().unwrap().clone()
    }

    fn no_request(&self) {
        match self.listener.accept() {
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            other => panic!("papers made an HTTP request: {other:?}"),
        }
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.join("calls"))
            .map(|c| c.lines().count())
            .unwrap_or(0)
    }

    /// The plain text, its manifest and its entry are as they were made.
    fn plain_unchanged(&self) {
        assert_eq!(self.file("sources/text/4.txt").unwrap(), PLAIN.as_bytes());
        assert_eq!(
            self.file("sources/text/4.json").unwrap(),
            format!("{}\n", self.plain_text).as_bytes()
        );
        let plain: serde_json::Value = serde_json::from_str(&self.plain_text).unwrap();
        assert_eq!(self.texts()[0], plain);
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn docling_writes_a_second_text_its_manifest_and_entry() {
    let p = project("second", r#"["sh", "STUB"]"#, "");
    let args = ["read", "--key", "4", "--extraction", "docling", "--record"];
    let out = p.papers(&args);
    assert!(out.status.success(), "{}", stdout(&out));
    p.no_request();
    assert_eq!(p.calls(), 1);

    let md = p.file("sources/text/4.docling.md").unwrap();
    assert!(!md.contains(&0), "the NUL byte is dropped");
    let md = String::from_utf8(md).unwrap();
    assert!(md.contains("## 1 Introduction\n") && md.contains("<!-- page -->"));
    assert!(p.file("sources/text/4.docling.md.tmp").is_none());
    let manifest: serde_json::Value =
        serde_json::from_slice(&p.file("sources/text/4.docling.json").unwrap()).unwrap();
    let texts = p.texts();
    assert_eq!(texts.len(), 2);
    let docling = &texts[1];
    assert_eq!(docling, &manifest);
    assert_eq!(docling["path"], "sources/text/4.docling.md");
    assert_eq!(docling["sha256"], sha256(md.as_bytes()));
    assert_eq!(docling["extraction"], "docling");
    assert_eq!(docling["extractor"], "docling 9.9.9");
    assert_eq!(docling["pdf_sha256"], sha256(PDF));
    // The PDF's origin and download time, as recorded for the plain text.
    assert_eq!(docling["origin"], texts[0]["origin"]);
    assert_eq!(docling["retrieved"], "2026-11-07T10:00:00Z");
    p.plain_unchanged();

    // The structure sidecar reads the Markdown headings and page marks.
    let structure: serde_json::Value =
        serde_json::from_slice(&p.file("sources/text/4.docling.md.structure.json").unwrap())
            .unwrap();
    assert_eq!(structure["text"]["path"], "sources/text/4.docling.md");
    assert_eq!(structure["text"]["sha256"], docling["sha256"]);
    assert_eq!(structure["pages"].as_array().map(Vec::len), Some(2));
    let headings: Vec<&str> = structure["sections"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["heading"].as_str())
        .collect();
    assert!(headings.contains(&"## 2 Methods"), "{headings:?}");

    // A second run finds the text; --force converts the saved PDF again.
    // Neither adds a third entry or touches the plain text.
    let out = p.papers(&args);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("already have sources/text/4.docling.md"));
    assert_eq!(p.calls(), 1);
    let mut forced = args.to_vec();
    forced.push("--force");
    let out = p.papers(&forced);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(p.calls(), 2);
    assert_eq!(p.texts().len(), 2);
    assert_eq!(&p.texts()[1], docling);
    p.plain_unchanged();
    p.no_request();

    // The plain mode still answers a run without --extraction.
    let out = p.papers(&["read", "--key", "4", "--record"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("already have sources/text/4.txt"));
    assert_eq!(p.texts().len(), 2);
    p.plain_unchanged();
    p.no_request();
}

#[test]
fn without_docling_read_fails_before_any_download() {
    for (name, docling) in [
        ("missing", r#"["papers-no-such-docling"]"#),
        ("empty", "[]"),
    ] {
        let p = project(name, docling, "");
        // No saved PDF: a run with Docling would download it.
        std::fs::remove_file(p.dir.join("sources/text/4.pdf")).unwrap();
        let out = p.papers(&["read", "--key", "4", "--extraction", "docling", "--record"]);
        let text = stdout(&out);
        assert!(!out.status.success(), "{text}");
        assert!(text.contains("Docling extraction needs Docling"), "{text}");
        p.no_request();
        assert!(p.file("sources/text/4.pdf").is_none());
        assert!(p.file("sources/text/4.docling.md").is_none());
        assert_eq!(
            std::fs::read_to_string(p.dir.join("refs.jsonl")).unwrap(),
            format!("{}\n", p.entry)
        );
    }
}

#[test]
fn a_saved_pdf_without_a_recorded_sha256_is_not_converted() {
    let p = project("unknown-pdf", r#"["sh", "STUB"]"#, "");
    std::fs::write(p.dir.join("sources/text/4.pdf"), b"%PDF-1.4\nanother PDF\n").unwrap();
    let out = p.papers(&["read", "--key", "4", "--extraction", "docling"]);
    let text = stdout(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("record-pdfs"), "{text}");
    assert_eq!(p.calls(), 0);
    p.no_request();
}

#[test]
fn a_failed_record_leaves_no_docling_text_or_manifest() {
    // The entry leaves the registry while Docling runs, so it cannot be recorded.
    let p = project(
        "order",
        r#"["sh", "STUB"]"#,
        r#"printf '' > "$(dirname "$0")/refs.jsonl""#,
    );
    let out = p.papers(&["read", "--key", "4", "--extraction", "docling", "--record"]);
    let text = stdout(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("left the registry"), "{text}");
    assert_eq!(p.calls(), 1);
    for gone in [
        "sources/text/4.docling.md",
        "sources/text/4.docling.md.tmp",
        "sources/text/4.docling.json",
        "sources/text/4.docling.md.structure.json",
    ] {
        assert!(p.file(gone).is_none(), "{gone} was written");
    }
}

#[test]
fn registry_check_reports_stale_derivatives() {
    let p = project("stale", r#"["sh", "STUB"]"#, "");
    let out = p.papers(&["read", "--key", "4", "--extraction", "docling", "--record"]);
    assert!(out.status.success(), "{}", stdout(&out));
    let check = || {
        let out = p.papers(&["registry", "check", "--format", "jsonl"]);
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| l.contains("derivative-stale"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(check(), Vec::<String>::new());

    // The Markdown changes: its structure sidecar is stale.
    let md = p.dir.join("sources/text/4.docling.md");
    std::fs::write(&md, "# Edited\n").unwrap();
    let stale = check();
    assert_eq!(stale.len(), 1, "{stale:?}");
    assert!(
        stale[0].contains("4.docling.md.structure.json"),
        "{stale:?}"
    );
    assert!(stale[0].contains("papers structure sources/text/4.docling.md"));

    // The PDF changes: the Docling text is stale too.
    std::fs::write(p.dir.join("sources/text/4.pdf"), b"%PDF-1.4\nnew\n").unwrap();
    let stale = check();
    assert_eq!(stale.len(), 2, "{stale:?}");
    assert!(
        stale
            .iter()
            .any(|f| f.contains("--extraction docling --record")),
        "{stale:?}"
    );
}
