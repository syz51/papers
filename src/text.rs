//! Text normalization: whitespace, accent folding, title keys, surnames, slugs.

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// Collapses runs of whitespace to single spaces and trims the ends.
pub fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Removes accents (NFKD, then drops combining marks) and transliterates the
/// common Latin letters that do not decompose.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfkd().filter(|&c| !is_combining_mark(c)) {
        match c {
            'ø' => out.push('o'),
            'Ø' => out.push('O'),
            'ł' => out.push('l'),
            'Ł' => out.push('L'),
            'đ' => out.push('d'),
            'Đ' => out.push('D'),
            'ı' => out.push('i'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'Æ' => out.push_str("AE"),
            'œ' => out.push_str("oe"),
            'Œ' => out.push_str("OE"),
            'þ' => out.push_str("th"),
            'Þ' => out.push_str("Th"),
            _ => out.push(c),
        }
    }
    out
}

/// Case-, punctuation-, whitespace-, and accent-insensitive comparison key.
pub fn key(s: &str) -> String {
    fold(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Last name token of a "Given Family" author name, ignoring generational suffixes.
pub fn surname(name: &str) -> &str {
    let mut tokens: Vec<&str> = name
        .split_whitespace()
        .map(|t| t.trim_end_matches(','))
        .collect();
    while tokens.len() > 1
        && matches!(
            tokens[tokens.len() - 1].to_ascii_lowercase().as_str(),
            "jr" | "jr." | "sr" | "sr." | "ii" | "iii" | "iv"
        )
    {
        tokens.pop();
    }
    tokens.last().copied().unwrap_or_default()
}

/// True when `family` is a whole part of `name`: a run of its words (split
/// on spaces, hyphens, commas, and periods) whose keys join to `family`'s key,
/// so "van Aster" and "Smith-Jones" match but "Li" does not match "William".
/// Names in scripts written without spaces (CJK) match by substring.
pub fn has_surname(name: &str, family: &str) -> bool {
    let want = key(family);
    if want.is_empty() {
        return false;
    }
    if !want.is_ascii() {
        return key(name).contains(&want);
    }
    let words: Vec<String> = name
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | ',' | '.'))
        .map(key)
        .filter(|w| !w.is_empty())
        .collect();
    (0..words.len()).any(|i| {
        let mut joined = String::new();
        words[i..].iter().any(|w| {
            joined += w;
            joined == want
        })
    })
}

/// Lowercase ASCII words of `s`, split on anything that is not a letter or digit.
pub fn ascii_words(s: &str) -> Vec<String> {
    fold(s)
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "and", "or", "in", "on", "for", "to", "with", "via", "from", "by",
    "at", "is", "are", "as", "its", "into", "under", "over", "toward", "towards", "do", "does",
    "what", "when", "how", "why", "we", "our", "their", "these", "those", "vs", "be", "can", "it",
    "this", "that",
];

/// Slug from the first 4 significant title words (3 if 4 would exceed 32 chars).
pub fn title_slug(title: &str) -> String {
    let words: Vec<String> = ascii_words(title)
        .into_iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .take(4)
        .collect();
    let slug = words.join("-");
    if words.len() == 4 && slug.len() > 32 {
        words[..3].join("-")
    } else if slug.is_empty() {
        "paper".to_owned()
    } else {
        slug
    }
}

/// Removes XML/HTML tags (Crossref JATS, `<i>` in titles) and decodes the
/// common entities, then squashes whitespace.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    // Tags inside a word (`Ca<sub>2</sub>`) should not split it, but tags are
    // usually word boundaries in titles and abstracts; squash the result.
    squash(&out)
}

/// Venue comparison key: like [`key`], with `&` read as "and" and a leading "The" dropped.
pub fn venue_key(s: &str) -> String {
    let words = ascii_words(&s.replace('&', " and "));
    let start = usize::from(words.first().is_some_and(|w| w == "the"));
    words[start..].concat()
}

