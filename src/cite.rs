//! `papers cite`: number the citations in Markdown documents by first
//! appearance, rewrite them, and print the reference list in a style.
//!
//! Citations are found with regular expressions that each capture a `key`
//! group: a registry key, or a citation key from [`crate::bib`]. Fenced code
//! blocks and inline code spans are skipped.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use regex::Regex;

use crate::bib::{self, Kind, Name};
use crate::registry::Reference;

/// `[12](references.md#reference-12)` and `[@key]`.
pub const DEFAULT_PATTERNS: [&str; 2] = [
    r"\[\d+\]\([^()\s]*#reference-(?P<key>[A-Za-z0-9._-]+)\)",
    r"\[@(?P<key>[A-Za-z0-9._:-]+)\]",
];
/// Each citation becomes its number in brackets, as IPSJ prints them.
pub const DEFAULT_REPLACE: &str = "[{n}]";

/// The IPSJ Journal author guidance the `ipsj` style follows, and when it was read.
pub const IPSJ_GUIDE: &str = "https://www.ipsj.or.jp/journal/submit/ronbun_j_prms.html";
pub const IPSJ_ACCESSED: &str = "2026-11-07";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Style {
    /// IPSJ Journal (情報処理学会論文誌) reference format.
    Ipsj,
}

pub struct Patterns(Vec<Regex>);

impl Patterns {
    /// Compiles `sources`, or the defaults when it is empty.
    pub fn new(sources: &[String]) -> Result<Patterns, String> {
        let defaults: Vec<String> = DEFAULT_PATTERNS.iter().map(|s| (*s).to_owned()).collect();
        let sources = if sources.is_empty() {
            &defaults
        } else {
            sources
        };
        sources
            .iter()
            .map(|s| {
                let re = Regex::new(s).map_err(|e| format!("citation pattern {s:?}: {e}"))?;
                if !re.capture_names().any(|n| n == Some("key")) {
                    return Err(format!("citation pattern {s:?} has no (?P<key>…) group"));
                }
                Ok(re)
            })
            .collect::<Result<_, _>>()
            .map(Patterns)
    }
}

/// One citation in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    /// Byte range of the whole match.
    pub span: Range<usize>,
    pub key: String,
    /// 1-based line and column (in characters).
    pub line: usize,
    pub column: usize,
}

/// Byte ranges of inline code spans: a run of backticks up to the next run of
/// the same length.
fn code_spans(line: &str) -> Vec<Range<usize>> {
    let b = line.as_bytes();
    let run = |i: usize| b[i..].iter().take_while(|&&c| c == b'`').count();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run(i);
        let mut j = i + n;
        let mut end = None;
        while j < b.len() {
            if b[j] == b'`' {
                let m = run(j);
                if m == n {
                    end = Some(j + m);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match end {
            Some(e) => {
                spans.push(i..e);
                i = e;
            }
            None => i += n,
        }
    }
    spans
}

/// The fence marker opening or closing a fenced code block, if `line` is one.
fn fence(line: &str) -> Option<(u8, usize)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let c = *t.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|&x| x == c).count();
    (n >= 3).then_some((c, n))
}

/// Citations in document order; where patterns overlap, the earliest (then
/// longest) match wins.
pub fn scan(text: &str, patterns: &Patterns) -> Vec<Citation> {
    let mut out = Vec::new();
    let mut open: Option<(u8, usize)> = None;
    let mut offset = 0;
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += line.len();
        if let Some(f) = fence(line) {
            match open {
                None => open = Some(f),
                Some((c, len)) if f.0 == c && f.1 >= len => open = None,
                Some(_) => {}
            }
            continue;
        }
        if open.is_some() {
            continue;
        }
        let code = code_spans(line);
        let mut found: Vec<(Range<usize>, String)> = Vec::new();
        for re in &patterns.0 {
            for caps in re.captures_iter(line) {
                let m = caps.get(0).expect("group 0 always matches");
                let Some(key) = caps.name("key") else {
                    continue;
                };
                let r = m.range();
                if code.iter().any(|c| c.start < r.end && r.start < c.end) {
                    continue;
                }
                found.push((r, key.as_str().to_owned()));
            }
        }
        found.sort_by_key(|(r, _)| (r.start, std::cmp::Reverse(r.end)));
        let mut last_end = 0;
        for (r, key) in found {
            if r.start < last_end {
                continue;
            }
            last_end = r.end;
            out.push(Citation {
                line: n + 1,
                column: line[..r.start].chars().count() + 1,
                span: start + r.start..start + r.end,
                key,
            });
        }
    }
    out
}

