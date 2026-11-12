//! `papers card`: a paper card (`rf.paper-card/1`, DR043). One headless
//! Claude call reads a paper's local text, with the card prompt as its system
//! prompt, and writes items of `field`, `value` and `quote`. Each item is then
//! checked with claimcheck's text forms, and no model takes part:
//!
//! - the quote has at least [`MIN_WORDS`] words;
//! - the quote is one contiguous passage of the text, as claimcheck matches a
//!   quotation (normalised, not cutting a word or number), which gives its
//!   raw line range;
//! - every number in the value occurs in the quote, as claimcheck matches
//!   numbers, unless it is part of a name ([`missing_numbers`]).
//!
//! An item that fails is dropped and counted. A card that keeps less than
//! [`MIN_KEPT_PERCENT`] of its items is not stored. When quotes were not found,
//! the text is a suspected extraction fault; when most drops are numbers, the
//! reply is at fault ([`refusal`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use claimcheck::normalize;
use claimcheck::numbers;
use claimcheck::source::IndexedText;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::registry::{self, Extraction};
use crate::{derivative, finding, http, pdf};

pub const SCHEMA: &str = "rf.paper-card/1";
/// The system prompt of the call.
pub const PROMPT: &str = include_str!("card_prompt.md");
/// The JSON schema the reply must follow (`--json-schema`).
pub const REPLY_SCHEMA: &str = include_str!("card_schema.json");
/// The fields an item may have.
pub const FIELDS: [&str; 6] = ["task", "dataset", "method", "metric", "result", "limit"];
/// The fewest words a quote may have.
pub const MIN_WORDS: usize = 8;
/// The smallest share of items, in percent, a card must keep to be stored.
pub const MIN_KEPT_PERCENT: usize = 80;
/// The quote and number checks: claimcheck's text forms, as a library.
pub const CHECKER: &str = "claimcheck 0.10.0";
/// The longest a call may take.
const TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// The most a call may print.
const MAX_REPLY: u64 = 16 << 20;

/// Why a card under [`MIN_KEPT_PERCENT`] is not stored, by its main cause.
/// Quotes not found in the text, when they are at least half of the drops,
/// point at the extraction; the other drops break the prompt's rules and
/// point at the reply.
pub fn refusal(dropped: &Dropped, path: &str, key: &str) -> String {
    let head = format!("not stored: the card keeps under {MIN_KEPT_PERCENT}% of its items");
    let reply = dropped.number + dropped.short + dropped.field;
    if dropped.not_found < reply {
        format!(
            "{head}; most dropped items break the prompt's rules (number not in quote {}, quote under {MIN_WORDS} words {}, unknown field {}), a fault of the reply, not of {path} (try papers card --key {key} --force)",
            dropped.number, dropped.short, dropped.field
        )
    } else {
        format!(
            "{head}; suspected text-extraction fault in {path} (try papers read --key {key} --extraction docling --record, then papers card --key {key} --docling)"
        )
    }
}

/// An item as the model wrote it.
#[derive(Debug, Clone, Deserialize)]
pub struct Written {
    pub field: String,
    pub value: String,
    pub quote: String,
}

/// A kept item: its quote was found at raw lines `lines` (1-based, inclusive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub field: String,
    pub value: String,
    pub quote: String,
    pub lines: [usize; 2],
}

/// Why items were dropped, as counts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Dropped {
    /// The field is not one of [`FIELDS`].
    pub field: usize,
    /// The quote has fewer than [`MIN_WORDS`] words.
    pub short: usize,
    /// The quote is not a contiguous passage of the text.
    pub not_found: usize,
    /// The value has a number that its quote lacks.
    pub number: usize,
}

impl Dropped {
    pub fn total(&self) -> usize {
        self.field + self.short + self.not_found + self.number
    }
}

/// The items kept and the counts of those dropped.
#[derive(Debug, Default, PartialEq)]
pub struct Checked {
    pub kept: Vec<Item>,
    pub dropped: Dropped,
    /// One line per dropped item: the reason, the field and the value.
    pub why: Vec<String>,
}

