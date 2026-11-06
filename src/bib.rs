//! Bibliography export from the registry: BibTeX and CSL-JSON, with citation
//! keys that stay the same from run to run.
//!
//! A citation key is `<first author's surname><year><first title word>` in
//! lowercase ASCII (`anon` without authors, `nd` without a year). Keys that
//! collide get `a`, `b`, … in registry key order across the whole registry, so
//! exporting a subset never changes them. An entry's optional `citekey` field
//! overrides the generated key; entries marked `duplicate_of` get none.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::catalogue::{ORG_WORDS, PARTICLES};
use crate::registry::Reference;
use crate::text::{fold, surname};

/// Words skipped when picking the title word of a citation key.
const STOP_WORDS: [&str; 12] = [
    "a", "an", "the", "on", "of", "in", "for", "to", "and", "with", "from", "towards",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Bibtex,
    CslJson,
}

/// A personal name split for citation styles, or an organization's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Name {
    Person { given: String, family: String },
    Literal(String),
}

/// Splits a "Given Family" name. Lowercase particles (`van`, `de`) join the
/// family name; organization names and single words stay whole.
pub fn split_name(name: &str) -> Name {
    let name = name.trim();
    let tokens: Vec<&str> = name.split_whitespace().collect();
    let org = tokens
        .iter()
        .any(|t| ORG_WORDS.contains(&t.trim_end_matches(',')));
    if org || tokens.len() < 2 || tokens.len() > 6 {
        return Name::Literal(name.to_owned());
    }
    let family_last = surname(name);
    let Some(last) = tokens
        .iter()
        .rposition(|t| t.trim_end_matches(',') == family_last)
    else {
        return Name::Literal(name.to_owned());
    };
    let mut start = last;
    while start > 1 && PARTICLES.contains(&tokens[start - 1]) {
        start -= 1;
    }
    let suffix: Vec<&str> = tokens[last + 1..].to_vec();
    let mut family = tokens[start..=last].join(" ");
    if !suffix.is_empty() {
        family = format!("{family}, {}", suffix.join(" "));
    }
    Name::Person {
        given: tokens[..start].join(" "),
        family,
    }
}

fn ascii_word(s: &str) -> String {
    fold(s)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn base_key(r: &Reference) -> String {
    let author = r
        .authors
        .as_ref()
        .and_then(|a| a.first())
        .map(|a| match split_name(a) {
            Name::Person { family, .. } => ascii_word(family.split(", ").next().unwrap_or(&family)),
            Name::Literal(l) => ascii_word(l.split_whitespace().next().unwrap_or(&l)),
        })
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "anon".to_owned());
    let year = r.year.map_or_else(|| "nd".to_owned(), |y| y.to_string());
    let word = r
        .title
        .as_deref()
        .unwrap_or_default()
        .split(|c: char| c.is_whitespace() || c == '-' || c == ':')
        .map(ascii_word)
        .find(|w| !w.is_empty() && !STOP_WORDS.contains(&w.as_str()))
        .unwrap_or_default();
    format!("{author}{year}{word}")
}

