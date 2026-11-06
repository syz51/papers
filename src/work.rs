//! A DOI work resolved through Crossref (authority) and OpenAlex (abstract,
//! open access), and the comparable record verify and stub use.

use crate::crossref;
use crate::http::Http;
use crate::openalex;
use crate::refs::Doi;
use crate::text::surname;

/// The metadata a note is checked against, from one source.
#[derive(Debug, Clone)]
pub struct Record {
    pub source: &'static str,
    /// Accepted titles; the first is the display title.
    pub titles: Vec<String>,
    pub authors: Vec<String>,
    pub first_surname: Option<String>,
    pub earliest_year: Option<i64>,
    pub latest_year: Option<i64>,
    /// What the year came from, e.g. `posted 2025-01, issued 2025`.
    pub year_basis: String,
    /// Container title, for journal articles only.
    pub journal: Option<String>,
}

impl From<&crossref::Work> for Record {
    fn from(w: &crossref::Work) -> Self {
        let mut titles = vec![w.title.clone()];
        if let Some(sub) = &w.subtitle {
            titles.push(format!("{}: {sub}", w.title));
        }
        let first_surname = w.authors.first().map(|a| {
            a.family
                .clone()
                .unwrap_or_else(|| surname(&a.name).to_owned())
        });
        Record {
            source: "Crossref",
            titles,
            authors: w.authors.iter().map(|a| a.name.clone()).collect(),
            first_surname,
            earliest_year: w.earliest_year(),
            latest_year: w
                .dates
                .iter()
                .filter_map(|(_, d)| d.get(..4)?.parse().ok())
                .max(),
            year_basis: w
                .dates
                .iter()
                .map(|(f, d)| format!("{f} {d}"))
                .collect::<Vec<_>>()
                .join(", "),
            journal: w
                .is_journal_article()
                .then(|| w.container.clone())
                .flatten(),
        }
    }
}

impl From<&openalex::Work> for Record {
    fn from(w: &openalex::Work) -> Self {
        Record {
            source: "OpenAlex",
            titles: vec![w.title.clone()],
            authors: w.authors.clone(),
            first_surname: w.authors.first().map(|a| surname(a).to_owned()),
            earliest_year: w.publication_year,
            latest_year: w.publication_year,
            year_basis: w
                .publication_date
                .as_deref()
                .map_or_else(String::new, |d| format!("publication_date {d}")),
            journal: w.journal_article.then(|| w.venue.clone()).flatten(),
        }
    }
}

/// A DOI with whatever each source knows about it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub doi: Doi,
    pub crossref: Option<crossref::Work>,
    pub openalex: Option<openalex::Work>,
}

impl Resolved {
    /// Looks the DOI up in both sources; `Ok(None)` when neither knows it.
    pub fn fetch(http: &Http, doi: &Doi) -> Result<Option<Resolved>, String> {
        let crossref = crossref::fetch(http, doi)?;
        let openalex = openalex::Client::new(http).by_doi(doi)?;
        Ok(
            (crossref.is_some() || openalex.is_some()).then(|| Resolved {
                doi: doi.clone(),
                crossref,
                openalex,
            }),
        )
    }

    /// Crossref's record when it has one, else OpenAlex's.
    pub fn record(&self) -> Record {
        match (&self.crossref, &self.openalex) {
            (Some(c), _) => c.into(),
            (None, Some(o)) => o.into(),
            (None, None) => unreachable!("Resolved holds at least one source"),
        }
    }

    /// Earliest year any source reports, with the per-source years.
    pub fn earliest_year(&self) -> (Option<i64>, String) {
        let years: Vec<(&str, i64)> = [
            (
                "Crossref",
                self.crossref.as_ref().and_then(|c| c.earliest_year()),
            ),
            (
                "OpenAlex",
                self.openalex.as_ref().and_then(|o| o.publication_year),
            ),
        ]
        .into_iter()
        .filter_map(|(s, y)| Some((s, y?)))
        .collect();
        let detail = years
            .iter()
            .map(|(s, y)| format!("{s} {y}"))
            .collect::<Vec<_>>()
            .join(", ");
        (years.iter().map(|(_, y)| *y).min(), detail)
    }

    pub fn abstract_text(&self) -> Option<&str> {
        self.openalex
            .as_ref()
            .and_then(|o| o.abstract_text.as_deref())
            .or_else(|| {
                self.crossref
                    .as_ref()
                    .and_then(|c| c.abstract_text.as_deref())
            })
    }

    /// Venue for a note: container plus volume(issue), pages and year when
    /// present, else the publisher series or publisher.
    pub fn venue(&self) -> Option<String> {
        if let Some(c) = &self.crossref {
            let year = c
                .issued_year()
                .map(|y| format!(" ({y})"))
                .unwrap_or_default();
            return c
                .citation()
                .or_else(|| c.series.clone())
                .or_else(|| c.publisher.clone())
                .map(|v| v + &year);
        }
        let o = self.openalex.as_ref()?;
        let year = o
            .publication_year
            .map(|y| format!(" ({y})"))
            .unwrap_or_default();
        o.venue.clone().map(|v| v + &year)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOURNAL: &str = include_str!("../tests/fixtures/crossref-journal.json");
    const NBER: &str = include_str!("../tests/fixtures/crossref-nber.json");
    const OA_JOURNAL: &str = include_str!("../tests/fixtures/openalex-journal.json");
    const OA_REPORT: &str = include_str!("../tests/fixtures/openalex-report.json");

    fn resolved(cr: Option<&str>, oa: Option<&str>) -> Resolved {
        let crossref = cr.map(|j| crossref::parse_work(j).unwrap());
        let openalex = oa.map(|j| openalex::parse_work(j).unwrap());
        let doi = crossref
            .as_ref()
            .map(|c| c.doi.clone())
            .or_else(|| openalex.as_ref().and_then(|o| o.doi.clone()))
            .unwrap();
        Resolved {
            doi,
            crossref,
            openalex,
        }
    }

    #[test]
    fn earliest_year_spans_sources() {
        let r = resolved(Some(JOURNAL), Some(OA_JOURNAL));
        assert_eq!(
            r.earliest_year(),
            (Some(2018), "Crossref 2020, OpenAlex 2018".into())
        );
        let rec = r.record();
        assert_eq!(rec.source, "Crossref");
        assert_eq!(rec.earliest_year, Some(2020));
        assert_eq!(rec.first_surname.as_deref(), Some("Quill"));
        assert_eq!(rec.journal.as_deref(), Some("Journal of Synthetic Finance"));
        assert_eq!(
            r.venue().as_deref(),
            Some("Journal of Synthetic Finance 12(3), 101-120 (2020)")
        );
    }

    #[test]
    fn venue_falls_back_to_series_and_openalex() {
        let r = resolved(Some(NBER), None);
        assert_eq!(
            r.venue().as_deref(),
            Some("Example Bureau of Economic Research (2025)")
        );
        assert_eq!(r.record().journal, None);
        let r = resolved(None, Some(OA_REPORT));
        assert_eq!(r.record().source, "OpenAlex");
        assert_eq!(
            r.venue().as_deref(),
            Some("Example Central Bank Research (2026)")
        );
        assert!(
            r.abstract_text()
                .unwrap()
                .starts_with("We study decentralized lending")
        );
    }
}
