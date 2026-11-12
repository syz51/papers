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
| `registry attach` | Record a text file already in the project in an entry's `texts`, with its sha256, and update its manifest | `papers registry attach --key 12 --text sources/text/12.txt --origin <url> --extraction layout` |
| `registry record-pdfs` | Record `pdf_sha256` for texts saved before `read` recorded it, when the saved PDF reproduces the text; no network request | `papers registry record-pdfs --dry-run` |
| `registry abstracts` | Fill the `abstract` field of entries that have none, from arXiv and OpenAlex by id; no PDF download | `papers registry abstracts --dry-run` |
| `card` | Make a paper card (`rf.paper-card/1`) from an entry's local text with one Claude call; keep only items whose quotes check. Off unless `[papers] cards = true` | `papers card --key 12` |
| `structure` | Write the `rf.paper-structure/1` sidecar of texts: pages, section spans, the reference list and the registry entries it cites | `papers structure --all` |
| `verify` | Compare registry entries (or notes) with arXiv, Crossref, OpenAlex | `papers verify --keys 12,40 --format jsonl` |
| `read` | Download an open-access PDF and extract its text (plain or layout), with a sha256 manifest; or convert the saved PDF with Docling as a second text | `papers read --key 12 --record --extraction layout` |
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
- `--refresh`: ignore cached API responses, and fetch a registered paper too.

### Registered papers first

`fetch`, `read` and `search` report a paper that the registry already holds before anything else, so that nobody fetches or reads it twice. [docs/interfaces.md](docs/interfaces.md) defines the exact lines.

- `papers fetch <ref>` for a registered paper prints its key, depth, title and each recorded text path, makes no request, and exits 0. `--refresh` fetches the metadata too.

  ```text
  registered: key 87 · depth full · A Synthetic Title
  text: sources/text/87.txt
  ```

- `papers read <ref>` prints the same block. When a recorded text still has its recorded sha256, `read` prints its path and manifest and downloads nothing. `--force` downloads.
- `papers search`, `refs` and `cited-by` start each result that the archive holds with `seen: key 87`. They match by DOI, arXiv id, OpenAlex id, URL and normalised title.
- `papers search` prints the time and result count of the last run of the same search, from the search log, on stderr. The same search has the same source, query and `--since`, and for arXiv the same `--category`: `papers: searched before: last run 2026-11-07T03:00:00Z, 20 results (1 run of this arxiv query in .research/papers/search.jsonl)`.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. For `seen`, the work is in the archive. |
| 1 | A finding at error severity: `registry check`, `verify`, a parse issue in `registry build`, or `cite` with an unknown key. For `card`, the card kept under 80% of its items and was not stored. For `seen`, the work is not in the archive. |
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
docling = ["uv", "run", "--quiet", "--no-project", "--with", "docling==2.150.0", "python", "-I"]  # default: the command that runs Python with Docling
cards = false                       # default; `true` turns on `papers card`
card_dir = "sources/cards"          # default: where cards are stored, one <key>.json each
claude = ["claude"]                 # default: the command that runs Claude Code
card_model = "claude-haiku-5-5"    # default: the model a card is made with
status_depth = { read = "full" }    # note status -> registry depth; default read = "method", skimmed = "abstract"
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
| `texts[].retrieved` | When `read` downloaded the text, as a UTC RFC 3339 time. It is omitted for texts attached by hand without `--retrieved` and for texts recorded before `papers` added it. |
| `texts[].extraction` | How the text was extracted from its PDF: `plain`, `layout` (`pdftotext -layout`) or `docling` (Docling's Markdown, the entry's second text). It is omitted for texts attached by hand without `--extraction` and for texts recorded before `papers` added it; `read` treats those as `plain`. Another value, such as one a later version writes, is kept as it is; `read` does not re-extract such a text without `--extraction`. |
| `texts[].pdf_sha256` | The sha256 of the PDF that `read` extracted the text from, saved beside the text as `<name>.pdf`. It is omitted for texts attached by hand and for texts recorded before `papers` added it; `registry record-pdfs` adds it to those whose saved PDF reproduces the text. |
| `texts[].extractor` | For a Docling text: `docling <version>`, the Docling version that converted the PDF. |