/// Citation keys for every registry entry, by registry key.
pub fn citation_keys(records: &[Reference]) -> BTreeMap<String, String> {
    let mut sorted: Vec<&Reference> = records.iter().collect();
    sorted.sort_by(|a, b| crate::registry::key_order(&a.key, &b.key));
    let pinned = |r: &Reference| {
        r.extra
            .get("citekey")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let mut groups: BTreeMap<String, Vec<&Reference>> = BTreeMap::new();
    // Duplicates export as their canonical entry, so they take no key of their own.
    for r in sorted
        .iter()
        .filter(|r| pinned(r).is_none() && r.duplicate_of.is_none())
    {
        groups.entry(base_key(r)).or_default().push(r);
    }
    let mut out = BTreeMap::new();
    // Pinned keys are reserved first; generated keys skip every key already
    // taken, so no two entries share a citation key unless both pin it.
    let mut taken = std::collections::HashSet::new();
    for r in &sorted {
        if let Some(k) = pinned(r) {
            taken.insert(k.clone());
            out.insert(r.key.clone(), k);
        }
    }
    for (base, group) in groups {
        if group.len() == 1 && taken.insert(base.clone()) {
            out.insert(group[0].key.clone(), base);
            continue;
        }
        let mut i = 0;
        for r in group {
            let key = loop {
                let candidate = format!("{base}{}", suffix(i));
                i += 1;
                if taken.insert(candidate.clone()) {
                    break candidate;
                }
            };
            out.insert(r.key.clone(), key);
        }
    }
    out
}

/// `a`, `b`, …, `z`, `aa`, `ab`, …
fn suffix(i: usize) -> String {
    let mut n = i;
    let mut s = Vec::new();
    loop {
        s.push(b'a' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).expect("ASCII")
}

/// What kind of publication an entry is, for both formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Journal,
    Conference,
    Preprint,
    Book,
    Web,
    Software,
    Dataset,
    Standard,
    Other,
}

const CONFERENCE_WORDS: [&str; 7] = [
    "proceedings",
    "proc.",
    "conference",
    "workshop",
    "symposium",
    "congress",
    "meeting",
];

pub fn kind(r: &Reference) -> Kind {
    let venue = r.venue.as_deref().unwrap_or_default().to_lowercase();
    match r.kind.as_deref() {
        Some("web") => Kind::Web,
        Some("software") => Kind::Software,
        Some("dataset") => Kind::Dataset,
        Some("standard") => Kind::Standard,
        Some("other") => Kind::Other,
        _ if CONFERENCE_WORDS.iter().any(|w| venue.contains(w)) => Kind::Conference,
        _ if r.ids.doi.is_some() && !venue.is_empty() && !venue.contains("arxiv") => Kind::Journal,
        _ if r.ids.arxiv.is_some() => Kind::Preprint,
        _ if venue.contains("press") || venue.contains("publisher") => Kind::Book,
        _ => Kind::Other,
    }
}

/// Escapes BibTeX/LaTeX special characters.
fn tex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' | '}' | '&' | '%' | '$' | '#' | '_' => {
                out.push('\\');
                out.push(c);
            }
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            c => out.push(c),
        }
    }
    out
}

fn bib_name(n: &str) -> String {
    match split_name(n) {
        Name::Person { given, family } => match family.split_once(", ") {
            Some((f, suffix)) => format!("{}, {}, {}", tex(f), tex(suffix), tex(&given)),
            None => format!("{}, {}", tex(&family), tex(&given)),
        },
        Name::Literal(l) => format!("{{{}}}", tex(&l)),
    }
}

pub fn bibtex(r: &Reference, citekey: &str) -> String {
    let k = kind(r);
    let entry_type = match k {
        Kind::Journal => "article",
        Kind::Conference => "inproceedings",
        Kind::Book => "book",
        _ => "misc",
    };
    let mut fields: Vec<(&str, String)> = Vec::new();
    if let Some(authors) = &r.authors {
        let mut names: Vec<String> = authors.iter().map(|a| bib_name(a)).collect();
        if r.authors_truncated {
            names.push("others".to_owned());
        }
        fields.push(("author", names.join(" and ")));
    }
    if let Some(t) = &r.title {
        fields.push(("title", format!("{{{}}}", tex(t))));
    }
    if let Some(v) = &r.venue {
        let field = match k {
            Kind::Journal => "journal",
            Kind::Conference => "booktitle",
            Kind::Book => "publisher",
            Kind::Web | Kind::Software | Kind::Dataset | Kind::Standard => "howpublished",
            // A preprint's or unknown work's venue is often a free-text remark.
            Kind::Preprint | Kind::Other => "note",
        };
        fields.push((field, tex(v)));
    }
    if let Some(y) = r.year {
        fields.push(("year", y.to_string()));
    }
    if let Some(d) = &r.ids.doi {
        fields.push(("doi", tex(d)));
    }
    if let Some(a) = &r.ids.arxiv {
        fields.push((
            "eprint",
            format!("{a}{}", r.version.as_deref().unwrap_or("")),
        ));
        fields.push(("archiveprefix", "arXiv".to_owned()));
    }
    if let Some(u) = &r.ids.url {
        fields.push(("url", u.clone()));
    }
    if let Some(a) = &r.accessed {
        fields.push(("urldate", a.clone()));
    }
    let body: Vec<String> = fields
        .iter()
        .map(|(f, v)| format!("  {f} = {{{v}}}"))
        .collect();
    format!("@{entry_type}{{{citekey},\n{}\n}}\n", body.join(",\n"))
}

