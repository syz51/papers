//! Crossref REST API (`api.crossref.org/works/<doi>`): the authority for DOI metadata.

use serde::{Deserialize, Serialize};

use crate::http::{Get, Http, encode};
use crate::refs::Doi;
use crate::text::{squash, strip_markup};

const ENDPOINT: &str = "https://api.crossref.org/works/";

/// Date fields that record when a version became public, in display order.
/// `created`, `deposited`, and `indexed` are registration events and are not used.
pub const DATE_FIELDS: [&str; 5] = [
    "posted",
    "published-online",
    "published-print",
    "published",
    "issued",
];

#[derive(Debug, Deserialize)]
struct Envelope {
    message: Message,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Message {
    #[serde(rename = "DOI")]
    doi: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    title: Vec<String>,
    #[serde(default)]
    subtitle: Vec<String>,
    #[serde(default)]
    author: Vec<Author>,
    #[serde(default)]
    container_title: Vec<String>,
    volume: Option<String>,
    issue: Option<String>,
    page: Option<String>,
    article_number: Option<String>,
    publisher: Option<String>,
    group_title: Option<String>,
    #[serde(default)]
    institution: Vec<Institution>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    posted: Option<DateParts>,
    published_online: Option<DateParts>,
    published_print: Option<DateParts>,
    published: Option<DateParts>,
    issued: Option<DateParts>,
    #[serde(default, deserialize_with = "null_as_empty")]
    updated_by: Vec<RawUpdate>,
    #[serde(default, deserialize_with = "null_as_empty")]
    update_to: Vec<RawUpdate>,
}

/// Crossref sends `null` for an absent update list.
fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<RawUpdate>, D::Error> {
    Ok(Option::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
struct RawUpdate {
    #[serde(rename = "DOI")]
    doi: Option<String>,
    #[serde(rename = "type", default)]
    kind: String,
    source: Option<String>,
    updated: Option<DateParts>,
}

/// Which way an update notice points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// `updated-by`: a later notice changes this work.
    UpdatedBy,
    /// `update-to`: this work is the notice; it changes another work.
    UpdateTo,
}

/// A Crossmark or Retraction Watch notice: a retraction, correction, and so on.
#[derive(Debug, Clone, Serialize)]
pub struct Update {
    pub direction: Direction,
    /// Crossref's `type`, such as `retraction`, `correction`, or `withdrawal`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The other work: the notice for `updated-by`, the changed work for `update-to`.
    pub doi: Option<String>,
    pub date: Option<String>,
    /// `publisher` or `retraction-watch`.
    pub source: Option<String>,
}

impl Update {
    /// A notice that takes the work back: retraction, withdrawal, or removal.
    pub fn is_retraction(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "retraction" | "partial_retraction" | "withdrawal" | "removal"
        )
    }
}

#[derive(Debug, Deserialize)]
struct Author {
    given: Option<String>,
    family: Option<String>,
    /// Organizational authors.
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Institution {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DateParts {
    #[serde(rename = "date-parts", default)]
    parts: Vec<Vec<Option<i64>>>,
}

impl DateParts {
    /// `YYYY`, `YYYY-MM`, or `YYYY-MM-DD`; `None` when the year is absent.
    fn render(&self) -> Option<String> {
        let parts = self.parts.first()?;
        let year = (*parts.first()?)?;
        let mut out = format!("{year:04}");
        for p in parts[1..].iter().map_while(|p| *p) {
            out += &format!("-{p:02}");
        }
        Some(out)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Contributor {
    pub name: String,
    /// Crossref's `family` field, when given.
    pub family: Option<String>,
}

/// A Crossref work.
#[derive(Debug, Clone, Serialize)]
pub struct Work {
    pub doi: Doi,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<Contributor>,
    /// `(field, date)` for each of [`DATE_FIELDS`] present.
    pub dates: Vec<(String, String)>,
    pub container: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub publisher: Option<String>,
    /// SSRN's `group-title` or a report's institution (e.g. NBER).
    pub series: Option<String>,
    #[serde(rename = "abstract")]
    pub abstract_text: Option<String>,
    /// Notices that change this work, or that this work is.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub updates: Vec<Update>,
}

impl Work {
    /// Earliest year among the publication and posting dates.
    pub fn earliest_year(&self) -> Option<i64> {
        self.dates
            .iter()
            .filter_map(|(_, d)| d.get(..4)?.parse().ok())
            .min()
    }

    /// The year of the version of record (`issued`), else the earliest.
    pub fn issued_year(&self) -> Option<i64> {
        self.dates
            .iter()
            .find(|(f, _)| f == "issued")
            .and_then(|(_, d)| d.get(..4)?.parse().ok())
            .or_else(|| self.earliest_year())
    }

    pub fn is_journal_article(&self) -> bool {
        self.kind == "journal-article"
    }

    /// Container with volume(issue) and pages: `Journal 135(2), 293-319`.
    pub fn citation(&self) -> Option<String> {
        let mut out = self.container.clone()?;
        match (&self.volume, &self.issue) {
            (Some(v), Some(i)) => out += &format!(" {v}({i})"),
            (Some(v), None) => out += &format!(" {v}"),
            (None, Some(i)) => out += &format!(" ({i})"),
            (None, None) => {}
        }
        if let Some(p) = &self.pages {
            out += &format!(", {p}");
        }
        Some(out)
    }
}

/// Parses a `/works/<doi>` response.
pub fn parse_work(json: &str) -> Result<Work, String> {
    let envelope: Envelope =
        serde_json::from_str(json).map_err(|e| format!("invalid Crossref JSON: {e}"))?;
    let m = envelope.message;
    let doi = Doi::parse(&m.doi).ok_or_else(|| format!("Crossref returned bad DOI {:?}", m.doi))?;
    let clean = |s: &str| Some(strip_markup(s)).filter(|s| !s.is_empty());
    let first = |v: &[String]| v.first().and_then(|s| clean(s));
    // Some publishers deposit affiliations as name-only "authors"; when the list
    // also has people, drop name-only entries that read like addresses.
    let has_people = m.author.iter().any(|a| a.family.is_some());
    let authors = m
        .author
        .iter()
        .filter(|a| {
            !(has_people
                && a.family.is_none()
                && a.name
                    .as_deref()
                    .is_some_and(|n| n.contains(',') || n.bytes().any(|b| b.is_ascii_digit())))
        })
        .filter_map(|a| {
            let family = a.family.as_deref().map(squash).filter(|s| !s.is_empty());
            let name = match (&a.given, &family, &a.name) {
                (Some(g), Some(f), _) => squash(&format!("{g} {f}")),
                (None, Some(f), _) => f.clone(),
                (_, None, Some(n)) => squash(n),
                _ => return None,
            };
            Some(Contributor { name, family })
        })
        .collect();
    let dates = DATE_FIELDS
        .iter()
        .zip([
            &m.posted,
            &m.published_online,
            &m.published_print,
            &m.published,
            &m.issued,
        ])
        .filter_map(|(field, d)| Some(((*field).to_owned(), d.as_ref()?.render()?)))
        .collect();
    Ok(Work {
        doi,
        kind: m.kind,
        title: first(&m.title).ok_or("Crossref record has no title")?,
        subtitle: first(&m.subtitle),
        authors,
        dates,
        container: first(&m.container_title),
        volume: m.volume.as_deref().and_then(clean),
        issue: m.issue.as_deref().and_then(clean),
        pages: m
            .page
            .as_deref()
            .or(m.article_number.as_deref())
            .and_then(clean),
        publisher: m.publisher.as_deref().and_then(clean),
        series: m.group_title.as_deref().and_then(clean).or_else(|| {
            m.institution
                .iter()
                .find_map(|i| i.name.as_deref().and_then(clean))
        }),
        abstract_text: m
            .abstract_text
            .as_deref()
            .and_then(clean)
            .map(|a| a.strip_prefix("Abstract ").map(str::to_owned).unwrap_or(a)),
        updates: [
            (Direction::UpdatedBy, m.updated_by),
            (Direction::UpdateTo, m.update_to),
        ]
        .into_iter()
        .flat_map(|(direction, list)| {
            list.into_iter().map(move |u| Update {
                direction,
                kind: u.kind,
                doi: u.doi.map(|d| d.to_lowercase()),
                date: u.updated.as_ref().and_then(DateParts::render),
                source: u.source,
            })
        })
        .collect(),
    })
}

/// Looks up a DOI; `Ok(None)` when Crossref has no record (e.g. a DataCite DOI).
pub fn fetch(http: &Http, doi: &Doi) -> Result<Option<Work>, String> {
    let url = format!("{ENDPOINT}{}", encode(doi.as_str(), b"/"));
    let reply = http.get(&Get::api(&url, &[200, 404]))?;
    if reply.status == 404 {
        return Ok(None);
    }
    parse_work(&reply.text()).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic replies in the Crossref REST API's shape (see tests/fixtures/README.md).
    const JOURNAL: &str = include_str!("../tests/fixtures/crossref-journal.json");
    const SSRN: &str = include_str!("../tests/fixtures/crossref-ssrn.json");
    const NBER: &str = include_str!("../tests/fixtures/crossref-nber.json");
    const RETRACTED: &str = include_str!("../tests/fixtures/crossref-retracted.json");
    const NOTICE: &str = include_str!("../tests/fixtures/crossref-retraction-notice.json");

    #[test]
    fn parses_update_notices() {
        let w = parse_work(RETRACTED).unwrap();
        let kinds: Vec<_> = w.updates.iter().map(|u| u.kind.as_str()).collect();
        assert_eq!(kinds, ["correction", "retraction"]);
        let r = &w.updates[1];
        assert_eq!(r.direction, Direction::UpdatedBy);
        assert!(r.is_retraction() && !w.updates[0].is_retraction());
        assert_eq!(r.doi.as_deref(), Some("10.5555/synth.2019.0042.retr"));
        assert_eq!(r.date.as_deref(), Some("2021-03-09"));
        assert_eq!(r.source.as_deref(), Some("retraction-watch"));
        let n = parse_work(NOTICE).unwrap();
        assert_eq!(n.updates.len(), 1);
        assert_eq!(n.updates[0].direction, Direction::UpdateTo);
        assert_eq!(n.updates[0].doi.as_deref(), Some("10.5555/synth.2019.0042"));
        assert!(parse_work(JOURNAL).unwrap().updates.is_empty());
    }

    #[test]
    fn parses_journal_article() {
        let w = parse_work(JOURNAL).unwrap();
        assert_eq!(w.doi.as_str(), "10.5555/synth.2020.0001");
        assert_eq!(w.kind, "journal-article");
        assert!(w.is_journal_article());
        assert_eq!(w.title, "Widget arbitrage across synthetic exchanges");
        let names: Vec<_> = w.authors.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Ada Quill", "Bram Tolliver"]);
        assert_eq!(w.authors[0].family.as_deref(), Some("Quill"));
        assert_eq!(w.container.as_deref(), Some("Journal of Synthetic Finance"));
        assert_eq!(
            w.citation().as_deref(),
            Some("Journal of Synthetic Finance 12(3), 101-120")
        );
        assert_eq!(w.earliest_year(), Some(2020));
        assert!(
            w.dates
                .contains(&("published-print".into(), "2020-02".into()))
        );
        assert_eq!(w.publisher.as_deref(), Some("Example Academic Press"));
    }

    #[test]
    fn parses_ssrn_posting() {
        let w = parse_work(SSRN).unwrap();
        assert_eq!(w.kind, "posted-content");
        assert!(!w.is_journal_article());
        assert_eq!(w.series.as_deref(), Some("SSRN"));
        assert_eq!(w.container, None);
        assert_eq!(w.dates[0], ("posted".into(), "2025".into()));
        assert_eq!(w.earliest_year(), Some(2025));
        assert_eq!(w.authors.len(), 3);
        assert_eq!(w.authors[1].name, "Corin Ashby Vale");
    }

    #[test]
    fn parses_nber_report() {
        let w = parse_work(NBER).unwrap();
        assert_eq!(w.doi.as_str(), "10.5555/w99001");
        assert_eq!(w.kind, "report");
        assert_eq!(w.title, "Synthetic Stablecoin Runs and Arbitrage Capacity");
        assert_eq!(
            w.series.as_deref(),
            Some("Example Bureau of Economic Research")
        );
        assert_eq!(w.earliest_year(), Some(2025));
        assert_eq!(w.citation(), None);
    }

    #[test]
    fn drops_affiliations_deposited_as_authors() {
        let json = r#"{"message": {"DOI": "10.5555/QFE.2026024", "title": ["T"], "author": [
            {"given": "Tamsin", "family": "Rook", "sequence": "first"},
            {"name": "School of Synthetic Studies, Example University, 1 Example Rd", "sequence": "first"},
            {"given": "Ulric", "family": "Fane", "sequence": "additional"},
            {"name": "The Toy Consortium", "sequence": "additional"}]}}"#;
        let names: Vec<_> = parse_work(json)
            .unwrap()
            .authors
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["Tamsin Rook", "Ulric Fane", "The Toy Consortium"]);
        let org = r#"{"message": {"DOI": "10.1234/x", "title": ["T"], "author": [
            {"name": "Example Settlements Bank, Basel"}]}}"#;
        assert_eq!(parse_work(org).unwrap().authors.len(), 1);
    }

    #[test]
    fn date_parts_render_and_skip_nulls() {
        let d: DateParts = serde_json::from_str(r#"{"date-parts":[[2024,2,16]]}"#).unwrap();
        assert_eq!(d.render().as_deref(), Some("2024-02-16"));
        let d: DateParts = serde_json::from_str(r#"{"date-parts":[[null]]}"#).unwrap();
        assert_eq!(d.render(), None);
        assert!(parse_work("{}").is_err());
    }
}