| `abstract`, `abstract_source` | The work's abstract as one string with single spaces, and where it is from (`arxiv:<id>v<n>` or `openalex:<W id>`). Both are absent when no abstract is recorded. [`registry abstracts`](#registry-abstracts) adds them. |

Fields that `papers` does not know are kept when it rewrites the registry, in entries and in `texts` objects.

`registry build` reads one of two sources:

- **A catalogue.** A numbered Markdown file with one `## Reference N` section per work. The entry line reads `[N] Authors. **Title**. Venue … [Original](url). Accessed <date>.`
  An entry's reading depth comes from a line `Inspection depth: <level>`, where the level is one of `metadata`, `abstract`, `method`, `full`, `artifact` or `reproduced`. That line wins over any prose in the entry. Without one, the depth comes from `Inspection:` lines that name exactly one level in plain words, such as "abstract only" or "read in full"; when they name none or several, it stays `null`. `Inspection depth: unknown` followed by a reason, such as `Inspection depth: unknown. Reason: only a historical record is kept.`, records that the depth is not known: the depth is `null` and nothing is reported. Any other entry left with a `null` depth is reported as `depth-missing`.
- **Notes.** One Markdown file per paper, with YAML frontmatter.
  An entry's reading depth comes from the note's `status`: `read` gives `method` and `skimmed` gives `abstract`. `read` does not give `full`, because the status does not show that the whole text was read. `[papers] status_depth` replaces these values or maps other statuses. A `depth` field in the frontmatter wins over the status. It must be one of `metadata`, `abstract`, `method`, `full`, `artifact` or `reproduced`; otherwise the parse issue `depth-invalid` is reported and the entry keeps the depth already in the registry. A note with another status or no status keeps the depth already in the registry, or `null`.

Every value comes from the text. When the text does not settle a value, it stays `null` and is reported as a parse issue. `registry build` prints each parse issue at error severity as `file:line: rule key: message`. It still writes the registry, so one bad entry does not hold back the others, and then exits 1. When an entry's main text cannot be read (`entry-missing`, `frontmatter-invalid`), each field the text no longer gives keeps its recorded value. Values the text still gives, such as a section's labelled lines, are updated. Warnings alone leave the exit code at 0. `--dry-run` reports the same issues and exit code without writing. A rebuild keeps tool-added state (`texts`, `verified`, and fields `papers` does not know) and warns about keys that have disappeared. Keys are never renumbered.

Every write (`registry build`, `registry attach`, `registry record-pdfs`, `registry abstracts`, `verify --record`, `read --record`) takes an exclusive lock on `<registry>.lock` next to the registry, reads the registry afresh, applies its change, and replaces the file atomically. Long runs apply their results at the end, so concurrent runs do not overwrite each other's changes. The lock file holds no data; keep it out of version control.

### `registry check` rules

