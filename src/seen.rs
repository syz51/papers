//! Lookup of works already in the archive (registry entries and paper notes)
//! by DOI, arXiv id (any version), OpenAlex id, normalized URL, or normalized
//! title.

use std::collections::BTreeMap;

use crate::config::Config;
use crate::id::ArxivId;
use crate::notes;
use crate::refs::{Doi, Ref};
use crate::registry::{self, Reference, key_order};
use crate::text;

/// One archive entry that matched a lookup.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hit {
    pub key: String,
    /// `registry` or the note's path.
    pub origin: String,
    /// The identifier that matched: `doi`, `arxiv`, `openalex`, `url`, or `title`.
    pub by: &'static str,
}

/// The origin of a registry entry's hits.
const REGISTRY: &str = "registry";

#[derive(Default)]
pub struct Index {
    map: BTreeMap<String, Vec<(String, String)>>,
    /// Whether any source (registry or notes) existed.
    pub loaded: bool,
    /// The registry's entries, by key.
    registry: BTreeMap<String, Reference>,
}

/// The identifiers a lookup can use.
#[derive(Debug, Default)]
pub struct Query {
    pub doi: Option<String>,
    pub arxiv: Option<String>,
    pub openalex: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
}

fn doi_key(doi: &str) -> Option<String> {
    Doi::parse(doi).map(|d| Ref::from_doi(d).key())
}

fn arxiv_key(id: &str) -> Option<String> {
    ArxivId::parse(id).ok().map(|a| format!("arxiv:{}", a.id))
}

fn openalex_key(id: &str) -> Option<String> {
    let id = crate::openalex::short_id(id.trim());
    (!id.is_empty()).then(|| format!("openalex:{}", id.to_ascii_uppercase()))
}

fn url_key(url: &str) -> Option<String> {
    text::normalize_url(url).map(|u| format!("url:{u}"))
}

fn title_key(title: &str) -> Option<String> {
    let k = text::key(title);
    (!k.is_empty()).then(|| format!("title:{k}"))
}

impl Index {
    /// Indexes the registry and the notes directory, whichever exist.
    pub fn load(config: &Config) -> Result<Index, String> {
        let mut index = Index::default();
        if let Some(records) = registry::load(&config.registry)? {
            index.loaded = true;
            for r in records {
                index.add_registered(r);
            }
        }
        if let Some(all) = notes::load(&config.notes)? {
            index.loaded = true;
            for note in &all {
                if let Err(e) = &note.meta {
                    eprintln!("papers: skipping {}: {e}", note.path.display());
                    continue;
                }
                let path = config.display(&note.path);
                index.add(&note.to_reference(&path, &config.status_depth), &path);
            }
        }
        Ok(index)
    }

    /// An index of registry entries only.
    pub fn of_registry(records: Vec<Reference>) -> Index {
        let mut index = Index {
            loaded: true,
            ..Index::default()
        };
        for r in records {
            index.add_registered(r);
        }
        index
    }

    /// The first note (not the registry) filed under an index key such as
    /// `arxiv:<id>` or `doi:<doi>`, by its path.
    pub fn note(&self, key: &str) -> Option<&str> {
        self.map
            .get(key)?
            .iter()
            .find(|(_, origin)| origin != "registry")
            .map(|(_, origin)| origin.as_str())
    }

    /// Indexes a registry entry and keeps it for [`Index::registered`].
    pub fn add_registered(&mut self, r: Reference) {
        self.add(&r, REGISTRY);
        self.registry.insert(r.key.clone(), r);
    }

    /// The registry entries that match, in key order. Notes are not entries.
    pub fn registered(&self, q: &Query) -> Vec<&Reference> {
        self.lookup(q)
            .iter()
            .filter(|h| h.origin == REGISTRY)
            .filter_map(|h| self.registry.get(&h.key))
            .collect()
    }

    pub fn add(&mut self, r: &Reference, origin: &str) {
        let keys = [
            r.ids.doi.as_deref().and_then(doi_key),
            r.ids.arxiv.as_deref().and_then(arxiv_key),
            r.ids.openalex.as_deref().and_then(openalex_key),
            r.ids.url.as_deref().and_then(url_key),
            // The record's URL may itself name an arXiv paper or a DOI.
            r.ids
                .url
                .as_deref()
                .and_then(|u| Ref::from_note_url(u).map(|r| r.key())),
            r.title.as_deref().and_then(title_key),
        ];
        for k in keys.into_iter().flatten() {
            let entry = self.map.entry(k).or_default();
            let item = (r.key.clone(), origin.to_owned());
            if !entry.contains(&item) {
                entry.push(item);
            }
        }
    }