/// Looks citation keys up in the registry: registry keys first, then citation
/// keys; duplicates resolve to their canonical entry.
pub struct Resolver<'a> {
    by_key: HashMap<&'a str, &'a Reference>,
    by_citekey: HashMap<String, String>,
    pub citekeys: BTreeMap<String, String>,
}

impl<'a> Resolver<'a> {
    /// Fails when two entries pin the same `citekey`: a citation of it would be ambiguous.
    pub fn new(records: &'a [Reference]) -> Result<Resolver<'a>, String> {
        let citekeys = bib::citation_keys(records);
        let mut by_citekey: HashMap<String, String> = HashMap::new();
        for (key, citekey) in &citekeys {
            if let Some(other) = by_citekey.insert(citekey.clone(), key.clone()) {
                let mut pair = [other, key.clone()];
                pair.sort_by(|a, b| crate::registry::key_order(a, b));
                return Err(format!(
                    "entries {} and {} both pin citekey {citekey}; give one of them another",
                    pair[0], pair[1]
                ));
            }
        }
        Ok(Resolver {
            by_key: records.iter().map(|r| (r.key.as_str(), r)).collect(),
            by_citekey,
            citekeys,
        })
    }

    /// The canonical entry for a registry key or citation key; `Ok(None)` when
    /// neither is known.
    pub fn resolve(&self, key: &str) -> Result<Option<&'a Reference>, String> {
        let r = match self.by_key.get(key) {
            Some(r) => *r,
            None => match self.by_citekey.get(key) {
                Some(k) => self.by_key[k.as_str()],
                None => return Ok(None),
            },
        };
        self.canonical(r).map(Some)
    }

    /// Follows `duplicate_of` to the entry that stands for `r`, however many
    /// hops; a cycle or a missing target is an error.
    pub fn canonical(&self, r: &'a Reference) -> Result<&'a Reference, String> {
        let mut path = vec![r.key.as_str()];
        let mut r = r;
        while let Some(target) = &r.duplicate_of {
            if path.contains(&target.as_str()) {
                path.push(target);
                return Err(format!("duplicate_of cycle: {}", path.join(" → ")));
            }
            r = self.by_key.get(target.as_str()).copied().ok_or_else(|| {
                format!(
                    "entry {} is a duplicate of {target}, which the registry lacks",
                    r.key
                )
            })?;
            path.push(&r.key);
        }
        Ok(r)
    }

    pub fn citekey(&self, r: &Reference) -> &str {
        self.citekeys.get(&r.key).map_or("", String::as_str)
    }
}

/// Registry keys in order of first citation.
#[derive(Debug, Default)]
pub struct Numbering {
    pub order: Vec<String>,
    index: HashMap<String, usize>,
}

impl Numbering {
    /// The 1-based number of `key`, assigning the next one on first sight.
    pub fn number(&mut self, key: &str) -> usize {
        if let Some(&n) = self.index.get(key) {
            return n;
        }
        self.order.push(key.to_owned());
        let n = self.order.len();
        self.index.insert(key.to_owned(), n);
        n
    }
}

/// Expands `{n}`, `{key}`, and `{citekey}` in a replacement template.
pub fn expand(template: &str, n: usize, key: &str, citekey: &str) -> String {
    template
        .replace("{n}", &n.to_string())
        .replace("{key}", key)
        .replace("{citekey}", citekey)
}

/// `text` with each span replaced; spans are in order and do not overlap.
pub fn rewrite(text: &str, replacements: &[(Range<usize>, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (span, with) in replacements {
        out.push_str(&text[at..span.start]);
        out.push_str(with);
        at = span.end;
    }
    out.push_str(&text[at..]);
    out
}

fn is_japanese(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x3040..=0x30FF   // hiragana, katakana
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified ideographs
            | 0xFF66..=0xFF9F) // half-width katakana
    })
}