impl Checked {
    pub fn written(&self) -> usize {
        self.kept.len() + self.dropped.total()
    }

    /// Whether the card keeps at least [`MIN_KEPT_PERCENT`] of its items.
    pub fn usable(&self) -> bool {
        self.written() > 0 && self.kept.len() * 100 >= self.written() * MIN_KEPT_PERCENT
    }
}

/// The text a card was checked against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardText {
    pub path: String,
    pub sha256: String,
}

/// Tokens and cost of the call, as the `claude` command reported them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens, cached ones included.
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// A paper card, `rf.paper-card/1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub schema: String,
    pub key: String,
    /// Always `generated`: a card is never a source; a claim cites the paper.
    pub class: String,
    pub text: CardText,
    pub model: String,
    pub created: String,
    pub prompt_sha256: String,
    pub checker: String,
    pub usage: Usage,
    pub items: Vec<Item>,
    /// Items the model wrote.
    pub written: usize,
    /// Items dropped, and why.
    pub dropped: usize,
    pub dropped_by: Dropped,
}

/// Where the card of entry `key` goes.
pub fn path(config: &Config, key: &str) -> PathBuf {
    config.card_dir.join(format!("{key}.json"))
}

/// The raw line range of `quote` in `text`: its first occurrence as one
/// contiguous passage, compared as claimcheck compares a quotation.
pub fn locate(text: &IndexedText, quote: &str) -> Option<[usize; 2]> {
    let (skel, _) = normalize::skeleton(&normalize::normalize_str(quote));
    // claimcheck ignores quotation segments this short.
    if skel.len() < 4 {
        return None;
    }
    let needle: String = skel.iter().collect();
    let at = text.find_bounded(&needle, 0)?;
    let (start, end) = text.skel_span(at, at + skel.len());
    Some([text.line_of(start), text.line_of(end - 1)])
}

/// The numbers of `value` that `quote` lacks, as written in `value`. A number
/// that is part of a name is not a figure and is not checked: one in a word
/// that starts with a letter (`F1`, `GPT-4o`, `Llama-2`, `recall@5`, `top-1`,
/// `Qwen-2.5-14B`), one followed by `-` and a capital (`10-K`), one joined to a
/// capitalised word of at least five letters (`2WikiMultiHopQA`), and a dotted
/// identifier. A word is a run of letters, digits, `.`, `-` and `@`. Figures
/// joined to a word (`7-point`, `5-fold`, `200ms`) or to a unit (`200MB`,
/// `3D`, `50Hz`, `8Gb`, `10Mbps`) are checked.
///
/// A number that the quote lacks is also kept when its [`name_phrase`]
/// (`S&P 500`, `Stage 3`, `Claude 3.5`, `ViT-B/32`, `3-gram`) occurs in
/// `text` as written, apart from white space ([`written_in`]).
pub fn missing_numbers(value: &str, quote: &str, text: &IndexedText) -> Vec<String> {
    let chars: Vec<char> = normalize::normalize_str(value)
        .iter()
        .map(|n| n.c)
        .collect();
    let name = |t: &numbers::NumTok| {
        let first = chars[..t.start]
            .iter()
            .rposition(|c| !in_word(*c))
            .map_or(0, |i| i + 1);
        chars[first].is_alphabetic()
            || (chars.get(t.end) == Some(&'-')
                && chars.get(t.end + 1).is_some_and(|c| c.is_uppercase()))
            || (chars.get(t.end).is_some_and(|c| c.is_uppercase())
                && chars.get(t.end + 1).is_some_and(|c| c.is_lowercase())
                && chars[t.end..]
                    .iter()
                    .take_while(|c| c.is_alphabetic())
                    .count()
                    >= 5)
    };
    let quote = IndexedText::new(quote.to_owned());
    numbers::scan(&chars)
        .into_iter()
        .filter(|t| !(t.dotted || name(t)))
        .filter(|t| !t.lookup_keys().iter().any(|k| quote.has_number(k)))
        .filter(|t| name_phrase(&chars, t).is_none_or(|phrase| !written_in(text, &phrase)))
        .map(|t| chars[t.start..t.end].iter().collect())
        .collect()
}