/// Title similarity in [0, 1]: Dice coefficient of the two titles' word sets,
/// and exactly 1 when their [`key`]s are equal.
pub fn similarity(a: &str, b: &str) -> f64 {
    if key(a) == key(b) {
        return 1.0;
    }
    let set = |s: &str| {
        let mut words = ascii_words(s);
        words.sort();
        words.dedup();
        words
    };
    let (a, b) = (set(a), set(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let common = a.iter().filter(|w| b.binary_search(w).is_ok()).count();
    (2 * common) as f64 / (a.len() + b.len()) as f64
}

/// Comparable form of a web URL: lowercase scheme and host without `www.`,
/// no fragment, no `utm_*` parameters, and no trailing slash. `None` for
/// anything that is not an http(s) URL.
pub fn normalize_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let rest = rest.split('#').next().unwrap_or_default();
    let (host_path, query) = match rest.split_once('?') {
        Some((hp, q)) => (hp, Some(q)),
        None => (rest, None),
    };
    let (host, path) = match host_path.find('/') {
        Some(i) => (&host_path[..i], &host_path[i..]),
        None => (host_path, ""),
    };
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host.is_empty() {
        return None;
    }
    let query: Vec<&str> = query
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter(|kv| !kv.is_empty() && !kv.starts_with("utm_"))
        .collect();
    let mut out = format!("{host}{}", path.trim_end_matches('/'));
    if !query.is_empty() {
        out = format!("{out}?{}", query.join("&"));
    }
    Some(out)
}

/// Dedupe key for a work without identifiers: normalized title plus the first
/// author's normalized surname (empty when no author is known).
pub fn title_author_key(title: &str, first_author: Option<&str>) -> Option<String> {
    let t = key(title);
    (!t.is_empty()).then(|| {
        format!(
            "{t}|{}",
            first_author.map(|a| key(surname(a))).unwrap_or_default()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surnames_match_whole_name_parts() {
        assert!(has_surname("Ann Smith-Jones", "Smith-Jones"));
        assert!(has_surname("Ann Smith Jones", "Smith-Jones"));
        assert!(has_surname("Ann Smith-Jones", "Jones"));
        assert!(has_surname("Ludo van Aster", "van Aster"));
        assert!(has_surname("José García", "Garcia"));
        assert!(has_surname("山田太郎", "山田"));
        assert!(!has_surname("William Chen", "Li"));
        assert!(!has_surname("Lina Tanaka", "Tan"));
        assert!(!has_surname("Ann Lee", ""));
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_url("HTTPS://WWW.Example.org/Path/?utm_source=x&a=1#frag").as_deref(),
            Some("example.org/Path?a=1")
        );
        assert_eq!(
            normalize_url("http://example.org/a/").as_deref(),
            Some("example.org/a")
        );
        assert_eq!(normalize_url("ftp://example.org/a"), None);
        assert_eq!(normalize_url("not a url"), None);
        assert_eq!(
            title_author_key("A Study: of Things!", Some("Ann B. Lee")).as_deref(),
            Some("astudyofthings|lee")
        );
        assert_eq!(title_author_key("", None), None);
    }

    #[test]
    fn squashes_whitespace() {
        assert_eq!(
            squash("  Synthetic\n  Market\tMaking "),
            "Synthetic Market Making"
        );
    }

    #[test]
    fn title_keys_ignore_case_punctuation_space_accents() {
        assert_eq!(
            key("Synthetic Market Making and Rebalancing-Versus-Holding"),
            key("synthetic market-making and rebalancing versus holding.")
        );
        assert_eq!(key("Évaluation des marchés"), key("Evaluation des Marches"));
        assert_eq!(key("Ørsted: a Study"), "orstedastudy");
        assert_ne!(
            key("Synthetic Market Making"),
            key("Synthetic Market Maker")
        );
    }

    #[test]
    fn surnames() {
        assert_eq!(surname("Pell Anders Kemp"), "Kemp");
        assert_eq!(surname("Niles O. Barrow"), "Barrow");
        assert_eq!(surname("Rowan E. Lark Jr."), "Lark");
        assert_eq!(surname("Plato"), "Plato");
        assert_eq!(key(surname("Aurelio Cappadonna")), "cappadonna");
        assert_eq!(key(surname("Rubén Ortiz-Rodríguez")), "ortizrodriguez");
    }

    #[test]
    fn slugs() {
        assert_eq!(
            title_slug("Synthetic Market Making and Rebalancing-Versus-Holding"),
            "synthetic-market-making"
        );
        assert_eq!(
            title_slug("The Fundamentals of Perpetual Futures"),
            "fundamentals-perpetual-futures"
        );
        assert_eq!(
            title_slug("Cross-Rollup MEV: Non-Atomic Arbitrage Across L2 Blockchains"),
            "cross-rollup-mev-non"
        );
        assert_eq!(
            title_slug("Understanding Characteristics of Stablecoin Implementations"),
            "understanding-characteristics-stablecoin"
        );
        assert_eq!(title_slug("Ação Ørsted Straße"), "acao-orsted-strasse");
        assert_eq!(title_slug("Of the"), "paper");
    }

    #[test]
    fn strips_markup() {
        assert_eq!(
            strip_markup("<jats:p>Prices &amp; <i>arbitrage</i>\n in  markets</jats:p>"),
            "Prices & arbitrage in markets"
        );
        assert_eq!(strip_markup("a &lt; b"), "a < b");
    }

    #[test]
    fn venue_keys_contain() {
        let note = venue_key("Journal of Financial Economics 135(2), 293–319 (2020)");
        assert!(note.contains(&venue_key("Journal of Financial Economics")));
        assert!(
            venue_key("Journal of Financial and Quantitative Analysis")
                .contains(&venue_key("Journal of Financial & Quantitative Analysis"))
        );
        assert!(
            venue_key("Review of Financial Studies 33(1)")
                .contains(&venue_key("The Review of Financial Studies"))
        );
        assert!(!note.contains(&venue_key("Journal of Finance and Economics")));
    }

    #[test]
    fn title_similarity() {
        assert_eq!(
            similarity(
                "Network-Based Detection of Wash Trading",
                "network based detection of wash trading."
            ),
            1.0
        );
        let s = similarity(
            "Network-Based Detection of Wash Trading",
            "Wash Trading Detection in Automatic Market Makers",
        );
        assert!(s > 0.3 && s < 0.8, "{s}");
        assert_eq!(similarity("", "x"), 0.0);
    }
}