/// Punctuation for English or Japanese entries (IPSJ appendix: half-width with a
/// space after commas in English, full-width in Japanese).
struct Punct {
    comma: &'static str,
    colon: &'static str,
    period: &'static str,
    and: &'static str,
    et_al: &'static str,
    online: &'static str,
    open: &'static str,
    close: &'static str,
    from: &'static str,
    accessed: &'static str,
}

const EN: Punct = Punct {
    comma: ", ",
    colon: ": ",
    period: ".",
    and: " and ",
    et_al: ", et al.",
    online: " (online)",
    open: " (",
    close: ")",
    from: "available from ",
    accessed: "(accessed ",
};
const JA: Punct = Punct {
    comma: "，",
    colon: "：",
    period: "．",
    and: "，",
    et_al: "ほか",
    online: "（オンライン）",
    open: "（",
    close: "）",
    from: "入手先",
    accessed: "（参照",
};

/// `R.L.` from `Ronald Linn`, `J.-P.` from `Jean-Paul`; existing initials stay.
fn initials(given: &str) -> String {
    let mut out = String::new();
    for token in given.split_whitespace() {
        for part in token.split('.').filter(|p| !p.is_empty()) {
            let bits: Vec<String> = part
                .split('-')
                .filter_map(|p| p.chars().next())
                .map(|c| format!("{}.", c.to_uppercase()))
                .collect();
            out += &bits.join("-");
        }
    }
    out
}

/// IPSJ name form: `Family, I.` for Latin names; Japanese and organization
/// names as written.
fn ipsj_name(name: &str) -> String {
    if is_japanese(name) {
        return name.to_owned();
    }
    match bib::split_name(name) {
        Name::Person { given, family } => match family.split_once(", ") {
            Some((f, suffix)) => format!("{f}, {}, {suffix}", initials(&given)),
            None => format!("{family}, {}", initials(&given)),
        },
        Name::Literal(l) => l,
    }
}

/// Up to three authors in full; four or more (or a truncated list) as the
/// first three and "et al." / ほか, which the guidance allows.
fn ipsj_authors(r: &Reference, p: &Punct) -> Option<String> {
    let names: Vec<String> = r.authors.as_ref()?.iter().map(|a| ipsj_name(a)).collect();
    if names.is_empty() {
        return None;
    }
    if names.len() > 3 || r.authors_truncated {
        let shown: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
        return Some(format!("{}{}", shown.join(p.comma), p.et_al));
    }
    Some(match names.as_slice() {
        [one] => one.clone(),
        [init @ .., last] => format!("{}{}{last}", init.join(p.comma), p.and),
        [] => unreachable!("checked non-empty"),
    })
}

/// `Proc. X` for a proceedings venue.
fn proc_venue(venue: &str) -> String {
    let lower = venue.to_lowercase();
    if lower.starts_with("proc.") {
        return venue.to_owned();
    }
    for prefix in ["proceedings of the ", "proceedings of ", "proceedings "] {
        if lower.starts_with(prefix) {
            return format!("Proc. {}", &venue[prefix.len()..]);
        }
    }
    format!("Proc. {venue}")
}