/// Whether `c` belongs to a word: a letter, a digit, `.`, `-` or `@`.
fn in_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-' | '@')
}

/// Whether `phrase` occurs in `text` as written: found as a quote is, and the
/// passage then has the same characters, case and marks, apart from white
/// space. So `Stage 3` is not found in `stage. 3`, nor `Accuracy 81` in
/// `Accuracy: 81`.
fn written_in(text: &IndexedText, phrase: &str) -> bool {
    let solid = |s: &str| -> Vec<char> {
        normalize::normalize_str(s)
            .iter()
            .map(|n| n.c)
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    let want = solid(phrase);
    let needle = normalize::skeleton_str(phrase);
    let len = needle.chars().count();
    let mut from = 0;
    while let Some(at) = text.find_bounded(&needle, from) {
        let (start, end) = text.skel_span(at, at + len);
        if solid(&text.text[start..end]) == want {
            return true;
        }
        from = at + 1;
    }
    false
}

/// Capitalised words that start a figure, not a name (`In 2020`, `Top 20`).
const NOT_NAMES: &[&str] = &[
    "a", "about", "after", "all", "almost", "an", "and", "around", "at", "before", "below",
    "between", "by", "each", "every", "for", "from", "in", "nearly", "of", "on", "only", "or",
    "over", "since", "than", "the", "to", "top", "under", "until", "up", "with", "within",
];

/// The name that number `t` of `chars` may be part of, as written: the number
/// joined to `-gram` (`3-gram`), or a name word and the number (`S&P 500`,
/// `Stage 3`, `Claude 3.5`, `Qwen 2.5 7B`, `ViT-B/32`, `DEF 14A`). The name
/// word comes just before the number, joined to it by a mark or one or more
/// spaces, past at most one other number. It holds a letter and no digit, and
/// a capital, unless it is a common word (`In`, `Top`), or a mark `&`, `/`,
/// `+` or `_` (`S&P`). A lowercase word (`top 20`, `reaches 92.4`,
/// `zero-shot 45`), a word that ends a clause (`Accuracy: 92.4`) and a word
/// glued by another mark (`B=256`, `US$5`) are not names. Nor is a percentage
/// or a decimal with two or more digits before the point (`BLEU 28.4`): a
/// name's number is a whole number or a version (`3.5`, `2.0`).
fn name_phrase(chars: &[char], t: &numbers::NumTok) -> Option<String> {
    let whole = chars[t.start..t.end]
        .iter()
        .take_while(|c| **c != '.')
        .filter(|c| c.is_ascii_digit())
        .count();
    if t.percent || (t.decimal && whole > 1) {
        return None;
    }
    let mut end = t.end + chars[t.end..].iter().take_while(|c| in_word(**c)).count();
    while end > t.end && matches!(chars[end - 1], '.' | '-') {
        end -= 1;
    }
    let phrase = |start: usize| Some(chars[start..end].iter().collect());
    if chars[t.end..].starts_with(&['-', 'g', 'r', 'a', 'm']) {
        return phrase(t.start);
    }
    let gap = |c: char| c == ' ' || c == '\t';
    let opening = |c: char| matches!(c, '(' | '[' | '{' | '"' | '\'' | '“' | '‘');
    let mut word_end = t.start;
    let mut skipped = false;
    let mut word_start = loop {
        let run = chars[..word_end]
            .iter()
            .rev()
            .take_while(|c| !c.is_whitespace())
            .count();
        if run == 0 {
            // The number or a skipped number stands alone: step over the
            // spaces before it, on the same line.
            let spaces = chars[..word_end]
                .iter()
                .rev()
                .take_while(|c| gap(**c))
                .count();
            if spaces == 0 {
                return None;
            }
            word_end -= spaces;
            continue;
        }
        let start = word_end - run;
        let word = &chars[start..word_end];
        if word.iter().any(|c| c.is_alphabetic()) {
            break start;
        }
        // One number such as `2.5` in `Qwen 2.5 7B` may stand between.
        if skipped || word_end == t.start || !word.iter().all(|c| c.is_ascii_digit() || *c == '.') {
            return None;
        }
        skipped = true;
        word_end = start;
    };
    while word_start < word_end && opening(chars[word_start]) {
        word_start += 1;
    }
    let word = &chars[word_start..word_end];
    let last = *word.last()?;
    // A name word glued to the number ends in `/`, `(`, `-` or `_` (`ViT-B/`,
    // `Q(`); one apart from it may not end a clause (`Accuracy:`).
    let glued = word_end == t.start;
    if (glued && !matches!(last, '/' | '(' | '-' | '_')) || (!glued && !last.is_alphanumeric()) {
        return None;
    }
    if word.iter().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let lower: String = word.iter().flat_map(|c| c.to_lowercase()).collect();
    let capital = word.iter().any(|c| c.is_uppercase()) && !NOT_NAMES.contains(&lower.as_str());
    let inner = &word[..word.len() - usize::from(glued)];
    let mark = inner
        .iter()
        .skip(1)
        .any(|c| matches!(c, '&' | '/' | '+' | '_'));
    if capital || mark {
        phrase(word_start)
    } else {
        None
    }
}

/// The words of `s`: each CJK character is a word, as word processors count
/// them, and the rest is split at white space and CJK punctuation.
fn words(s: &str) -> usize {
    let cjk = |c: char| matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF);
    // CJK punctuation (`、`, `。`, `「`) separates words and is none itself.
    let gap = |c: char| cjk(c) || matches!(c as u32, 0x3000..=0x303F);
    let other: String = s.chars().map(|c| if gap(c) { ' ' } else { c }).collect();
    s.chars().filter(|c| cjk(*c)).count() + other.split_whitespace().count()
}