| Rule | Severity | Meaning |
| --- | --- | --- |
| `duplicate-doi`, `duplicate-arxiv`, `duplicate-url`, `duplicate-title` | warn; info when annotated | Keys share a DOI, an arXiv id (any version), a normalized URL, or a normalized title plus first-author surname. arXiv ids and DOIs also come from arxiv.org, doi.org, SSRN and NBER URLs, and an arXiv DOI (`10.48550/arXiv.<id>`) counts as the arXiv id; such URLs are not compared as URLs. |
| `duplicate-target-missing` | error | `duplicate_of` names a key the registry lacks. |
| `duplicate-cycle` | error | Following `duplicate_of` from a key comes back to it, such as `1 → 2 → 1`. It is reported once, at the cycle's least key. |
| `duplicate-chain` | warn | `duplicate_of` names a key that is itself a duplicate. The message names the key the chain ends at, which `duplicate_of` should name. |
| `verified-mismatch` | warn | `verify --record` recorded `mismatch` or `not-found` in `verified.status`. Check the entry, then run `verify --keys <key> --record` again. |
| `no-identifier` | warn | The entry has no DOI, arXiv id or URL. |
| `malformed-date` | error (warn for reported dates) | An access date, a reported date or `verified.at` is not a calendar date. |
| `invalid-value` | error | `type` or `depth` is outside the `rf.reference/1` vocabulary, or `verified.status` is not `match`, `mismatch` or `not-found`. |
| `key-mismatch` | error | The heading number and the `[N]` entry number differ, or a note's id and its file name differ. |
| `key-missing-from-source` | warn | A registry key is no longer in the source. |
| `registry-stale` | warn | The registry differs from a fresh parse of the source. |
| `derivative-stale` | warn | A file made from a text or PDF records an input sha256 that the input no longer has: a structure sidecar whose text changed, a Docling text whose `<name>.pdf` changed, or a paper card whose text changed. The message names the command that makes it again. Inputs that are not in the checkout, such as git-ignored texts in a worktree, are skipped. |

These parse issues are also reported:

| Rule | Severity | Meaning |
| --- | --- | --- |
| `entry-unnumbered` | warn | The section has no `[N]` line, so its first paragraph was read instead. |
| `entry-missing` | error | The section has no entry line at all. Fields only the entry line gave keep their recorded values. |
| `duplicate-key` | error | Two sections share a number. |
| `duplicate-annotation-conflict` | error | An entry's duplicate annotations name different keys. |
| `ambiguous-id` | info | The entry names several DOIs or arXiv ids, so none is recorded. |
| `authors-unparsed` | info | The author text does not split into names unambiguously. |
| `depth-unparsed` | warn | An `Inspection depth:` line names a single word outside the vocabulary, or two such lines name different levels; the depth stays `null`. A longer value is read as prose. |
| `depth-missing` | warn | A catalogue entry has no depth and does not state `Inspection depth: unknown` with a reason, or states `unknown` without one. It is reported at the heading, or at the `unknown` line. Notes are not checked: a note's depth comes from its status. |
| `frontmatter-invalid` | error | A note's frontmatter does not parse. The entry keeps its recorded values. |
| `depth-invalid` | error | A note's `depth` field is outside the vocabulary. The entry keeps the depth already in the registry. |

### `verify` rules

`verify` checks every registry entry, or the notes when there is no registry. Use `--keys`, `--unverified` or `--limit` to choose fewer. Responses are cached, so a rerun costs no requests. `--record` writes `verified{at, against, status}` back to the registry: `at` is the date, `against` the index, and `status` is `match`, `mismatch` or `not-found`. Only `verify --record` sets the field, and `registry build` keeps it while the checked metadata is unchanged. `registry check` checks its values and reports a recorded `mismatch` or `not-found` as `verified-mismatch`. An entry never checked has `verified: null`. An entry whose only DOI is an arXiv DOI is checked against arXiv. An entry with neither a title nor authors is skipped: there is nothing to compare, so it is never recorded as a match.

Withdrawal and retraction findings are notices about the work, not metadata differences: they leave `verified.status` as `match`. The registry has no field for them; each `verify` run reports them again. Only a paper with more than one arXiv version can have a withdrawn version, so `verify` reads the version history (one request to `oaipmh.arxiv.org`, cached like the others) only for those papers.

