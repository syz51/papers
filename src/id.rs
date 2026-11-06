//! arXiv identifier parsing and normalization.

use std::fmt;

/// A validated arXiv identifier, without the `arXiv:` prefix or URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArxivId {
    /// Versionless id: `2208.06046` or `hep-th/9901001`.
    pub id: String,
    pub version: Option<u32>,
}

impl ArxivId {
    /// Parses an id given as `2208.06046`, `2208.06046v4`, `arXiv:2208.06046`,
    /// an arxiv.org abs/pdf/html URL, or an old-style id like `hep-th/9901001`.
    pub fn parse(input: &str) -> Result<Self, String> {
        let s = input.trim();
        let bare = match strip_url(s) {
            Some(rest) => rest,
            None => strip_prefix_ignore_case(s, "arxiv:").unwrap_or(s),
        };
        parse_bare(bare).ok_or_else(|| {
            format!(
                "invalid arXiv id {input:?}: expected e.g. 2208.06046, 2208.06046v4, \
                 arXiv:2208.06046, an arxiv.org abs/pdf/html URL, or hep-th/9901001"
            )
        })
    }

    /// Parses an arxiv.org `abs` URL (any version) as found in a note's `url`;
    /// returns `None` for anything else.
    pub fn from_abs_url(url: &str) -> Option<Self> {
        let rest = strip_host(url.trim())?;
        parse_bare(clean_path(rest.strip_prefix("abs/")?))
    }

    /// Versionless landing page.
    pub fn abs_url(&self) -> String {
        format!("https://arxiv.org/abs/{}", self.id)
    }
}

/// The form sent to the API: the id plus its version when one was given.
impl fmt::Display for ArxivId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.version {
            Some(v) => write!(f, "{}v{v}", self.id),
            None => f.write_str(&self.id),
        }
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Strips scheme and host of an arxiv.org URL, returning the path.
fn strip_host(s: &str) -> Option<&str> {
    let s = strip_prefix_ignore_case(s, "https://")
        .or_else(|| strip_prefix_ignore_case(s, "http://"))
        .unwrap_or(s);
    ["arxiv.org/", "www.arxiv.org/", "export.arxiv.org/"]
        .iter()
        .find_map(|host| strip_prefix_ignore_case(s, host))
}

/// Path of an abs/pdf/html URL with query, fragment, slash, and `.pdf` removed.
fn strip_url(s: &str) -> Option<&str> {
    let path = strip_host(s)?;
    let rest = ["abs/", "pdf/", "html/"]
        .iter()
        .find_map(|kind| path.strip_prefix(kind))?;
    Some(clean_path(rest))
}

fn clean_path(rest: &str) -> &str {
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let rest = rest.trim_end_matches('/');
    rest.strip_suffix(".pdf").unwrap_or(rest)
}

/// Parses `<id>[v<n>]` where id is new-style `YYMM.NNNN(N)` or old-style
/// `archive[.XX]/YYMMNNN`. New-style numbers have 4 digits before 2015 and 5
/// since. The old-style subject class is dropped, as arXiv does.
pub(crate) fn parse_bare(s: &str) -> Option<ArxivId> {
    let (base, version) = match s.rfind('v') {
        Some(pos) if pos > 0 && is_digits(&s[pos + 1..]) => {
            let v: u32 = s[pos + 1..].parse().ok()?;
            if v == 0 {
                return None;
            }
            (&s[..pos], Some(v))
        }
        _ => (s, None),
    };
    let id = if let Some((yymm, num)) = base.split_once('.')
        && is_yymm(yymm)
        && is_digits(num)
        && yymm >= "0704"
        && num.len() == if yymm >= "1501" { 5 } else { 4 }
    {
        base.to_owned()
    } else {
        let (archive, num) = base.split_once('/')?;
        let archive = match archive.split_once('.') {
            Some((a, class))
                if class.len() == 2 && class.bytes().all(|b| b.is_ascii_uppercase()) =>
            {
                a
            }
            Some(_) => return None,
            None => archive,
        };
        let archive_ok = !archive.is_empty()
            && archive.starts_with(|c: char| c.is_ascii_lowercase())
            && archive.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
        if !(archive_ok && num.len() == 7 && is_digits(num) && is_yymm(&num[..4])) {
            return None;
        }
        format!("{archive}/{num}")
    };
    Some(ArxivId { id, version })
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_yymm(s: &str) -> bool {
    s.len() == 4 && is_digits(s) && matches!(s[2..].parse::<u8>(), Ok(1..=12))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(input: &str) -> String {
        ArxivId::parse(input).unwrap().to_string()
    }

    #[test]
    fn accepts_supported_forms() {
        assert_eq!(ok("2208.06046"), "2208.06046");
        assert_eq!(ok(" 2208.06046v4 "), "2208.06046v4");
        assert_eq!(ok("arXiv:2208.06046"), "2208.06046");
        assert_eq!(ok("ARXIV:2208.06046v2"), "2208.06046v2");
        assert_eq!(ok("0704.0001"), "0704.0001");
        assert_eq!(ok("https://arxiv.org/abs/2208.06046v4"), "2208.06046v4");
        assert_eq!(ok("http://www.arxiv.org/abs/2208.06046/"), "2208.06046");
        assert_eq!(ok("https://arxiv.org/pdf/2208.06046v6.pdf"), "2208.06046v6");
        assert_eq!(ok("https://arxiv.org/pdf/2208.06046"), "2208.06046");
        assert_eq!(ok("https://arxiv.org/html/2208.06046v6#S2"), "2208.06046v6");
        assert_eq!(
            ok("https://export.arxiv.org/abs/2208.06046?context=q-fin"),
            "2208.06046"
        );
        assert_eq!(ok("hep-th/9901001"), "hep-th/9901001");
        assert_eq!(ok("hep-th/9901001v3"), "hep-th/9901001v3");
        assert_eq!(ok("solv-int/9901001"), "solv-int/9901001");
        assert_eq!(ok("math.GT/0309136"), "math/0309136");
        assert_eq!(ok("arXiv:hep-th/9901001"), "hep-th/9901001");
        assert_eq!(
            ok("https://arxiv.org/abs/hep-th/9901001v1"),
            "hep-th/9901001v1"
        );
    }

    #[test]
    fn splits_version() {
        let id = ArxivId::parse("2208.06046v12").unwrap();
        assert_eq!((id.id.as_str(), id.version), ("2208.06046", Some(12)));
        assert_eq!(id.abs_url(), "https://arxiv.org/abs/2208.06046");
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "not-an-id",
            "",
            "2208.0604",
            "1412.00001",
            "0612.0001",
            "2208.060461",
            "2213.06046",
            "2208.06046v",
            "2208.06046v0",
            "2208.06046x",
            "HEP-TH/9901001",
            "hep-th/990100",
            "math.gt/0309136",
            "https://example.org/abs/2208.06046",
            "https://arxiv.org/list/2208.06046",
            "doi:10.1000/xyz",
        ] {
            assert!(ArxivId::parse(bad).is_err(), "{bad:?} should be rejected");
        }
        let err = ArxivId::parse("not-an-id").unwrap_err();
        assert!(err.contains("\"not-an-id\""), "{err}");
    }

    #[test]
    fn abs_url_only_for_notes() {
        let id = ArxivId::from_abs_url("https://arxiv.org/abs/2208.06046v3").unwrap();
        assert_eq!(id.id, "2208.06046");
        assert!(ArxivId::from_abs_url("https://arxiv.org/pdf/2208.06046").is_none());
        assert!(ArxivId::from_abs_url("https://doi.org/10.1002/fut.22556").is_none());
    }
}