/// Checks each written item against `text` (see the module docs).
pub fn check(text: &str, items: &[Written]) -> Checked {
    let index = IndexedText::new(text.to_owned());
    let mut out = Checked::default();
    for item in items {
        let d = &mut out.dropped;
        let reason = if !FIELDS.contains(&item.field.as_str()) {
            d.field += 1;
            "unknown field".to_owned()
        } else if words(&item.quote) < MIN_WORDS {
            d.short += 1;
            format!("quote under {MIN_WORDS} words")
        } else if let Some(lines) = locate(&index, &item.quote) {
            let missing = missing_numbers(&item.value, &item.quote, &index);
            if missing.is_empty() {
                out.kept.push(Item {
                    field: item.field.clone(),
                    value: item.value.clone(),
                    quote: item.quote.clone(),
                    lines,
                });
                continue;
            }
            d.number += 1;
            format!("number not in quote: {}", missing.join(", "))
        } else {
            d.not_found += 1;
            "quote not found".to_owned()
        };
        out.why.push(format!(
            "dropped ({reason}): {}: {}",
            item.field, item.value
        ));
    }
    out
}

/// The items and usage of a `claude -p --output-format json` reply for
/// `model`. The usage is that of `model` alone when the reply breaks it down
/// by model, so a helper model's tokens are not counted.
pub fn parse_reply(bytes: &[u8], model: &str) -> Result<(Vec<Written>, Usage), String> {
    let reply: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("the reply is not JSON: {e}"))?;
    if reply.get("is_error").and_then(Value::as_bool) == Some(true) {
        let why = reply
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("no reason given");
        return Err(format!("the call failed: {why}"));
    }
    let items = reply
        .pointer("/structured_output/items")
        .ok_or("the reply has no structured_output.items")?;
    let items: Vec<Written> = serde_json::from_value(items.clone())
        .map_err(|e| format!("the reply's items do not follow the schema: {e}"))?;
    let n = |v: &Value, name: &str| v.get(name).and_then(Value::as_u64).unwrap_or(0);
    let usage = match reply.pointer("/modelUsage").and_then(|m| m.get(model)) {
        Some(u) => Usage {
            input_tokens: n(u, "inputTokens")
                + n(u, "cacheCreationInputTokens")
                + n(u, "cacheReadInputTokens"),
            output_tokens: n(u, "outputTokens"),
            cost_usd: u.get("costUSD").and_then(Value::as_f64).map(round4),
        },
        None => {
            let u = reply.get("usage").cloned().unwrap_or_default();
            Usage {
                input_tokens: n(&u, "input_tokens")
                    + n(&u, "cache_creation_input_tokens")
                    + n(&u, "cache_read_input_tokens"),
                output_tokens: n(&u, "output_tokens"),
                cost_usd: reply
                    .get("total_cost_usd")
                    .and_then(Value::as_f64)
                    .map(round4),
            }
        }
    };
    Ok((items, usage))
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Runs the card call: `config.claude` with the paper text on standard input,
/// in an empty directory, with no tools, settings sources, MCP servers or
/// slash commands. Gives the raw reply.
fn call(config: &Config, text: Vec<u8>) -> Result<Vec<u8>, String> {
    let [program, args @ ..] = config.claude.as_slice() else {
        return Err("[papers] claude is empty; it names the command that runs Claude Code".into());
    };
    let at = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    // Outside the working directory, which stays empty. Named by its sha256
    // and replaced by a rename, so a parallel call never reads it half-written.
    let sha = http::sha256_hex(PROMPT.as_bytes());
    let prompt = config.state.join(format!("card-prompt-{}.md", &sha[..12]));
    let work = config.state.join("card-cwd");
    std::fs::create_dir_all(&work).map_err(|e| at(&work, e))?;
    if std::fs::read_dir(&work)
        .map_err(|e| at(&work, e))?
        .next()
        .is_some()
    {
        return Err(format!(
            "{} is not empty; the card call runs in an empty directory",
            work.display()
        ));
    }
    if !prompt.exists() {
        let tmp = prompt.with_extension(format!("md.{}.tmp", std::process::id()));
        std::fs::write(&tmp, PROMPT).map_err(|e| at(&tmp, e))?;
        std::fs::rename(&tmp, &prompt).map_err(|e| at(&prompt, e))?;
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .args(["-p", "--model", &config.card_model, "--system-prompt-file"])
        .arg(&prompt)
        .args([
            "--tools",
            "",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--output-format",
            "json",
            "--json-schema",
            REPLY_SCHEMA.trim(),
        ])
        .current_dir(&work);
    pdf::run_with_input(program, command, Some(text), TIMEOUT, MAX_REPLY)
}

/// Saves a raw reply under `<state>/card-replies/`, named by key and time.
fn keep_reply(config: &Config, key: &str, created: &str, reply: &[u8]) -> Result<PathBuf, String> {
    let dir = config.state.join("card-replies");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp: String = created
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let path = dir.join(format!("{key}-{stamp}.json"));
    std::fs::write(&path, reply).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The recorded text of `entry` a card is made from: its Docling text with
/// `docling`, else its first other text.
fn text_of(entry: &registry::Reference, docling: bool) -> Result<&registry::Text, String> {
    entry
        .texts
        .iter()
        .find(|t| (t.extraction() == Ok(Some(Extraction::Docling))) == docling)
        .ok_or_else(|| match docling {
            true => format!(
                "entry {} has no Docling text; run papers read --key {} --extraction docling --record",
                entry.key, entry.key
            ),
            false => format!(
                "entry {} has no local text; run papers read --key {} --record",
                entry.key, entry.key
            ),
        })
}

/// `papers card`: makes and stores the card of entry `key`, or only checks
/// the saved raw reply `reply` without calling Claude or storing a card.
/// Returns false when the card keeps too few items to be stored.
pub fn run(
    config: &Config,
    key: &str,
    docling: bool,
    force: bool,
    reply: Option<&Path>,
) -> Result<bool, String> {
    if !config.cards {
        return Err(
            "paper cards are off: set `cards = true` under [papers] in research.toml (DR043)"
                .into(),
        );
    }
    let records = registry::load(&config.registry)?
        .ok_or_else(|| format!("{}: no registry", config.display(&config.registry)))?;
    let entry = records
        .iter()
        .find(|r| r.key == key)
        .ok_or_else(|| format!("key {key} is not in {}", config.display(&config.registry)))?;
    let text = text_of(entry, docling)?;
    let file = config.root.join(&text.path);
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sha256 = http::sha256_hex(&bytes);
    if sha256 != text.sha256 {
        return Err(format!(
            "{} has sha256 {}, not the {} the registry records; read it again before making its card",
            text.path,
            &sha256[..12],
            &text.sha256[..text.sha256.len().min(12)]
        ));
    }
    let content =
        String::from_utf8(bytes.clone()).map_err(|_| format!("{} is not UTF-8", text.path))?;
    let card_path = path(config, key);
    let prompt_sha256 = http::sha256_hex(PROMPT.as_bytes());
    let shown = config.display(&card_path);
    if !force
        && reply.is_none()
        && let Some(old) = std::fs::read(&card_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Card>(&b).ok())
        && old.text.sha256 == sha256
        && old.prompt_sha256 == prompt_sha256
        && old.model == config.card_model
    {
        println!(
            "already have {shown} (same text, prompt and model); not calling Claude (--force to make it again)"
        );
        return Ok(true);
    }
    let created = finding::now();
    let raw = match reply {
        Some(saved) => std::fs::read(saved).map_err(|e| format!("{}: {e}", saved.display()))?,
        None => {
            println!(
                "calling {} for key {key} ({} on {})",
                config.claude.join(" "),
                config.card_model,
                text.path
            );
            let raw = call(config, bytes)?;
            let kept = keep_reply(config, key, &created, &raw)?;
            println!("reply: {}", config.display(&kept));
            raw
        }
    };
    let (items, usage) = parse_reply(&raw, &config.card_model)?;
    let checked = check(&content, &items);
    let d = &checked.dropped;
    let summary = format!(
        "kept {} of {} items; dropped {} (quote under {MIN_WORDS} words {}, quote not found {}, number not in quote {}, unknown field {}); tokens in {} out {}{}",
        checked.kept.len(),
        checked.written(),
        d.total(),
        d.short,
        d.not_found,
        d.number,
        d.field,
        usage.input_tokens,
        usage.output_tokens,
        usage
            .cost_usd
            .map(|c| format!(", {c} USD"))
            .unwrap_or_default()
    );
    for line in &checked.why {
        println!("{line}");
    }
    if !checked.usable() {
        println!("{summary}");
        println!("{}", refusal(&checked.dropped, &text.path, key));
        return Ok(false);
    }
    if reply.is_some() {
        // A saved reply does not record its prompt or the model it was asked
        // for, so a card stored from it could claim the wrong provenance.
        println!("{summary}");
        println!("not stored: a saved reply is only checked (--reply)");
        return Ok(true);
    }
    let card = Card {
        schema: SCHEMA.to_owned(),
        key: key.to_owned(),
        class: "generated".to_owned(),
        text: CardText {
            path: text.path.clone(),
            sha256,
        },
        model: config.card_model.clone(),
        created,
        prompt_sha256,
        checker: CHECKER.to_owned(),
        usage,
        written: checked.written(),
        dropped: checked.dropped.total(),
        dropped_by: checked.dropped,
        items: checked.kept,
    };
    std::fs::create_dir_all(&config.card_dir)
        .map_err(|e| format!("{}: {e}", config.card_dir.display()))?;
    let json = serde_json::to_string_pretty(&card).expect("cards serialize");
    derivative::write(&card_path, format!("{json}\n").as_bytes())?;
    println!("{summary}");
    println!("card: {shown}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "Abstract\nWe train a parser on the Penn Treebank and reach an F1\nof 92.4 on section 23, which beats the baseline by 1.2 points.\nThe method fails on long sentences.\n";

    /// The numbers of `value` that `quote` lacks, against the text [`TEXT`].
    fn missing(value: &str, quote: &str) -> Vec<String> {
        missing_numbers(value, quote, &IndexedText::new(TEXT.to_owned()))
    }

    fn item(field: &str, value: &str, quote: &str) -> Written {
        Written {
            field: field.into(),
            value: value.into(),
            quote: quote.into(),
        }
    }

    #[test]
    fn a_found_quote_is_kept_with_its_raw_lines() {
        let checked = check(
            TEXT,
            &[item(
                "result",
                "The parser reaches an F1 of 92.4 on section 23.",
                "train a parser on the Penn Treebank and reach an F1 of 92.4 on section 23",
            )],
        );
        assert_eq!(checked.dropped, Dropped::default());
        assert_eq!(checked.kept[0].lines, [2, 3]);
    }

    #[test]
    fn a_value_with_a_number_absent_from_its_quote_is_dropped_and_counted() {
        let quote = "We train a parser on the Penn Treebank and reach an F1";
        let checked = check(
            TEXT,
            &[
                // 92.4 is in the text but not in this quote.
                item("result", "The parser reaches 92.4 F1.", quote),
                item("method", "A parser trained on the Penn Treebank.", quote),
            ],
        );
        assert_eq!(checked.kept.len(), 1);
        assert_eq!(checked.kept[0].field, "method");
        assert_eq!(
            checked.dropped,
            Dropped {
                number: 1,
                ..Dropped::default()
            }
        );
        assert_eq!(
            checked.why,
            ["dropped (number not in quote: 92.4): result: The parser reaches 92.4 F1."]
        );
        assert_eq!(
            missing("up 1,200 (12.0%)", "rose by 1200 or 12 %"),
            Vec::<String>::new()
        );
        assert_eq!(missing("reaches 92.4 F1", quote), vec!["92.4"]);
        // A name with a digit is not a figure.
        assert_eq!(
            missing("uses F1 and H133", "no numbers here"),
            Vec::<String>::new()
        );
        assert_eq!(
            missing(
                "recall@5 with Llama-2 and GPT-4o on 10-K filings and Qwen-2.5-14B",
                "no numbers here"
            ),
            Vec::<String>::new()
        );
        // A name may start with a digit joined to a capitalised word.
        assert_eq!(
            missing("on 2WikiMultiHopQA and HotpotQA", "on HotpotQA and MuSiQue"),
            Vec::<String>::new()
        );
        assert_eq!(
            missing("uses 200MB in 3D at 50Hz, 8Gb, 10Mbps", "no numbers here"),
            vec!["200", "3", "50", "8", "10"]
        );
        // A figure beside such a name is still checked.
        assert_eq!(
            missing("top-1 accuracy of 57.04 at k = 3", "top-1 accuracy"),
            vec!["57.04", "3"]
        );
        // A figure joined to a word is checked, and found when the quote has it.
        assert_eq!(
            missing("a 7-point gain, 5-fold, 200ms", "no numbers here"),
            vec!["7", "5", "200"]
        );
        assert_eq!(
            missing("a 7-point gain in 200ms", "gains 7 points at 200ms"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_spaced_name_in_the_paper_text_is_not_a_figure() {
        let text = IndexedText::new(
            "Firms of the S&P 500 index. In Stage 3 we ask Claude 3.5 Sonnet,\n\
             Qwen 2.5 7B and ViT-B/32 (DEF 14A forms), and count 3-gram overlap.\n\
             The parser reaches 92.4 F1 with the top 20 results in 2020.\n\
             Accuracy 81 overall.\n"
                .to_owned(),
        );
        let quote = "no numbers here";
        let names =
            "S&P 500 firms; (Stage 3) uses Claude 3.5, Qwen 2.5 7B, ViT-B/32, DEF 14A and 3-gram";
        assert_eq!(missing_numbers(names, quote, &text), Vec::<String>::new());
        // A name the paper text lacks is checked.
        assert_eq!(
            missing_numbers("Stage 4 and Claude 3", quote, &text),
            vec!["4", "3"]
        );
        // A figure after a lowercase or common word, or a clause, is checked
        // even when the paper text has the phrase.
        assert_eq!(
            missing_numbers(
                "reaches 92.4 F1 with the top 20 results, Top 20 results, In 2020, Accuracy: 81",
                quote,
                &text
            ),
            vec!["92.4", "20", "20", "2020", "81"]
        );
        assert_eq!(
            missing_numbers("a 7-point gain, 5-fold, 200ms", quote, &text),
            vec!["7", "5", "200"]
        );
        // Figures that look like names are checked: a metric's score, a
        // percentage, a setting, an assignment or a price.
        let text = IndexedText::new(
            "We reach BLEU 28.4 and BLEU 28 with Recall 90%.\n\
             The zero-shot 45 score uses N=128 and US$5 per run.\n\
             Accuracy: 81 on the final stage. 3 runs failed.\n"
                .to_owned(),
        );
        assert_eq!(
            missing_numbers(
                "BLEU 28.4, Recall 90%, zero-shot 45, N=128, US$5, Accuracy 81, Stage 3",
                quote,
                &text
            ),
            vec!["28.4", "90", "45", "128", "5", "81", "3"]
        );
        // A whole number after a name is kept only as the text writes it.
        assert_eq!(
            missing_numbers("BLEU 28", quote, &text),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cjk_character_counts_as_a_word() {
        assert_eq!(words("書類管理番号は"), 7);
        assert_eq!(words("先頭 8 文字です。"), 7);
        assert_eq!(words("eight plain words in this short quote here"), 8);
    }

    #[test]
    fn a_refused_card_names_its_main_cause() {
        let numbers = Dropped {
            number: 4,
            not_found: 1,
            ..Dropped::default()
        };
        let text = refusal(&numbers, "sources/text/4.txt", "4");
        assert!(text.contains("number not in quote 4"), "{text}");
        assert!(!text.contains("extraction fault"), "{text}");
        let quotes = Dropped {
            number: 2,
            not_found: 2,
            ..Dropped::default()
        };
        let text = refusal(&quotes, "sources/text/4.txt", "4");
        assert!(
            text.contains("suspected text-extraction fault in sources/text/4.txt"),
            "{text}"
        );
    }

    #[test]
    fn short_unknown_and_missing_quotes_are_dropped_and_counted() {
        let checked = check(
            TEXT,
            &[
                item("result", "Beats it.", "beats the baseline by 1.2 points"),
                item(
                    "summary",
                    "A parser.",
                    "We train a parser on the Penn Treebank and reach",
                ),
                item(
                    "limit",
                    "Fails on long texts.",
                    "The method fails on very long documents and sentences alike",
                ),
                // Cuts the number 92.4: not a bounded match.
                item(
                    "result",
                    "Reaches 92.",
                    "on the Penn Treebank and reach an F1 of 92",
                ),
            ],
        );
        assert!(checked.kept.is_empty());
        assert_eq!(
            checked.dropped,
            Dropped {
                field: 1,
                short: 1,
                not_found: 2,
                number: 0
            }
        );
    }

    #[test]
    fn the_gate_needs_eighty_percent_kept() {
        let with = |kept: usize, dropped: usize| Checked {
            kept: vec![
                Item {
                    field: "task".into(),
                    value: String::new(),
                    quote: String::new(),
                    lines: [1, 1]
                };
                kept
            ],
            dropped: Dropped {
                not_found: dropped,
                ..Dropped::default()
            },
            why: Vec::new(),
        };
        assert!(with(8, 2).usable());
        assert!(with(12, 3).usable());
        assert!(!with(11, 3).usable());
        assert!(!with(1, 13).usable());
        assert!(!with(0, 0).usable());
    }

    #[test]
    fn the_reply_gives_items_and_the_model_s_own_usage() {
        let reply = br#"{"is_error":false,"structured_output":{"items":[{"field":"task","value":"v","quote":"q"}]},
            "usage":{"input_tokens":9,"output_tokens":9},"total_cost_usd":1.0,
            "modelUsage":{"claude-haiku-5-5":{"inputTokens":3,"cacheCreationInputTokens":20,"cacheReadInputTokens":100,"outputTokens":50,"costUSD":0.123456},
                          "claude-fable-5-2":{"inputTokens":7,"outputTokens":7,"costUSD":0.001}}}"#;
        let (items, usage) = parse_reply(reply, "claude-haiku-5-5").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            usage,
            Usage {
                input_tokens: 123,
                output_tokens: 50,
                cost_usd: Some(0.1235)
            }
        );
        let e = parse_reply(br#"{"is_error":true,"result":"usage limit"}"#, "m").unwrap_err();
        assert!(e.contains("usage limit"), "{e}");
    }
}