| Rule | Severity | Meaning |
| --- | --- | --- |
| `verify-title` | error | No source title equals ours; case, accents and punctuation are ignored. |
| `verify-first-author` | error | Our first author lacks the source's first surname. |
| `verify-author-count` | error | The author counts differ. Skipped for "et al." lists. |
| `verify-year` | error | For notes: not the arXiv v1 year, or after the earliest Crossref/OpenAlex date. For the registry: outside the years the source reports. |
| `verify-venue` | warn | Our venue does not name the source's journal. Journal articles only. |
| `verify-withdrawn` | warn | arXiv withdrew a version of the paper, such as `arXiv v2 withdrawn 2025-11-12; ours pins v1`. arXiv's OAI-PMH version history marks it; the Atom API does not. Or the arXiv comment says the paper is withdrawn. |
| `verify-retracted` | warn | Crossref lists a retraction, withdrawal or removal notice for the DOI (`updated-by`), from the publisher or Retraction Watch. |
| `verify-notice` | warn | The DOI is itself a notice, such as a retraction (`update-to`). Cite the work it changes. |
| `verify-updated` | info | Crossref lists another notice for the DOI, such as a correction or an expression of concern. |
| `verify-not-found` | error (warn for title searches) | The source has no record for our identifier. |
| `verify-failed` | error | A lookup failed, including the arXiv version history; the run exits 2. |
| `verify-manual` | info | No index lists the work, and a note's `checked` date records a hand check. |
| `verify-skipped` | info | There is nothing to look up or compare: a duplicate, a non-paper without a DOI or arXiv id, or an entry with neither title nor authors. |

When an index's metadata is wrong rather than the note, record why in the note's frontmatter instead of copying the error: `former_titles` lists earlier titles an index may still show, `author_variants` lists other spellings of the first author that an index uses (for example a name arXiv lists with a letter missing; `papers registry build` copies it into the registry), and `checked: <date>` marks a work no index lists whose metadata was confirmed by hand. `former_titles` and `author_variants` take a list or a single string.

## `read`

