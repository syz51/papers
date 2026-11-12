//! `papers card` with a stand-in for Claude Code: the switch, the call, the
//! checks, the gate, the stored card and its stale report.

use std::path::PathBuf;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const TEXT: &str = "A Synthetic Title\n\nAbstract\nWe train a parser on the Penn Treebank and reach an F1\nof 92.4 on section 23, which beats the baseline by 1.2 points.\nThe method fails on long sentences with more than forty words.\n";

/// Stands in for `claude`: records its arguments, working directory and
/// standard input, then prints `reply.json`.
const STUB: &str = r#"dir=$(dirname "$0")
for a in "$@"; do printf '%s\n' "$a"; done > "$dir/args"
pwd > "$dir/cwd"
ls -A | wc -l | tr -d ' ' > "$dir/cwd-entries"
cat > "$dir/stdin"
echo call >> "$dir/calls"
cat "$dir/reply.json"
"#;

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Project {
    dir: PathBuf,
}

/// Entry 4 with a plain text; `cards` is the switch.
fn project(name: &str, cards: bool) -> Project {
    let dir = std::env::temp_dir().join(format!("papers-card-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sources/text")).unwrap();
    std::fs::write(dir.join("claude.sh"), STUB).unwrap();
    std::fs::write(
        dir.join("research.toml"),
        format!(
            "[papers]\nregistry = \"refs.jsonl\"\ncards = {cards}\nclaude = [\"sh\", \"{}\"]\n",
            dir.join("claude.sh").display()
        ),
    )
    .unwrap();
    let text = format!(
        r#"{{"path":"sources/text/4.txt","sha256":"{}","origin":"https://example.org/4.pdf","extraction":"plain"}}"#,
        sha256(TEXT.as_bytes())
    );
    std::fs::write(
        dir.join("refs.jsonl"),
        format!(
            r#"{{"schema":"rf.reference/1","key":"4","type":"paper","title":"A Synthetic Title","authors":["Ann Lee"],"year":2026,"venue":null,"ids":{{"doi":null,"arxiv":null,"dblp":null,"openalex":null,"url":null}},"version":null,"accessed":null,"depth":null,"texts":[{text}],"duplicate_of":null,"source":null,"verified":null}}"#
        ) + "\n",
    )
    .unwrap();
    std::fs::write(dir.join("sources/text/4.txt"), TEXT).unwrap();
    Project { dir }
}

/// A reply with `good` items whose quotes are found and whose numbers are in
/// them, one item with a number absent from its quote, and `bad` items whose
/// quotes are not in the text.
fn reply(good: usize, bad: usize) -> String {
    let mut items = Vec::new();
    for _ in 0..good {
        items.push(serde_json::json!({"field":"result","value":"A parser reaches an F1 of 92.4 on section 23.","quote":"train a parser on the Penn Treebank and reach an F1 of 92.4 on section 23"}));
    }
    items.push(serde_json::json!({"field":"result","value":"The parser beats the baseline by 3.5 points.","quote":"which beats the baseline by 1.2 points. The method fails on long sentences"}));
    for _ in 0..bad {
        items.push(serde_json::json!({"field":"limit","value":"It fails on long texts.","quote":"The method fails on very long documents and on sentences alike"}));
    }
    serde_json::json!({
        "type": "result", "is_error": false, "result": "",
        "structured_output": {"items": items},
        "total_cost_usd": 0.2,
        "usage": {"input_tokens": 1, "output_tokens": 1},
        "modelUsage": {"claude-haiku-5-5": {"inputTokens": 10, "cacheCreationInputTokens": 100, "cacheReadInputTokens": 1000, "outputTokens": 500, "costUSD": 0.11}}
    })
    .to_string()
}