fn date_parts(date: &str) -> Option<Value> {
    let parts: Vec<i64> = date
        .split('-')
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<i64>>>()?;
    Some(json!({ "date-parts": [parts] }))
}

pub fn csl(r: &Reference, citekey: &str) -> Value {
    let k = kind(r);
    let csl_type = match k {
        Kind::Journal => "article-journal",
        Kind::Conference => "paper-conference",
        Kind::Preprint => "article",
        Kind::Book => "book",
        Kind::Web => "webpage",
        Kind::Software => "software",
        Kind::Dataset => "dataset",
        Kind::Standard => "standard",
        Kind::Other => "document",
    };
    let mut m = Map::new();
    m.insert("id".into(), citekey.into());
    m.insert("type".into(), csl_type.into());
    if let Some(t) = &r.title {
        m.insert("title".into(), t.clone().into());
    }
    if let Some(authors) = &r.authors {
        let names: Vec<Value> = authors
            .iter()
            .map(|a| match split_name(a) {
                Name::Person { given, family } => match family.split_once(", ") {
                    Some((f, suffix)) => json!({"family": f, "given": given, "suffix": suffix}),
                    None => json!({"family": family, "given": given}),
                },
                Name::Literal(l) => json!({ "literal": l }),
            })
            .collect();
        m.insert("author".into(), names.into());
    }
    if let Some(y) = r.year {
        m.insert("issued".into(), json!({ "date-parts": [[y]] }));
    }
    if let Some(v) = &r.venue {
        let field = match k {
            Kind::Book => "publisher",
            Kind::Preprint | Kind::Other => "note",
            _ => "container-title",
        };
        m.insert(field.into(), v.clone().into());
    }
    if let Some(d) = &r.ids.doi {
        m.insert("DOI".into(), d.clone().into());
    }
    if let Some(a) = &r.ids.arxiv {
        m.insert(
            "number".into(),
            format!("arXiv:{a}{}", r.version.as_deref().unwrap_or("")).into(),
        );
        if k == Kind::Preprint {
            m.insert("publisher".into(), "arXiv".into());
        }
    }
    if let Some(u) = &r.ids.url {
        m.insert("URL".into(), u.clone().into());
    }
    if let Some(parts) = r.accessed.as_deref().and_then(date_parts) {
        m.insert("accessed".into(), parts);
    }
    m.insert("custom".into(), json!({ "registry_key": r.key }));
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(key: &str, author: &str, year: Option<i64>, title: &str) -> Reference {
        let mut r = Reference::new(key.into());
        r.authors = Some(vec![author.into()]);
        r.year = year;
        r.title = Some(title.into());
        r
    }

    #[test]
    fn splits_names() {
        assert_eq!(
            split_name("Ludo van Aster"),
            Name::Person {
                given: "Ludo".into(),
                family: "van Aster".into()
            }
        );
        assert_eq!(
            split_name("Marten Lowe Kingsley Jr."),
            Name::Person {
                given: "Marten Lowe".into(),
                family: "Kingsley, Jr.".into()
            }
        );
        assert_eq!(
            split_name("Synthetic Standards Association"),
            Name::Literal("Synthetic Standards Association".into())
        );
        assert_eq!(split_name("Widgetco"), Name::Literal("Widgetco".into()));
    }

    #[test]
    fn citation_keys_are_stable_across_subsets() {
        let a = rec("2", "Ann Lée", Some(2026), "The Widgets of Retrieval");
        let b = rec("10", "Ann Lee", Some(2026), "Widgets, again");
        let c = rec("3", "Bo Chen", None, "On Gadgets");
        let mut pinned = rec("4", "Cy Dee", Some(2020), "X");
        pinned.extra.insert("citekey".into(), "dee-x".into());
        let mut dup = rec("11", "Ann Lee", Some(2026), "Widgets");
        dup.duplicate_of = Some("2".into());
        let all = citation_keys(&[b.clone(), a.clone(), c, pinned, dup]);
        assert!(!all.contains_key("11"));
        assert_eq!(all["2"], "lee2026widgetsa");
        assert_eq!(all["10"], "lee2026widgetsb");
        assert_eq!(all["3"], "chenndgadgets");
        assert_eq!(all["4"], "dee-x");
        assert_eq!(suffix(25), "z");
        assert_eq!(suffix(26), "aa");
    }

    #[test]
    fn generated_citation_keys_avoid_pinned_ones() {
        let mut pinned = rec("1", "Cy Dee", Some(2020), "Other");
        pinned
            .extra
            .insert("citekey".into(), "lee2026widgets".into());
        let mut pinned_a = rec("2", "Cy Dee", Some(2021), "Other");
        pinned_a
            .extra
            .insert("citekey".into(), "chen2025gadgetsa".into());
        let lone = rec("3", "Ann Lee", Some(2026), "Widgets");
        let pair_1 = rec("4", "Bo Chen", Some(2025), "Gadgets");
        let pair_2 = rec("5", "Bo Chen", Some(2025), "Gadgets, again");
        let keys = citation_keys(&[pinned, pinned_a, lone, pair_1, pair_2]);
        assert_eq!(keys["1"], "lee2026widgets");
        assert_eq!(keys["3"], "lee2026widgetsa");
        assert_eq!(keys["4"], "chen2025gadgetsb");
        assert_eq!(keys["5"], "chen2025gadgetsc");
        let distinct: std::collections::HashSet<&String> = keys.values().collect();
        assert_eq!(distinct.len(), keys.len());
    }

    #[test]
    fn writes_bibtex() {
        let mut r = rec("7", "Ann Lee", Some(2025), "Widgets & {Gadgets}");
        r.authors = Some(vec![
            "Ann Lee".into(),
            "Synthetic Standards Association".into(),
        ]);
        r.authors_truncated = true;
        r.venue = Some("Proceedings of the Synthetic Conference".into());
        r.ids.doi = Some("10.1234/w_1".into());
        r.accessed = Some("2026-11-07".into());
        let bib = bibtex(&r, "lee2025widgets");
        assert!(bib.starts_with("@inproceedings{lee2025widgets,\n"), "{bib}");
        assert!(
            bib.contains("author = {Lee, Ann and {Synthetic Standards Association} and others}")
        );
        assert!(
            bib.contains("title = {{Widgets \\& \\{Gadgets\\}}}"),
            "{bib}"
        );
        assert!(bib.contains("booktitle = {Proceedings of the Synthetic Conference}"));
        assert!(bib.contains("doi = {10.1234/w\\_1}"));
        assert!(bib.contains("urldate = {2026-11-07}"));
        let mut p = rec("8", "Bo Chen", Some(2026), "Preprint");
        p.ids.arxiv = Some("2601.00001".into());
        p.version = Some("v2".into());
        let bib = bibtex(&p, "k");
        assert!(bib.starts_with("@misc{k,") && bib.contains("eprint = {2601.00001v2}"));
        p.venue = Some("Under review".into());
        assert!(bibtex(&p, "k").contains("note = {Under review}"));
        assert_eq!(csl(&p, "k")["note"], "Under review");
    }

    #[test]
    fn writes_csl_json() {
        let mut r = rec("9", "Ann Lee", Some(2024), "Widgets");
        r.ids.doi = Some("10.1234/w".into());
        r.venue = Some("Journal of Synthetic Results".into());
        r.accessed = Some("2026-11-07".into());
        let v = csl(&r, "lee2024widgets");
        assert_eq!(v["type"], "article-journal");
        assert_eq!(v["author"][0], json!({"family": "Lee", "given": "Ann"}));
        assert_eq!(v["issued"], json!({"date-parts": [[2024]]}));
        assert_eq!(v["accessed"], json!({"date-parts": [[2026, 11, 7]]}));
        assert_eq!(v["container-title"], "Journal of Synthetic Results");
        assert_eq!(v["custom"]["registry_key"], "9");
        let mut w = rec("10", "Widgetco", None, "Widget docs");
        w.kind = Some("web".into());
        assert_eq!(csl(&w, "k")["type"], "webpage");
        assert_eq!(csl(&w, "k")["author"][0], json!({"literal": "Widgetco"}));
    }
}