`papers read <ref>` or `papers read --key <key>` downloads an open-access PDF into `--out` and extracts its text. Without `--out`, the directory is that of the texts of the paper's registry entry (the one `--key` names, else the first that lists the reference), else the directory that holds most of the registry's texts, else `<root>/sources/text`. A project that keeps its texts in `docs/research/sources/text` needs no `--out`. With `--key`, files are named after the key, unless the entry's only plain or layout text in that directory has another name and no file named after the key is there. Then `--extraction docling` names its files after that text: `coral.txt` gives `coral.pdf` and `coral.docling.md`. A plain or layout read does so only when `coral.json`, the manifest `read` writes, is there, so it never overwrites a text attached by hand. The last stdout line is the manifest `{path, sha256, origin, retrieved, extraction, pdf_sha256}`, and a `<name>.json` sidecar keeps a copy. `read` also writes the text's [structure sidecar](#structure-sidecars-rfpaper-structure1) and prints `structure: <path>` before the manifest. The sha256 is of the text file, `retrieved` is the UTC time of the download (RFC 3339), `extraction` is the extraction mode, and `pdf_sha256` is the sha256 of the PDF, which is saved as `<name>.pdf` beside the text. Texts attached by hand have no `retrieved` unless `--retrieved` gives one.

`--extraction` chooses how the text is extracted:

- `plain`, the default, gives the text in reading order, as described below.
- `layout` runs `pdftotext -layout`, which keeps columns and table alignment. It needs Poppler's `pdftotext` on `PATH`; without it, `read` stops with an error before it downloads or extracts anything. When `pdftotext` fails on the PDF, the PDF is kept and no text is written. There is no fallback to `pdf-extract`.
- `docling` converts the PDF with [Docling](#docling-a-second-text) into a second text, `<name>.docling.md`. The plain or layout text stays as it is.

Write order: a plain or layout text is first written to `<name>.txt.tmp`. `read` then records it in the registry (with `--record`), writes the `<name>.json` manifest, and only then renames the text into place. When a step fails, the steps before it are undone, as for a [Docling text](#docling-a-second-text): a reader never finds a new text without its record.

Without `--extraction`, `read` uses the mode recorded for the text it would replace: the stored text that still has its recorded hash, else a text recorded at the same path in the sidecar or the registry entry. So a layout text stays layout when it is extracted again. Other texts of the entry do not set the mode. In plain mode the text comes from the `pdf-extract` crate. When it fails, panics or finds no text, `papers read` runs Poppler's `pdftotext` on the saved PDF, if it is on `PATH`. It also runs `pdftotext` when the paper's title is missing from `pdf-extract`'s text, as when that text is garbled. It then keeps `pdftotext`'s text only if that text contains the title; otherwise it keeps `pdf-extract`'s text and prints the title warning. A `pdftotext` run that takes longer than 60 seconds, or prints more than 64 MiB, is stopped and reported. The `text:` line names the extractor that produced the text. When neither extractor gives text, the PDF is kept, no text is written, and the `text:` line gives both reasons, such as `pdftotext is not on PATH`.

`<ref>` can also be the http(s) URL of a PDF or of a landing page, for open copies that have no arXiv id or DOI, such as a central bank's bulletin or a university repository's file:
- A URL ending in `.pdf` is downloaded directly.
- Any other URL is read as a landing page. Its `citation_pdf_url` meta tag and same-host PDF links are tried in order.
- The files are named after the URL's host and last path segment, plus 8 hex digits of its hash, such as `bis.org_bisbull90_1a2b3c4d`.
- When the landing page names a DOI in its metadata (`citation_doi`, `dc.identifier`, `prism.doi`), the DOI's own open-access locations are tried next, then other versions with the same title. This happens only when the page and its links yield no PDF, including after the browser retry described below.
- The manifest's `origin` is the URL the PDF came from.

Running it again skips the download when the stored text still has the recorded hash. `--force` downloads anyway, and extracts in the recorded mode. An explicit `--extraction` that differs from the stored text's mode extracts the text again; this is the only way `read` replaces a layout text with plain text. When the stored text's record, in its sidecar or the registry entry, has a `pdf_sha256` and `<name>.pdf` in `--out` still has it, `read` extracts from that PDF and makes no network request; the manifest keeps the PDF's `origin` and `retrieved`. Without `--key`, there is then no title for plain mode's title check. Otherwise, as for manifests written before `papers` recorded `pdf_sha256`, it downloads the PDF again; [`registry record-pdfs`](#registry-record-pdfs) records `pdf_sha256` for those manifests when the saved PDF reproduces the text. Either way it says which. An explicit mode that matches a stored text without a recorded mode (plain) is recorded for it. `--record` (with `--key`) adds the manifest to the entry's `texts`.

For a text obtained another way (by hand, or listed in an existing manifest), `papers registry attach --key <key> --text <path> [--origin <url>] [--retrieved <time>] [--extraction plain|layout|docling]` records it the same way:

- The file must be non-empty UTF-8 text, and its path must be inside the project root. The path is taken as written, after `.` and `..` are removed. It is inside the project when one of its directories is the project root, under any name.
- Directories below the root may be symlinks. A `sources` directory linked to shared storage is accepted, and the text is recorded under its path in the project.
- A symlinked file must point into the project or into its own directory; a link to a file elsewhere is refused. A `..` after a symlinked directory is refused when it leads to another file than the path without it.
- A text already recorded at that path is replaced. It keeps the replaced text's origin when `--origin` is not given.
- When the file is unchanged, the recorded `retrieved`, `extraction`, `pdf_sha256` and unknown fields are kept too, unless `--retrieved` or `--extraction` gives new values. They come from the manifest beside the text first, then from the replaced text. When the file changed, they are dropped, and `attach` prints a note for `retrieved` and `extraction`; pass the options to record them.
- `--retrieved` takes an RFC 3339 time and records it in UTC, to the second.
- `attach` reads the manifest `read` left beside the text: `<name>.json` beside `<name>.txt` (in any case), or `<name>.docling.json` beside the Docling text `<name>.docling.md`, whose `extractor` and `pdf_sha256` are kept. When that manifest names the same path, `attach` rewrites it with the recorded text, so its sha256, origin, `retrieved` and `extraction` match the registry. It does so under the registry's lock; when the manifest cannot be read or written, `attach` fails and leaves the registry unchanged. Another JSON file there is left alone.

Unless `--no-browser` is given, a candidate retries in headless Chromium only when its plain-HTTP reply was a JavaScript check page: a 202 HTML reply (how AWS WAF serves its challenge, as on figshare) or a small 200 HTML page with a challenge marker. A 403, a 429, any other refusal, and transport errors are reported and never retried in the browser. When a host blocks automated browsers, `papers` reports the block. It does no CAPTCHA solving or fingerprint spoofing.

### Docling: a second text

`papers read --key <key> --extraction docling [--record]` (or `papers read <ref> --extraction docling`) converts the PDF with [Docling](https://github.com/docling-project/docling) into Markdown. Headings are `#` lines, tables are Markdown tables, and pages are separated by `<!-- page -->` lines. OCR is off and table structure is on, as measured in the extractor comparison of November 2026.

- It writes `<name>.docling.md` and its manifest `<name>.docling.json` beside `<name>.txt`, and the text's structure sidecar. `<name>.txt`, `<name>.json` and the entry's plain or layout text are not changed.
- It converts the saved `<name>.pdf` when its sha256 is the `pdf_sha256` recorded for one of the paper's texts, and makes no network request. The manifest keeps that text's `origin` and `retrieved`. A saved PDF with no recorded sha256 is not converted; `registry record-pdfs` records it when it reproduces the text, or `--force` downloads the PDF again. Only when no PDF is saved does `read` download it.
- The manifest is `{path, sha256, origin, retrieved, extraction: "docling", pdf_sha256, extractor: "docling <version>"}`. NUL bytes in Docling's output are dropped.
- `--record` adds it to the entry's `texts` as a second text. An entry keeps one Docling text: running it again replaces that text in place, and the plain text keeps its place too.
- Running it again does nothing when the Docling text still has its recorded sha256 and the saved PDF, if any, its `pdf_sha256`. `--force` converts again.
- A run without `--extraction` never reads, replaces or records the Docling text.

Docling runs as an external program, like `pdftotext`. `[papers] docling` is the command that runs Python with Docling importable; `papers` appends `-c <script> <pdf> <out>`. The default runs Docling 2.150.0 through `uv`, which installs it on first use; Docling then downloads its layout and table models (about 0.5 GB). Without a working command, `read` stops with an error that names Docling before it downloads anything. A conversion that takes longer than 30 minutes is stopped. Docling is MIT-licensed; its models are Apache-2.0 and CDLA-Permissive-2.0.

Write order: Docling writes to `<name>.docling.md.tmp`. `read` then records the text in the registry (with `--record`), writes the manifest, and only then renames the text into place. When a step fails, the steps before it are undone: the temporary file is removed, the old manifest is put back, and the entry gets back the Docling text it had. Structure sidecars are written the same way, through a temporary file and a rename.

### `registry record-pdfs`

`papers registry record-pdfs [--dir <dir>] [--dry-run] [--registry <path>]` records `pdf_sha256` for texts that `read` saved before it recorded that field. After it, `papers read --extraction <mode>` extracts such a text again from the saved PDF and makes no download. The command makes no network request.

It looks at each `<name>.json` manifest in `--dir`, which defaults to the directory that holds most of the registry's texts, else `<root>/sources/text`, that names `<name>.txt`, has no `pdf_sha256` and has `<name>.pdf` beside it. It extracts that PDF in the text's recorded mode, or in plain mode when none is recorded, with the extractors `read` uses:

- Plain: `pdf-extract` first, then `pdftotext` if it is on `PATH`. `read` kept one or the other, depending on the title it knew then, so the text may have come from either.
- Layout: `pdftotext -layout` only.

It records the PDF's sha256 only when an extractor gives a text with the recorded text sha256:

- It writes `pdf_sha256` to the manifest and to each registry text with the same path and sha256, under the registry's lock. It prints a `recorded:` line that names the extractor.
- Any other text is left unchanged. It prints a `not reproduced:` line with the reason, such as `pdftotext is not on PATH`. `read` downloads again for a mode change of such a text.
- A text is not reproduced when another version of an extractor wrote it and that version's output differs. Installing Poppler can reproduce more plain and layout texts; run the command again then.

A text is named by the keys of the registry entries that list its path, else by `<name>`. The last lines give the counts and the names of the texts that were not reproduced. `--dry-run` prints the same and writes nothing. The exit code is 0 also when texts were not reproduced.

### `registry abstracts`

`papers registry abstracts [--key <key>]… [--limit <n>] [--dry-run] [--registry <path>]` fills the `abstract` field of entries that have none. A search index can then find a paper by its abstract when no text is saved.

- An entry with an arXiv id gets the abstract from arXiv, in the entry's recorded `version` when it has one. Any other entry with a DOI or an OpenAlex id gets it from OpenAlex. An entry with none of these is counted and skipped.
- The command sends only those public ids, in batches for arXiv and one work per request for OpenAlex. It uses the response cache and downloads no PDF.
- It writes `abstract` and `abstract_source` and changes nothing else, under the registry's lock. An entry that has an abstract is never changed.
- After 3 OpenAlex failures in a row, it does not attempt the rest and reports each as failed. Run it again later; entries that got an abstract are not asked again.
- The last lines give the counts and the keys whose source has no abstract. `--dry-run` prints the same and writes nothing. The exit code is 1 when a lookup failed, else 0.

[docs/interfaces.md](docs/interfaces.md) defines the two fields.

## Structure sidecars: `rf.paper-structure/1`

`papers structure <key|path>…` writes `<text file name>.structure.json` beside each text: `sources/text/87.txt.structure.json` for `sources/text/87.txt`, and `sources/text/87.docling.md.structure.json` for its Docling text. `papers structure --all` does so for every text the registry records. `papers read` writes it for the text it saves or finds unchanged in `--out`, and prints `structure: <path>`.

The sidecar holds, as raw line ranges of the text:

- the pages, from the form feeds the extractor wrote, or a Markdown text's `<!-- page -->` lines;
- the section spans: `front`, `abstract`, `body`, `references`, `appendix` or `unknown`, each with its heading and level;
- the reference list, divided into references where they are numbered or separated by blank lines;
- for each reference, the registry key it cites, matched by DOI, arXiv id or normalised title.

The command reads only the text and the registry. It makes no network request, never changes a text, and works for a text of any `extraction` value and for Markdown texts. What detection does not find is `unknown` or a warning, never a guess. It prints one line per text, then the text's warnings, then the counts of texts with a reference list and with an abstract span. `--dry-run` prints the same and writes nothing. A sidecar is rewritten only when its content changes. It records its text's sha256; `registry check` reports `derivative-stale` when the text no longer has it.

[docs/interfaces.md](docs/interfaces.md) defines the schema, the line numbering and what a search index does with the sidecar.

## Paper cards: `rf.paper-card/1`

A paper card lists what a paper claims, each item with a verbatim quote from the paper's local text and the quote's raw line range. A search index can rank a paper by its card when the full text is too long or too noisy to index. A card is class `generated`: it is never a source, and a citation goes to the paper.

Cards are off by default. `[papers] cards = true` turns them on (decision record DR043 of the masters project).

`papers card --key <key> [--docling] [--force] [--reply <file>]` makes the card of one entry:

1. It reads the entry's first plain or layout text, or its Docling text with `--docling`, and checks the text's sha256 against the registry.
2. It runs Claude Code once, in print mode: `<[papers] claude> -p --model <card_model> --system-prompt-file <prompt> --tools "" --setting-sources "" --strict-mcp-config --disable-slash-commands --output-format json --json-schema <schema>`. The paper text goes on standard input and is the only input besides the card prompt. The call runs in an empty directory, `<state_dir>/papers/card-cwd`, and is stopped after 20 minutes.
3. It saves the raw reply as `<state_dir>/papers/card-replies/<key>-<time>.json`.
4. It checks every item and drops those that fail. The kept items, the dropped counts and the tokens are printed.
5. It stores the card as `<card_dir>/<key>.json` through a temporary file and a rename.

An item is kept only when all of these hold:

- its `field` is one of the prompt's fields;
- its quote has at least 8 words, where each CJK character counts as a word, as word processors count them;
- its quote occurs in the text as one contiguous passage, after claimcheck's normalisation (case, ligatures, quote and dash variants, hyphenation at line ends, white space and punctuation). The match gives the item's `lines`;
- every number in its `value` also occurs in its quote, as claimcheck matches numbers (`92.4` matches `92.40`, `1,200` matches `1200`). A number that is part of a name is not checked: one in a word that starts with a letter (`F1`, `GPT-4o`, `Llama-2`, `recall@5`, `top-1`), one followed by `-` and a capital (`10-K`), and a dotted identifier. A word here is a run of letters, digits, `.`, `-` and `@`. Figures joined to a word, such as `7-point`, `5-fold` and `200ms`, are checked. The prompt asks the model to take figures only from the quote.

The gate: when fewer than 80% of the written items are kept, or none were written, the card is not stored. The command prints `not stored`, names the main cause, and exits 1. An earlier card of the entry is left as it was.

- When quotes not found in the text are fewer than the other drops (numbers not in the quote, short quotes, unknown fields), the reply is at fault, not the text. The message gives the counts. Try `papers card --key <key> --force`.
- Otherwise the text is named as a suspected text-extraction fault (such as mixed PDF columns). Try the Docling text: `papers read --key <key> --extraction docling --record`, then `papers card --key <key> --docling`.

Running it again does nothing when the stored card has the text's sha256, the prompt's sha256 and the model. `--force` calls Claude again. `--reply <file>` checks a saved reply again without a call, for example after the checks change. It prints the kept and dropped items and stores nothing, because a saved reply does not record the prompt it answered.

`[papers] claude` names the command, so a wrapper that chooses the billing can stand in for `claude`. Only the paper text and the prompt go to the model. Send published papers only.

### The card

```json
{
  "schema": "rf.paper-card/1",
  "key": "87",
  "class": "generated",
  "text": {"path": "sources/text/87.txt", "sha256": "…"},
  "model": "claude-haiku-5-5",
  "created": "2026-11-12T03:00:00Z",
  "prompt_sha256": "…",
  "checker": "claimcheck 0.10.0",
  "usage": {"input_tokens": 24564, "output_tokens": 2107, "cost_usd": 0.1052},
  "items": [
    {"field": "result", "value": "…", "quote": "…", "lines": [120, 122]}
  ],
  "written": 14,
  "dropped": 1,
  "dropped_by": {"field": 0, "short": 0, "not_found": 1, "number": 0}
}
```

- `text` is the text the card was made from and its sha256 at that time. `registry check` reports `derivative-stale` when the text no longer has it.
- `lines` are 1-based and inclusive, in the raw text, as in `rf.paper-structure/1`.
- `field` is one of `task`, `dataset`, `method`, `metric`, `result` and `limit`.
- `prompt_sha256`, `checker`, `usage`, `written`, `dropped` and `dropped_by` are added to the shared fields (`key`, `text`, `model`, `created`, `items`).

### Turning cards off

1. Set `cards = false` under `[papers]`, or remove the line.
2. Delete the generated cards in `card_dir` and the saved replies in `<state_dir>/papers/card-replies/`.
3. Supersede DR043 with a decision record that says why.

## Search log: `rf.search/1`

Every `search`, `fetch`, `refs` and `cited-by` appends one JSON line per query to `[papers] search_log`. `--no-log` turns this off. The log means a literature review can show what was searched, when, and what came back, without reconstructing it later. `search` reads it: a query that was already run on the same source is reported with the time and result count of its last run. A `fetch` of a registered paper makes no request and adds no line.

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

The tests use synthetic records and public metadata fixtures only, and they make no network requests. A shell script stands in for Docling.

Every pull request that changes the tool's behaviour raises the version in `Cargo.toml` and `Cargo.lock`.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option.