/// One reference in the IPSJ format, without its `[n]` label. The registry has
/// no volume, issue, or page fields, so those are never printed.
pub fn ipsj(r: &Reference) -> String {
    let title = r.title.as_deref().unwrap_or(&r.key);
    let p = if is_japanese(title) { &JA } else { &EN };
    let mut s = match ipsj_authors(r, p) {
        Some(a) => format!("{a}{}{title}", p.colon),
        None => title.to_owned(),
    };
    let year = |s: &mut String| {
        if let Some(y) = r.year {
            *s += &format!("{}{y}{}", p.open, p.close);
        }
    };
    let doi = |s: &mut String, doi: &str| {
        *s += p.online;
        *s += &format!("{}DOI{}{doi}", p.comma, p.colon);
    };
    let venue = r.venue.as_deref().filter(|v| !v.is_empty());
    let kind = bib::kind(r);
    match kind {
        Kind::Web | Kind::Software | Kind::Dataset | Kind::Standard | Kind::Other
            if r.ids.url.is_some() && (kind != Kind::Other || r.ids.doi.is_none()) =>
        {
            if let Some(v) = venue.filter(|_| kind != Kind::Other) {
                s += p.comma;
                s += v;
            }
            s += p.online;
            s += p.comma;
            s += &format!("{}<{}>", p.from, r.ids.url.as_deref().unwrap_or_default());
            match &r.accessed {
                Some(d) => s += &format!("{}{d}{}", p.accessed, p.close),
                None => year(&mut s),
            }
        }
        Kind::Preprint => {
            let id = r.ids.arxiv.as_deref().unwrap_or_default();
            s += &format!(
                "{}arXiv:{id}{}",
                p.comma,
                r.version.as_deref().unwrap_or("")
            );
            doi(&mut s, &format!("10.48550/arXiv.{id}"));
            year(&mut s);
        }
        _ => {
            if let Some(v) = venue.filter(|_| kind != Kind::Other) {
                s += p.comma;
                s += &if kind == Kind::Conference {
                    proc_venue(v)
                } else {
                    v.to_owned()
                };
            }
            if let Some(d) = &r.ids.doi {
                doi(&mut s, d);
            }
            year(&mut s);
        }
    }
    s += p.period;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(key: &str, authors: &[&str], year: Option<i64>, title: &str) -> Reference {
        let mut r = Reference::new(key.into());
        r.authors = Some(authors.iter().map(|a| (*a).to_owned()).collect());
        r.year = year;
        r.title = Some(title.into());
        r
    }

    #[test]
    fn records_the_ipsj_guidance_and_access_date() {
        assert_eq!(IPSJ_ACCESSED, "2026-11-07");
        let readme = include_str!("../README.md");
        assert!(readme.contains(IPSJ_GUIDE) && readme.contains(IPSJ_ACCESSED));
    }

    #[test]
    fn scans_citations_outside_code() {
        let p = Patterns::new(&[]).unwrap();
        let text = "See [12](references.md#reference-12) and [@lee2026widgets].\n\
                    `[@inline]` is code.\n\
                    ```\n[@fenced]\n```\n\
                    Again [3](../references.md#reference-3)[@12].\n";
        let found = scan(text, &p);
        let keys: Vec<&str> = found.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["12", "lee2026widgets", "3", "12"]);
        assert_eq!((found[1].line, found[1].column), (1, 42));
        assert_eq!(
            &text[found[2].span.clone()],
            "[3](../references.md#reference-3)"
        );
        assert_eq!(found[3].line, 6);
    }

    #[test]
    fn custom_patterns_need_a_key_group() {
        assert!(Patterns::new(&[r"\\cite\{\w+\}".into()]).is_err());
        assert!(Patterns::new(&["(".into()]).is_err());
        let p = Patterns::new(&[r"\\cite\{(?P<key>\w+)\}".into()]).unwrap();
        assert_eq!(scan(r"as \cite{x1} shows", &p)[0].key, "x1");
    }

    #[test]
    fn code_spans_match_backtick_runs() {
        let spans = |s: &str| -> Vec<(usize, usize)> {
            code_spans(s)
                .into_iter()
                .map(|r| (r.start, r.end))
                .collect()
        };
        assert_eq!(spans("a `b` c"), [(2, 5)]);
        assert_eq!(spans("``a ` b`` `c"), [(0, 9)]);
        assert!(code_spans("unclosed ` tick").is_empty());
    }

    #[test]
    fn numbers_by_first_appearance_and_resolves_keys() {
        let mut a = rec("5", &["Ann Lee"], Some(2026), "Widgets");
        a.ids.doi = Some("10.1234/w".into());
        let mut dup = rec("9", &["Ann Lee"], Some(2026), "Widgets");
        dup.duplicate_of = Some("5".into());
        let b = rec("2", &["Bo Chen"], Some(2025), "Gadgets");
        let records = [a, dup, b];
        let res = Resolver::new(&records).unwrap();
        assert_eq!(res.resolve("9").unwrap().unwrap().key, "5");
        assert_eq!(res.resolve("chen2025gadgets").unwrap().unwrap().key, "2");
        assert!(res.resolve("nope").unwrap().is_none());
        let mut n = Numbering::default();
        let got: Vec<usize> = ["2", "5", "2", "5"].iter().map(|k| n.number(k)).collect();
        assert_eq!(got, [1, 2, 1, 2]);
        assert_eq!(
            expand("[{n}](#r-{key}) {citekey}", 3, "5", "lee"),
            "[3](#r-5) lee"
        );
        assert_eq!(
            rewrite(
                "a [x] b [y]",
                &[(2..5, "[1]".into()), (8..11, "[2]".into())]
            ),
            "a [1] b [2]"
        );
    }

    #[test]
    fn duplicate_chains_resolve_to_the_end_and_cycles_fail() {
        let canonical = rec("5", &["Ann Lee"], Some(2026), "Widgets");
        let mut middle = rec("8", &["Ann Lee"], Some(2026), "Widgets");
        middle.duplicate_of = Some("5".into());
        let mut first = rec("9", &["Ann Lee"], Some(2026), "Widgets");
        first.duplicate_of = Some("8".into());
        let mut loop_a = rec("3", &["Bo Chen"], Some(2025), "Gadgets");
        loop_a.duplicate_of = Some("4".into());
        let mut loop_b = rec("4", &["Bo Chen"], Some(2025), "Gadgets");
        loop_b.duplicate_of = Some("3".into());
        let mut dangling = rec("6", &["Cy Dee"], Some(2024), "Gizmos");
        dangling.duplicate_of = Some("99".into());
        let records = [canonical, middle, first, loop_a, loop_b, dangling];
        let res = Resolver::new(&records).unwrap();
        assert_eq!(res.resolve("9").unwrap().unwrap().key, "5");
        assert_eq!(
            res.resolve("3").unwrap_err(),
            "duplicate_of cycle: 3 → 4 → 3"
        );
        assert!(
            res.resolve("6")
                .unwrap_err()
                .contains("which the registry lacks")
        );
    }

    #[test]
    fn a_citekey_pinned_twice_is_an_error() {
        let mut a = rec("1", &["Ann Lee"], Some(2026), "Widgets");
        a.extra.insert("citekey".into(), "shared".into());
        let mut b = rec("2", &["Bo Chen"], Some(2025), "Gadgets");
        b.extra.insert("citekey".into(), "shared".into());
        let records = [b, a];
        let err = Resolver::new(&records).err().unwrap();
        assert_eq!(
            err,
            "entries 1 and 2 both pin citekey shared; give one of them another"
        );
    }

    #[test]
    fn formats_ipsj_names() {
        assert_eq!(initials("Ronald Linn"), "R.L.");
        assert_eq!(initials("Jean-Paul"), "J.-P.");
        assert_eq!(initials("R.L."), "R.L.");
        assert_eq!(ipsj_name("Ronald L. Quist"), "Quist, R.L.");
        assert_eq!(ipsj_name("Ludo van Aster"), "van Aster, L.");
        assert_eq!(ipsj_name("山田　花子"), "山田　花子");
        assert_eq!(
            ipsj_name("Synthetic Standards Association"),
            "Synthetic Standards Association"
        );
    }

    // The expected strings follow the examples on the IPSJ page, minus the
    // volume, issue, and page fields the registry does not hold.
    #[test]
    fn formats_ipsj_entries() {
        let mut j = rec(
            "1",
            &["佐藤一郎", "鈴木次郎", "山田　花子"],
            Some(2005),
            "合成データによる評価モデルの検討",
        );
        j.venue = Some("情報処理学会論文誌".into());
        j.ids.doi = Some("10.1234/x".into());
        j.ids.url = Some("https://doi.org/10.1234/x".into());
        assert_eq!(
            ipsj(&j),
            "佐藤一郎，鈴木次郎，山田　花子：合成データによる評価モデルの検討，情報処理学会論文誌（オンライン），DOI：10.1234/x（2005）．"
        );

        let mut e = rec(
            "2",
            &["Tomoyuki Yamakami"],
            Some(2007),
            "Exploratory Session Analysis in the Mobile Clickstream",
        );
        e.venue = Some("IPSJ Digital Courier".into());
        e.ids.doi = Some("10.2197/ipsjdc.3.14".into());
        assert_eq!(
            ipsj(&e),
            "Yamakami, T.: Exploratory Session Analysis in the Mobile Clickstream, IPSJ Digital Courier (online), DOI: 10.2197/ipsjdc.3.14 (2007)."
        );

        let mut c = rec(
            "3",
            &["John C. Tang", "Scott L. Minneman"],
            Some(1990),
            "VideoDraw: a video interface for collaborative drawing",
        );
        c.venue = Some("Proceedings of CHI'90".into());
        assert_eq!(
            ipsj(&c),
            "Tang, J.C. and Minneman, S.L.: VideoDraw: a video interface for collaborative drawing, Proc. CHI'90 (1990)."
        );

        let mut w = rec("4", &["情報処理学会"], None, "コンピュータ博物館設立の提言");
        w.kind = Some("web".into());
        w.venue = Some("情報処理学会".into());
        w.ids.url = Some("http://www.ipsj.or.jp/03somu/teigen/museum200702.html".into());
        w.accessed = Some("2007-02-05".into());
        assert_eq!(
            ipsj(&w),
            "情報処理学会：コンピュータ博物館設立の提言，情報処理学会（オンライン），入手先<http://www.ipsj.or.jp/03somu/teigen/museum200702.html>（参照2007-02-05）．"
        );

        let mut en_web = rec("5", &["Widgetco"], None, "Widget docs");
        en_web.kind = Some("software".into());
        en_web.ids.url = Some("https://example.org/docs".into());
        en_web.accessed = Some("2026-11-07".into());
        assert_eq!(
            ipsj(&en_web),
            "Widgetco: Widget docs (online), available from <https://example.org/docs>(accessed 2026-11-07)."
        );

        let mut pre = rec(
            "6",
            &["Ann Lee", "Bo Chen", "Cy Dee", "Di Eve"],
            Some(2026),
            "Synthetic Widgets",
        );
        pre.ids.arxiv = Some("2601.00001".into());
        pre.version = Some("v2".into());
        assert_eq!(
            ipsj(&pre),
            "Lee, A., Chen, B., Dee, C., et al.: Synthetic Widgets, arXiv:2601.00001v2 (online), DOI: 10.48550/arXiv.2601.00001 (2026)."
        );

        let mut book = rec(
            "7",
            &["Ada B. Example"],
            Some(2008),
            "Foundations of Example Retrieval",
        );
        book.venue = Some("Example University Press".into());
        assert_eq!(
            ipsj(&book),
            "Example, A.B.: Foundations of Example Retrieval, Example University Press (2008)."
        );
    }
}