impl Project {
    fn papers(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_papers"))
            .args(args)
            .current_dir(&self.dir)
            .output()
            .expect("run papers")
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(path)).ok()
    }

    fn calls(&self) -> usize {
        self.read("calls").map_or(0, |c| c.lines().count())
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn shown(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn cards_are_off_unless_switched_on() {
    let p = project("off", false);
    std::fs::write(p.dir.join("reply.json"), reply(9, 0)).unwrap();
    let out = p.papers(&["card", "--key", "4"]);
    assert_eq!(out.status.code(), Some(2), "{}", shown(&out));
    assert!(shown(&out).contains("cards = true"), "{}", shown(&out));
    assert_eq!(p.calls(), 0);
    assert!(p.read("sources/cards/4.json").is_none());
}

#[test]
fn a_card_keeps_checked_items_and_counts_the_dropped() {
    let p = project("card", true);
    std::fs::write(p.dir.join("reply.json"), reply(9, 0)).unwrap();
    let out = p.papers(&["card", "--key", "4"]);
    assert!(out.status.success(), "{}", shown(&out));
    assert_eq!(p.calls(), 1);

    // The call: the text on standard input, the prompt as the system prompt,
    // no tools or settings, in an empty directory.
    assert_eq!(p.read("stdin").unwrap(), TEXT);
    let args = p.read("args").unwrap();
    let args: Vec<&str> = args.lines().collect();
    for want in [
        "-p",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "claude-haiku-5-5",
    ] {
        assert!(args.contains(&want), "{want} missing from {args:?}");
    }
    let after = |flag: &str| args[args.iter().position(|a| *a == flag).unwrap() + 1];
    assert_eq!(after("--tools"), "");
    assert_eq!(after("--setting-sources"), "");
    assert_eq!(after("--output-format"), "json");
    let prompt = std::fs::read_to_string(after("--system-prompt-file")).unwrap();
    assert!(prompt.contains("Every number in the value must also appear in the quote"));
    assert_eq!(p.read("cwd-entries").unwrap().trim(), "0");
    assert!(
        p.read("cwd")
            .unwrap()
            .trim()
            .ends_with(".research/papers/card-cwd")
    );

    let card: serde_json::Value =
        serde_json::from_str(&p.read("sources/cards/4.json").unwrap()).unwrap();
    assert_eq!(card["schema"], "rf.paper-card/1");
    assert_eq!(card["key"], "4");
    assert_eq!(card["class"], "generated");
    assert_eq!(card["text"]["path"], "sources/text/4.txt");
    assert_eq!(card["text"]["sha256"], sha256(TEXT.as_bytes()));
    assert_eq!(card["model"], "claude-haiku-5-5");
    assert_eq!(card["checker"], "claimcheck 0.10.0");
    assert_eq!(card["usage"]["input_tokens"], 1110);
    assert_eq!(card["usage"]["output_tokens"], 500);
    assert_eq!(card["written"], 10);
    assert_eq!(card["dropped"], 1);
    assert_eq!(card["dropped_by"]["number"], 1);
    let items = card["items"].as_array().unwrap();
    assert_eq!(items.len(), 9);
    assert_eq!(items[0]["lines"], serde_json::json!([4, 5]));
    assert!(p.read("sources/cards/4.json.tmp").is_none());
    let replies: Vec<_> = std::fs::read_dir(p.dir.join(".research/papers/card-replies"))
        .unwrap()
        .collect();
    assert_eq!(replies.len(), 1);

    // Same text, prompt and model: no second call.
    let out = p.papers(&["card", "--key", "4"]);
    assert!(out.status.success(), "{}", shown(&out));
    assert!(shown(&out).contains("already have sources/cards/4.json"));
    assert_eq!(p.calls(), 1);

    // A saved reply is checked again without a call, and nothing is stored.
    let saved = replies[0].as_ref().unwrap().path();
    let before = p.read("sources/cards/4.json").unwrap();
    let out = p.papers(&["card", "--key", "4", "--reply", saved.to_str().unwrap()]);
    assert!(out.status.success(), "{}", shown(&out));
    assert!(
        shown(&out).contains("kept 9 of 10 items"),
        "{}",
        shown(&out)
    );
    assert!(shown(&out).contains("not stored: a saved reply is only checked"));
    assert_eq!(p.calls(), 1);
    assert_eq!(p.read("sources/cards/4.json").unwrap(), before);
}

#[test]
fn a_card_that_keeps_under_eighty_percent_is_not_stored() {
    let p = project("gate", true);
    // 7 found, 1 with a number absent from its quote, 2 not found: 70% kept.
    std::fs::write(p.dir.join("reply.json"), reply(7, 2)).unwrap();
    let out = p.papers(&["card", "--key", "4"]);
    assert_eq!(out.status.code(), Some(1), "{}", shown(&out));
    let text = shown(&out);
    assert!(text.contains("kept 7 of 10 items"), "{text}");
    assert!(
        text.contains("suspected text-extraction fault in sources/text/4.txt"),
        "{text}"
    );
    assert!(p.read("sources/cards/4.json").is_none());
    assert!(p.read("sources/cards/4.json.tmp").is_none());
    // The raw reply is kept all the same.
    assert_eq!(
        std::fs::read_dir(p.dir.join(".research/papers/card-replies"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn registry_check_reports_a_stale_card() {
    let p = project("stale", true);
    std::fs::write(p.dir.join("reply.json"), reply(9, 0)).unwrap();
    let out = p.papers(&["card", "--key", "4"]);
    assert!(out.status.success(), "{}", shown(&out));
    let stale = || {
        let out = p.papers(&["registry", "check", "--format", "jsonl"]);
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| l.contains("derivative-stale") && l.contains("cards/4.json"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(stale(), Vec::<String>::new());
    std::fs::write(p.dir.join("sources/text/4.txt"), "Another text.\n").unwrap();
    let found = stale();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("papers card --key 4 --force"),
        "{found:?}"
    );
}

#[test]
fn a_stale_docling_card_is_made_again_from_the_docling_text() {
    let p = project("stale-docling", true);
    std::fs::write(p.dir.join("reply.json"), reply(9, 0)).unwrap();
    std::fs::write(p.dir.join("sources/text/4.docling.md"), TEXT).unwrap();
    let refs = p.read("refs.jsonl").unwrap();
    let docling = format!(
        r#",{{"path":"sources/text/4.docling.md","sha256":"{}","origin":"https://example.org/4.pdf","extraction":"docling"}}]"#,
        sha256(TEXT.as_bytes())
    );
    std::fs::write(
        p.dir.join("refs.jsonl"),
        refs.replacen("}],", &format!("}}{docling},"), 1),
    )
    .unwrap();
    let out = p.papers(&["card", "--key", "4", "--docling"]);
    assert!(out.status.success(), "{}", shown(&out));
    let card: serde_json::Value =
        serde_json::from_str(&p.read("sources/cards/4.json").unwrap()).unwrap();
    assert_eq!(card["text"]["path"], "sources/text/4.docling.md");
    std::fs::write(p.dir.join("sources/text/4.docling.md"), "Another text.\n").unwrap();
    let out = p.papers(&["registry", "check", "--format", "jsonl"]);
    let found: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| l.contains("derivative-stale") && l.contains("cards/4.json"))
        .map(str::to_owned)
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("papers card --key 4 --force --docling"),
        "{found:?}"
    );
}