    pub fn lookup(&self, q: &Query) -> Vec<Hit> {
        let probes = [
            ("doi", q.doi.as_deref().and_then(doi_key)),
            ("arxiv", q.arxiv.as_deref().and_then(arxiv_key)),
            ("openalex", q.openalex.as_deref().and_then(openalex_key)),
            ("url", q.url.as_deref().and_then(url_key)),
            ("title", q.title.as_deref().and_then(title_key)),
        ];
        let mut hits: Vec<Hit> = Vec::new();
        for (by, k) in probes {
            let Some(k) = k else { continue };
            for (key, origin) in self.map.get(&k).into_iter().flatten() {
                if !hits.iter().any(|h| &h.key == key && &h.origin == origin) {
                    hits.push(Hit {
                        key: key.clone(),
                        origin: origin.clone(),
                        by,
                    });
                }
            }
        }
        hits.sort_by(|a, b| key_order(&a.key, &b.key).then_with(|| a.origin.cmp(&b.origin)));
        hits
    }
}

/// The identifiers in user input: an arXiv id or URL, a DOI or DOI URL, an
/// SSRN/NBER URL (mapped to its DOI), or any other http(s) URL.
pub fn parse_input(input: &str) -> Result<Query, String> {
    let url = text::normalize_url(input).map(|_| input.trim().to_owned());
    let (doi, arxiv) = match Ref::parse(input) {
        Ok(Ref::Arxiv(id)) => (None, Some(id.id)),
        Ok(Ref::Doi(doi)) => (Some(doi.as_str().to_owned()), None),
        Err(_) if url.is_some() => (None, None),
        Err(e) => return Err(e),
    };
    Ok(Query {
        doi,
        arxiv,
        url,
        ..Query::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> Index {
        let mut a = Reference::new("12".into());
        a.ids.doi = Some("10.1234/ABC.5".into());
        a.title = Some("Widgets: A Survey".into());
        let mut b = Reference::new("13".into());
        b.ids.arxiv = Some("2601.00001".into());
        b.ids.url = Some("https://arxiv.org/abs/2601.00001v2".into());
        let mut c = Reference::new("14".into());
        c.ids.url = Some("https://www.example.org/report/".into());
        let mut i = Index::default();
        i.add(&a, "registry");
        i.add(&b, "registry");
        i.add(&c, "registry");
        i.add(&a, "notes/2026-doe-widgets.md");
        i
    }

    fn keys(i: &Index, input: &str) -> Vec<String> {
        let mut keys: Vec<String> = i
            .lookup(&parse_input(input).unwrap())
            .into_iter()
            .map(|h| h.key)
            .collect();
        keys.dedup();
        keys
    }

    #[test]
    fn arxiv_dois_find_arxiv_entries() {
        let mut by_id = Reference::new("20".into());
        by_id.ids.arxiv = Some("2601.00003".into());
        let mut by_doi = Reference::new("21".into());
        by_doi.ids.doi = Some("10.48550/arXiv.2601.00004".into());
        let mut i = Index::default();
        i.add(&by_id, "registry");
        i.add(&by_doi, "registry");
        assert_eq!(keys(&i, "10.48550/arXiv.2601.00003"), ["20"]);
        assert_eq!(
            keys(&i, "https://doi.org/10.48550/arxiv.2601.00003"),
            ["20"]
        );
        assert_eq!(keys(&i, "arXiv:2601.00004v2"), ["21"]);
    }

    #[test]
    fn finds_by_each_identifier() {
        let i = index();
        assert_eq!(keys(&i, "https://doi.org/10.1234/abc.5"), ["12"]);
        assert_eq!(keys(&i, "doi:10.1234/ABC.5"), ["12"]);
        assert_eq!(keys(&i, "arXiv:2601.00001v1"), ["13"]);
        assert_eq!(keys(&i, "https://arxiv.org/pdf/2601.00001"), ["13"]);
        assert_eq!(keys(&i, "http://example.org/report#s2"), ["14"]);
        assert!(keys(&i, "https://example.org/other").is_empty());
        assert!(parse_input("not an id").is_err());
        let by_title = i.lookup(&Query {
            title: Some("widgets — a survey".into()),
            ..Query::default()
        });
        assert_eq!(by_title.len(), 2, "{by_title:?}");
        assert!(by_title.iter().all(|h| h.key == "12" && h.by == "title"));
    }

    #[test]
    fn registered_gives_registry_entries_only_and_matches_openalex_ids() {
        let mut i = index();
        let mut d = Reference::new("15".into());
        d.ids.openalex = Some("https://openalex.org/W77".into());
        i.add_registered(d);
        let mut e = Reference::new("9".into());
        e.title = Some("Widgets: a survey".into());
        i.add_registered(e);
        let by_id = Query {
            openalex: Some("w77".into()),
            ..Query::default()
        };
        assert_eq!(i.lookup(&by_id)[0].by, "openalex");
        let keys =
            |q: &Query| -> Vec<String> { i.registered(q).iter().map(|r| r.key.clone()).collect() };
        assert_eq!(keys(&by_id), ["15"]);
        // Key 12 is indexed without its record (as a note is), so only 9 is an entry.
        let by_title = Query {
            title: Some("Widgets a Survey".into()),
            ..Query::default()
        };
        assert_eq!(keys(&by_title), ["9"]);
    }
}
