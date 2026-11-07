# papers

`papers` is a literature CLI for agent-driven research archives. It covers:

- metadata from arXiv, Crossref, OpenAlex, Semantic Scholar, DBLP and CiNii Research;
- a reference registry (`rf.reference/1`);
- duplicate and metadata checks;
- open-access full text with hashes;
- a search log (`rf.search/1`);
- citation graphs;
- BibTeX and CSL-JSON export;
- citation renumbering with IPSJ-style reference lists.

Network access happens only in the commands that need it. Every request goes through one client, which:

- spaces requests per host across processes;
- retries 429, 5xx and transport errors with backoff (5, 10, 20, 40 s, honouring `Retry-After`);
- identifies itself as `papers/<version>`;
- sends no email address or other personal data;
- keeps credentials out of its messages: values of `appid`, `api_key`, `key` and `token` query parameters are printed as `<redacted>`, and API-key headers are dropped when a redirect leaves the host or falls back from https to http.

A batch of arXiv ids whose request still fails after these retries is not split up: the batch fails, and the remaining batches are reported as not attempted. Only a response that cannot be read as a feed is split in half to find the id behind it.

## Install

```sh
cargo install --path .
```

This needs Rust 1.99 or newer (edition 2024). `papers read` can retry a page that answers with a JavaScript check in headless Chromium (see [`read`](#read)). It uses Playwright's `chromium-headless-shell`, or the binary in `PAPERS_CHROME`.

## Commands

| Command | What it does | Example |
| --- | --- | --- |
| `fetch` | Metadata for arXiv ids, DOIs, SSRN/NBER URLs, or `--title` | `papers fetch 2601.00001 10.1234/abc` |
| `search` | Search arXiv (default), OpenAlex, DBLP, Semantic Scholar, or CiNii Research | `papers search --source openalex "retrieval evaluation" --since 2024-01-01` |
| `seen` | Is a work already in the registry or notes? Prints matching keys | `papers seen 10.1234/abc` |
| `registry build` | Parse the catalogue or notes into the registry, keeping tool-added state | `papers registry build` |
| `registry check` | Duplicates, missing ids, bad dates, parse problems, as `rf.finding/1` | `papers registry check --format jsonl` |
| `registry attach` | Record a text file already in the project in an entry's `texts`, with its sha256 | `papers registry attach --key 12 --text sources/text/12.txt --origin <url>` |
| `verify` | Compare registry entries (or notes) with arXiv, Crossref, OpenAlex | `papers verify --keys 12,40 --format jsonl` |
| `read` | Download an open-access PDF and extract its text, with a sha256 manifest | `papers read --key 12 --record` |
| `refs` | Works a paper cites (OpenAlex, else Semantic Scholar) | `papers refs 2601.00001 --max 50` |
| `cited-by` | Works citing a paper | `papers cited-by 10.1234/abc --source s2` |
| `bib` | Export registry entries as BibTeX or CSL-JSON | `papers bib 12 40 --format csl-json` |
| `cite` | Number citations in Markdown by first appearance and print the reference list | `papers cite intro.md method.md --out build/` |
| `stub` | Print, or `--write`, a paper note from the archive's template | `papers stub 2601.00001 --write` |
| `config` | Show the resolved configuration | `papers config --json` |

Paths that `papers` prints (a result's `note`, a written stub, and registry `texts`) are relative to the project root, the directory holding `research.toml`.

`papers <command> --help` lists every option. The global options are:

- `--config <PATH>`: use this `research.toml`.
- `--papers <DIR>`: the notes directory.
- `--refresh`: ignore cached API responses.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. For `seen`, the work is in the archive. |
| 1 | A finding at error severity: `registry check`, `verify`, or `cite` with an unknown key. For `seen`, the work is not in the archive. |
| 2 | Usage error, I/O error, or a failed lookup. |

## Configuration: `research.toml`

`papers` looks for the first `research.toml` found walking up from the current directory; `--config` overrides that search. Relative paths in the file resolve against the file's directory. Without a file, the defaults apply relative to the current directory.

`papers` reads only the shared `[project]` table and its own `[papers]` table. It rejects unknown keys in `[papers]`.

```toml
[project]
state_dir = ".research"             # default

[papers]
notes = "research/papers"           # one Markdown note per paper (default)
template = "research/_templates/paper.md"  # default: <notes>/../_templates/paper.md
registry = "references.jsonl"       # default
catalogue = "docs/references.md"    # numbered Markdown catalogue; none by default
search_log = ".research/papers/search.jsonl"  # default: <state_dir>/papers/search.jsonl
cache_dir = ".research/papers/cache"          # default: <state_dir>/papers/cache
status_depth = { read = "full", skimmed = "abstract" }  # note status -> registry depth
cite_patterns = ['\[@(?P<key>[\w.-]+)\]']    # default: the two patterns under "cite"
cite_replace = "[{n}]"              # default
```

### Environment variables

| Variable | Use |
| --- | --- |
| `OPENALEX_API_KEY` | OpenAlex API key. On macOS, it falls back to the login Keychain item `papers-openalex-api-key`. The key is sent in the `Authorization` header and is never printed, logged or cached. |
| `S2_API_KEY` | Optional Semantic Scholar key, sent as `x-api-key`. |
| `CINII_APPID` | CiNii Research application id, required by `search --source cinii`. It is redacted from error messages. |
| `PAPERS_LOCK_DIR` | Folder for the per-host throttle lock files. The default is `~/Library/Caches/papers-research-tool` on macOS, or the XDG cache folder elsewhere. |
| `PAPERS_CHROME` | Chromium binary for `read`'s browser retry. |

## The registry: `rf.reference/1`

The registry has one JSON object per line, ordered by key. `papers` owns the file; other tools only read it. Unknown values are `null`, never guessed.

The fields are `schema`, `key`, `type`, `title`, `authors`, `year`, `venue`, `ids{doi, arxiv, dblp, openalex, url}`, `version`, `accessed`, `depth`, `texts[{path, sha256, origin}]`, `duplicate_of`, `source{path, line}` and `verified{at, against, status}`.

Optional fields that `papers` adds to the contract:

| Field | Meaning |
| --- | --- |
| `authors_truncated` | `true` when the source lists only the first authors ("et al."). It is omitted when false. `verify` then skips the author count, and exports end the list with "and others" or "et al.". |
| `author_variants` | Other spellings of the first author that an index uses when its metadata misspells the name; `verify` accepts any of them. Copied from a note's frontmatter. It is omitted when empty. |
| `citekey` | A citation key that overrides the generated one in `bib` and `cite`. It is set by hand and kept when the registry is rebuilt. |

Fields that `papers` does not know are kept when it rewrites the registry.

`registry build` reads one of two sources:

- **A catalogue.** A numbered Markdown file with one `## Reference N` section per work. The entry line reads `[N] Authors. **Title**. Venue … [Original](url). Accessed <date>.`
- **Notes.** One Markdown file per paper, with YAML frontmatter.

Every value comes from the text. When the text does not settle a value, it stays `null` and is reported as a parse issue. A rebuild keeps tool-added state (`texts`, `verified`, and fields `papers` does not know) and warns about keys that have disappeared. Keys are never renumbered.

Every write (`registry build`, `verify --record`, `read --record`) takes an exclusive lock on `<registry>.lock` next to the registry, reads the registry afresh, applies its change, and replaces the file atomically. Long runs apply their results at the end, so concurrent runs do not overwrite each other's changes. The lock file holds no data; keep it out of version control.

### `registry check` rules

| Rule | Severity | Meaning |
| --- | --- | --- |
| `duplicate-doi`, `duplicate-arxiv`, `duplicate-url`, `duplicate-title` | warn; info when annotated | Keys share a DOI, an arXiv id (any version), a normalized URL, or a normalized title plus first-author surname. arXiv ids and DOIs also come from arxiv.org, doi.org, SSRN and NBER URLs, and an arXiv DOI (`10.48550/arXiv.<id>`) counts as the arXiv id; such URLs are not compared as URLs. |
| `duplicate-target-missing` | error | `duplicate_of` names a key the registry lacks. |
| `no-identifier` | warn | The entry has no DOI, arXiv id or URL. |
| `malformed-date` | error (warn for reported dates) | An access or reported date is not a calendar date. |
| `invalid-value` | error | `type` or `depth` is outside the `rf.reference/1` vocabulary. |
| `key-mismatch` | error | The heading number and the `[N]` entry number differ, or a note's id and its file name differ. |
| `key-missing-from-source` | warn | A registry key is no longer in the source. |
| `registry-stale` | warn | The registry differs from a fresh parse of the source. |

These parse issues are also reported:

| Rule | Severity | Meaning |
| --- | --- | --- |
| `entry-unnumbered` | warn | The section has no `[N]` line, so its first paragraph was read instead. |
| `entry-missing` | error | The section has no entry line at all. |
| `duplicate-key` | error | Two sections share a number. |
| `duplicate-annotation-conflict` | error | An entry's duplicate annotations name different keys. |
| `ambiguous-id` | info | The entry names several DOIs or arXiv ids, so none is recorded. |
| `authors-unparsed` | info | The author text does not split into names unambiguously. |
| `frontmatter-invalid` | error | A note's frontmatter does not parse. |

### `verify` rules

`verify` checks every registry entry, or the notes when there is no registry. Use `--keys`, `--unverified` or `--limit` to choose fewer. Responses are cached, so a rerun costs no requests. `--record` writes `verified{at, against, status}` back to the registry. An entry whose only DOI is an arXiv DOI is checked against arXiv. An entry with neither a title nor authors is skipped: there is nothing to compare, so it is never recorded as a match.

| Rule | Severity | Meaning |
| --- | --- | --- |
| `verify-title` | error | No source title equals ours; case, accents and punctuation are ignored. |
| `verify-first-author` | error | Our first author lacks the source's first surname. |
| `verify-author-count` | error | The author counts differ. Skipped for "et al." lists. |
| `verify-year` | error | For notes: not the arXiv v1 year, or after the earliest Crossref/OpenAlex date. For the registry: outside the years the source reports. |
| `verify-venue` | warn | Our venue does not name the source's journal. Journal articles only. |
| `verify-not-found` | error (warn for title searches) | The source has no record for our identifier. |
| `verify-failed` | error | The lookup failed; the run exits 2. |
| `verify-manual` | info | No index lists the work, and a note's `checked` date records a hand check. |
| `verify-skipped` | info | There is nothing to look up or compare: a duplicate, a non-paper without a DOI or arXiv id, or an entry with neither title nor authors. |

When an index's metadata is wrong rather than the note, record why in the note's frontmatter instead of copying the error: `former_titles` lists earlier titles an index may still show, `author_variants` lists other spellings of the first author that an index uses (for example a name arXiv lists with a letter missing; `papers registry build` copies it into the registry), and `checked: <date>` marks a work no index lists whose metadata was confirmed by hand.

## `read`

`papers read <ref>` or `papers read --key <key>` downloads an open-access PDF into `--out`, which defaults to `<root>/sources/text`, and extracts its text. The last stdout line is the manifest `{path, sha256, origin}`, and a `<name>.json` sidecar keeps a copy. The sha256 is of the text file.

Running it again skips the download when the stored text still has the recorded hash; `--force` downloads anyway. `--record` (with `--key`) adds the manifest to the entry's `texts`. For a text obtained another way (by hand, or listed in an existing manifest), `papers registry attach --key <key> --text <path> [--origin <url>]` records it the same way: the path must be inside the project root, and a text already recorded at that path is replaced.

Unless `--no-browser` is given, a candidate retries in headless Chromium only when its plain-HTTP reply was a JavaScript check page: a 202 HTML reply (how AWS WAF serves its challenge, as on figshare) or a small 200 HTML page with a challenge marker. A 403, a 429, any other refusal, and transport errors are reported and never retried in the browser. When a host blocks automated browsers, `papers` reports the block. It does no CAPTCHA solving or fingerprint spoofing.

## Search log: `rf.search/1`

Every `search`, `fetch`, `refs` and `cited-by` appends one JSON line per query to `[papers] search_log`. `--no-log` turns this off. The log means a literature review can show what was searched, when, and what came back, without reconstructing it later.

```json
{"schema":"rf.search/1","at":"2026-11-07T03:00:00Z","source":"arxiv","query":"retrieval evaluation","params":{"max":20,"since":"2024-01-01","categories":[],"sort":"relevance","arxiv_query":"all:retrieval AND all:evaluation","total":130},"results":[{"id":"arxiv:2601.00001v2","title":"…","year":2026}],"seen":["12"]}
```

| Field | Meaning |
| --- | --- |
| `at` | RFC 3339 UTC. |
| `source` | `arxiv`, `crossref`, `openalex`, `dblp`, `s2` or `cinii`. |
| `query` | The user's words, or the work for `refs` and `cited-by`. |
| `params` | Source-specific options. `fetch`, `refs` and `cited-by` record `{"command": …}`. |
| `results[].id` | Source-prefixed: `arxiv:IDvN`, `doi:…`, `openalex:W…`, `dblp:…`, `s2:…` or `cinii:…`. |
| `seen` | Archive keys (registry or notes) among the results. |

## Cache

`verify`, `fetch` and the citation-graph commands cache successful API lookups under `<cache_dir>/http/<host>/<sha256 of URL>.json` for 7 days. `--refresh` bypasses the cache.

Searches and PDF downloads are never cached. API keys travel in headers and never appear in cached URLs.

## Export: `bib`

`papers bib [KEY…] --format bibtex|csl-json` prints entries in the order given. With no keys, it prints every entry that is not a duplicate. A key marked `duplicate_of` exports its canonical entry, following the chain to its end; a cycle or a missing target is an error.

Citation keys are `<first author's surname><year><first significant title word>`, in lowercase ASCII:

- Without authors the surname part is `anon`; without a year, the year part is `nd`.
- When keys collide, they take `a`, `b`, … in registry key order across the whole registry, so a subset export never changes a key.
- A `citekey` field overrides the generated key. Generated keys skip every pinned key, taking the next suffix instead. Two entries that pin the same `citekey` are an error in `bib` and `cite`.

How entries map:

| Registry entry | BibTeX | CSL-JSON |
| --- | --- | --- |
| Venue with conference words (Proceedings, Conference, Workshop, Symposium…) | `@inproceedings`, `booktitle` | `paper-conference` |
| DOI and a venue | `@article`, `journal` | `article-journal` |
| arXiv only | `@misc` with `eprint` and `archiveprefix` | `article`, publisher arXiv |
| `web`, `software`, `dataset` or `standard` | `@misc`, `howpublished`, `url`, `urldate` | `webpage`, `software`, `dataset` or `standard` |
| Other | `@misc` (venue as `note`) | `document` |

CSL items carry the registry key in `custom.registry_key`.

## Citations: `cite`

`papers cite DOC… [--out DIR]` works in four steps:

1. It scans the documents in the order given, skipping fenced code blocks and inline code.
2. It numbers the cited works by first appearance.
3. It rewrites each citation with `--replace`, which defaults to `[{n}]`. The template can use `{n}`, `{key}` and `{citekey}`.
4. It prints the reference list. `--json` prints one `{n, key, citekey, text}` object per line instead.

`cite` never rewrites its input documents: they keep their citation keys, so it can run again after every edit. `--out DIR` writes the renumbered copies into `DIR` under their file names; a copy that would replace its own input, and two inputs with the same file name, are refused. All copies are written to temporary files first and only then moved into place. Without `--out`, nothing is written. If any citation key is unknown, `cite` lists each one as `file:line:column`, writes nothing, and exits 1.

The default patterns are:

- `[N](references.md#reference-KEY)`, a link to a catalogue entry (any path ending in `#reference-KEY`);
- `[@KEY]`, where KEY is a registry key or a citation key.

`--pattern` (repeatable) or `[papers] cite_patterns` replaces the defaults. Each pattern is a regex with a `(?P<key>…)` group.

### Style `ipsj`

The `ipsj` style follows the IPSJ Journal author guidance (情報処理学会論文誌 原稿執筆案内) at https://www.ipsj.or.jp/journal/submit/ronbun_j_prms.html, including its appendix 付録 参考文献の記載方法. The page was accessed on 2026-11-07; a test pins that date.

| Entry | Form |
| --- | --- |
| Journal | `Family, I.: Title, Journal (online), DOI: … (Year).` |
| Conference | `Family, I.: Title, Proc. Name (Year).` |
| Book | `Family, I.: Title, Publisher (Year).` |
| Web, software, dataset | `Author: Page title, Site (online), available from <URL>(accessed YYYY-MM-DD).` |
| Preprint | `Family, I.: Title, arXiv:ID (online), DOI: 10.48550/arXiv.ID (Year).` |

Names and author lists:

- English names are `Family, I.`. Japanese and organization names are given as written.
- One to three authors are listed in full, as `A, B and C`.
- Four or more authors, or a list marked `authors_truncated`, show the first three followed by `, et al.` (Japanese: ほか). The guidance permits this shortening; it does not require it.

Punctuation:

- Entries with Japanese titles use full-width punctuation: `，` `：` `．` `（2005）` `（オンライン）` `入手先<URL>（参照YYYY-MM-DD）`.
- English entries use half-width punctuation.

Known gaps:

- **No volume, issue or pages.** The registry has no fields for them, so entries never print `Vol.`, `No.` or `pp.`.
- **DOI on conference papers.** The guidance prints a DOI only for electronic journals. `papers` also adds a DOI to conference papers that have one.
- **No preprint rule.** The guidance has no rule for preprints or technical reports. The preprint form above extends its electronic-journal form, with the DOI that arXiv registers. Treat the preprint form as unverified.

## Sources and rate limits

The documentation listed here was checked on 2026-11-07. Behaviour marked "live" was confirmed by requests that day.

| Source | Spacing in `papers` | What the provider documents |
| --- | --- | --- |
| arXiv API (`export.arxiv.org`) | 3 s | arXiv's API terms of use (https://info.arxiv.org/help/api/tou.html) ask for one request every three seconds. The page was not re-read in this session. |
| Crossref (`api.crossref.org`) | 1 s | Not re-checked in this session. |
| OpenAlex (`api.openalex.org`) | 1 s | https://help.openalex.org/api/authentication/ and https://help.openalex.org/api/llm-quick-reference/ (pages dated 2026-11-03; docs.openalex.org redirects there). See the notes below the table. |
| Semantic Scholar (`api.semanticscholar.org`) | 1 s | https://www.semanticscholar.org/product/api documents 1000 requests per second shared by all unauthenticated users, and 1 request per second with an introductory key. See the notes below the table. |
| DBLP (`dblp.org`) | 1 s | Not retrievable. See the notes below the table. |
| CiNii Research OpenSearch (`cir.nii.ac.jp`) | 1 s | https://support.nii.ac.jp/en/cir/r_opensearch (updated 2025-11-25). See the notes below the table. |

**OpenAlex**
- Limits: 10 requests/s without a key, 30 with a free key. The daily budget is $0.10 without a key and $1 with one. List calls cost $0.0001 and searches $0.001.
- 429 responses carry `Retry-After`. An OR filter takes at most 100 values. `per_page` is at most 100.
- Live: `cites:W…` lists citing works, `referenced_works` holds URL-form ids, and `/works/doi:10.48550/arxiv.<id>` resolves arXiv papers.

**Semantic Scholar**
- Unauthenticated requests often got 429 in practice; the backoff retries succeeded.
- The `/paper/search`, `/paper/{ARXIV:id|DOI:doi}/references` and `/citations` endpoints were confirmed live. The API reference pages did not load (navigation shell, then 429).

**DBLP**
- The search API FAQ (https://dblp.org/faq/How+to+use+the+dblp+search+API.html) and the API itself (`dblp.org` and `dblp.uni-trier.de`) answered with an Anubis bot challenge (HTTP 202 for the API).
- `papers` reports the challenge and does not solve it. The parser follows dblp's published JSON format (`result.hits.hit[].info`) but is unverified against a live response.

**CiNii Research OpenSearch**
- Endpoint: `/opensearch/articles` with `q`, `count` (1–200), `from=YYYY` and `format=json`.
- The page marks `appid` as required. Register at https://support.nii.ac.jp/en/cinii/api/developer. The server answered without one on 2026-11-07, but `papers` still requires `CINII_APPID`.
- No request rate is documented. The Web API usage regulations PDF was not read.
- Live: items carry `@id`, `title`, `dc:creator`, `prism:publicationName`, `prism:publicationDate` and DOIs in `dc:identifier` (`cir:DOI`).

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The tests use synthetic records and public metadata fixtures only, and they make no network requests.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option.
