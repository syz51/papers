# Interfaces that other tools read

This file defines what `papers` writes for other tools and for agents. A change to anything here is a change of contract: raise the version and tell the readers.

## Registered papers first

`papers fetch`, `papers read` and `papers search` say that the registry already holds a paper before they do anything else for it. The registry is the `rf.reference/1` file that `[papers] registry` names. A paper is registered when an entry has its arXiv id (any version), its DOI, or its URL.

### The registered block

For each registry entry that matches, `fetch` and `read` print this block on stdout before any other line for that paper:

```text
registered: key <key> · depth <depth> · <title>
text: <path>
```

- Line 1 is always present. `<depth>` is the entry's `depth`, or `unknown` when it has none. `<title>` is the entry's title, or `(no title)`.
- When the entry is a duplicate, line 1 is `registered: key <key> · depth <depth> · duplicate of <key> · <title>`.
- One `text:` line follows for each text in the entry's `texts`, in recorded order. `<path>` is relative to the project root.
- ` (missing)` follows the path when the file does not exist. ` (changed)` follows it when the file's sha256 is not the recorded one. A path with no suffix is a file with the recorded sha256.
- The separator is ` · ` (space, U+00B7, space). The title is last, so it may contain the separator.

### `fetch`

- A registered input prints the block and a blank line. `papers` makes no request for it, writes no search-log entry, and prints `papers: <input>: registered; not fetched (--refresh to fetch)` on stderr. The exit code is 0.
- With the global `--refresh`, `fetch` prints the block and a blank line, then fetches and prints the metadata as for any other input.
- `fetch --title <title>` does the same when an entry has the same normalised title.
- With `--json`, the block is the line `{"input": <input>, "registered": {"key", "title", "depth", "duplicate_of", "texts": [{"path", "state"}]}}`. `state` is `ok`, `missing` or `changed`.

### `read`

- `read <ref>` for a registered paper prints the block first. `read --key <key>` prints the block of that entry first.
- Without `--key`, when a recorded text has its recorded sha256, `read` prints `already have <path> (sha256 matches); not downloading (--force to fetch again)` and then the text's manifest. It makes no request and writes no file. The exit code is 0.
- `--force` downloads. `--extraction <mode>` downloads or extracts again when the recorded text has another mode; a text with no recorded mode counts as plain.
- When no text recorded in the entry answers the run, `read` prints a line that names `papers read --key <key> --record` and then reads the paper as before. With `--force` it prints no such line.
- A registry that cannot be loaded is reported on stderr and does not stop `read <ref>`.
- The last stdout line of a successful `read` stays the text's manifest as one JSON object.
- A run without `--extraction docling` ignores the entry's Docling text: it never answers such a run, and such a run never replaces it.

## The Docling text: a second entry in `texts`

`papers read --key <key> --extraction docling --record` adds a second text to an entry. The entry's plain or layout text, its `<name>.txt` and its `<name>.json` manifest are not changed.

| Field | Value |
| --- | --- |
| `path` | `<dir>/<name>.docling.md`, beside `<dir>/<name>.txt`. |
| `sha256` | The sha256 of the Markdown file. |
| `origin`, `retrieved` | Those of the PDF it was converted from: the values recorded for the text with the same `pdf_sha256`, or the download's. |
| `extraction` | `docling`. A reader finds the Docling text by this value, not by its place in `texts`. |
| `pdf_sha256` | The sha256 of `<dir>/<name>.pdf` that Docling converted. |
| `extractor` | `docling <version>`, such as `docling 2.150.0`. |

- An entry has at most one text with `extraction` `docling`. Running the command again replaces it in place.
- The Markdown has `#` headings, Markdown tables, no NUL bytes, and a line `<!-- page -->` between pages. Its structure sidecar is `<name>.docling.md.structure.json`.
- `<name>.docling.json` holds the same object as one JSON line.

## Derivatives

A derivative is a file `papers` makes from a text or a PDF: a structure sidecar, a Docling text or a paper card. Each records the sha256 of its input: a sidecar and a card in `text.sha256`, a Docling text in `pdf_sha256`.

- A derivative is written to `<file>.tmp` and renamed into place. A Docling text is renamed only after its registry record and its manifest are written, so a reader never finds the file without its record. `read` writes a plain or layout text the same way.
- `papers registry check` reports the rule `derivative-stale` (warn) for a derivative whose input now has another sha256. The message names the command that makes it again. An input that is not in the checkout is skipped.

### `search`, `refs` and `cited-by`

- A result that the archive (registry or notes) holds starts with the line `seen: key <key>`. With several keys the line is `seen: key <key>, key <key>`, in key order.
- A result matches by DOI, arXiv id, OpenAlex id, URL or normalised title. A normalised title is lower case, without accents, punctuation or spaces.
- With `--json`, each result has a `seen` array of keys, empty when the archive does not hold it.
- When the search log holds the same search, `search` prints this line on stderr before it searches: `papers: searched before: last run <RFC 3339 time>, <n> results (<m> runs of this <source> query in <log path>)`. `<n>` is the number of results the last run logged. The search still runs.
- Two searches are the same when they have the same source and the same normalised query. For arXiv the query is the one sent, which holds the words, `--category` and `--since`. For the other sources it is the words, and `--since` must be equal too. `--max` and `--sort` do not count. A normalised query is lower case, without accents, with single spaces.
- A search log that cannot be read is reported on stderr and does not stop the search.

## Paper structure: `rf.paper-structure/1`

`papers structure <key|path>…` and `papers structure --all` write one sidecar for each text. `papers read` writes it too, for the text it saves or finds unchanged in `--out`, and prints `structure: <path>` before the manifest line. When a registered paper's recorded text answers the run, `read` writes no file and prints no such line. When the sidecar cannot be written, `read` says so on stderr and still succeeds. The sidecar says where the text has its pages, sections and reference list, and which registry entries the reference list cites. A search index reads it; `papers` never changes the text.

### File name

The sidecar lies beside the text. Its name is the text's file name plus `.structure.json`: `sources/text/87.txt` has `sources/text/87.txt.structure.json`. The rule is the same for every text of an entry, whatever its `extraction` value or file extension.

### Lines

- A line number counts from 1. Lines are what the file's bytes give when split at LF (`\n`). A CR before the LF belongs to the line. A last line without LF counts; an LF at the end of the file starts no line.
- A range gives `first_line` and `last_line`, and includes both.
- A form feed (U+000C) is part of the line it is in. It does not start a line.

### The object

The file is one JSON object, pretty-printed, with a final LF.

```json
{
  "schema": "rf.paper-structure/1",
  "text": {
    "path": "sources/text/87.txt",
    "sha256": "<64 hex digits>",
    "lines": 1271,
    "extraction": "layout"
  },
  "generator": "papers 0.4.0",
  "pages": [{"page": 1, "first_line": 1}, {"page": 2, "first_line": 58}],
  "sections": [
    {"kind": "front", "heading": null, "level": 0, "first_line": 1, "last_line": 5},
    {"kind": "abstract", "heading": "Abstract", "level": 1, "first_line": 6, "last_line": 9},
    {"kind": "body", "heading": "1 Introduction", "level": 1, "first_line": 10, "last_line": 20},
    {"kind": "body", "heading": "2.1 Synthetic Quotes", "level": 2, "first_line": 24, "last_line": 26},
    {"kind": "references", "heading": "References", "level": 1, "first_line": 32, "last_line": 41},
    {"kind": "appendix", "heading": "A Proofs", "level": 1, "first_line": 42, "last_line": 47}
  ],
  "references": {
    "first_line": 33,
    "last_line": 41,
    "split": "numbered",
    "entries": [
      {"first_line": 33, "last_line": 34, "key": "12", "by": "doi"},
      {"first_line": 40, "last_line": 41, "key": null, "by": null}
    ]
  },
  "warnings": []
}
```

| Field | Meaning |
| --- | --- |
| `schema` | Always `rf.paper-structure/1`. |
| `text.path` | The text's path relative to the project root, as in the registry's `texts[].path`. |
| `text.sha256` | The sha256 of the text file as `papers` read it. When it is not the text's sha256 now, the sidecar is out of date: run `papers structure` again. |
| `text.lines` | The number of lines. |
| `text.extraction` | The `extraction` value the registry records for this path, as it is recorded. `unknown` when no entry records the path or the record has no mode. |
| `generator` | `papers <version>`. |
| `pages` | The pages in order, each with its number from 1 and its first line. A page without text is not listed, so the numbers can skip. It is the string `unknown` when the text has no page mark. A form feed (U+000C) ends a page; the next page's `first_line` is the line where its text starts. In a Markdown text without form feeds, a line that is only an HTML comment starting with `page`, such as `<!-- page -->` or `<!-- page break -->`, ends a page; the next page starts on the line after it. |
| `sections` | The spans in line order. Together they cover every line exactly once. An empty text has none. |
| `sections[].kind` | `front`, `abstract`, `body`, `references`, `appendix` or `unknown`. |
| `sections[].heading` | The heading, trimmed, with single spaces. `null` for a span without a heading. When the extractor put a heading's number and its words on two lines, it is both joined by a space, and `first_line` is the number's line. |
| `sections[].level` | 1 for a top heading, 2 for `2.1`, 3 for `2.1.1`. 0 for a span without a heading. In a Markdown text, a heading with a section number has the number's depth, and any other heading has its count of `#`. |
| `references` | The reference list, or the string `unknown` when no reference heading is detected. |
| `references.first_line`, `last_line` | The list without its heading line and without blank lines at its end. It lies inside the `references` section. |
| `references.split` | How the list divides into references: `numbered` (`[1]` or `1.` marks), `blank` (blank lines between references) or `unknown`. |
| `references.entries` | With `numbered` or `blank`: every reference in order, each with its line range. With `unknown`: only the matches, each with the lines of the id or title that matched. |
| `entries[].key` | The registry key the reference cites. `null` when no entry matches. |
| `entries[].by` | What matched: `doi`, `arxiv` or `title`. `null` when `key` is `null`. |
| `warnings` | Sentences that say what detection did not find or could not decide. |

### Section kinds

- `front`: the lines before the first heading, such as the title and the authors. It has no heading.
- `abstract`: from an `Abstract` heading to the next heading. When no heading follows within 80 lines, the abstract ends before its first blank line, or after 80 lines when it has none; the lines after it up to the next heading are `unknown`, with a warning.
- `body`: a numbered heading (`1 Introduction`, `2.1 Method`, `IV. RESULTS`) or a common unnumbered one (`Introduction`, `Conclusion`, `Acknowledgments`). In a Markdown text (`.md`, `.markdown`), detection reads only the `#` headings outside code fences: `Abstract` and `References` headings give those kinds, headings after the references are `appendix`, and all others are `body`.
- `references`: from a `References`, `Bibliography` or similar heading to the next heading. The span may hold lines that follow the list, such as tables, when no heading separates them.
- `appendix`: an `Appendix` or `Supplementary Material` heading, and lettered headings (`A Proofs`, `A.1 Details`) after the reference list.
- `unknown`: lines whose kind detection cannot decide. A text without any detected heading is one `unknown` span, with a warning.

Detection reads patterns, not meaning. It reports `unknown` or a warning where a pattern is missing. It does not guess: a text without an `Abstract` heading has no `abstract` span.

### Citation edges

Each entry of `references.entries` with a `key` is a citation edge from the text's registry entry to the entry `key`.

- A reference matches an entry by DOI, then by arXiv id, then by title. DOIs compare without case. An arXiv id matches in any version, in the forms `arXiv:2601.00001`, `arxiv.org/abs/2601.00001`, `arxiv.org/pdf/2601.00001`, `abs/2601.00001` and the DOI `10.48550/arXiv.2601.00001`.
- A title matches when the reference's normalised text contains the entry's normalised title. A normalised title is lower case, without accents, punctuation or spaces, and a line-end hyphen is joined. Titles shorter than 20 normalised characters do not match. The longest title wins. Of two entries with the same DOI, arXiv id or title, the first in the registry is the match.
- A match on an entry that has `duplicate_of` gives that key instead.
- The entries that record the text itself are never a match.
- A DOI or arXiv id that a line break divides does not match.

### What a search index does with it

- Index the raw text, and keep `first_line` and `last_line` as the anchors of each passage. Do not write a cleaned copy of the text.
- For matching only, join a word that a line-end hyphen divides, and join wrapped lines into one string. Map each match back to raw lines.
- Cut passages at section borders. A passage never crosses from one section into the next.
- Put the paper's title and the section's `heading` before each passage's words in the indexed string.
- Index the `references` list range in a separate field that a default search does not read. Index `front` and `unknown` spans as body text.
- Use `entries[].key` as citation edges between registry entries.
- Treat `unknown`, an absent `abstract` span and each warning as "not known". Fall back to the whole text for such a text.

## The `abstract` field of `rf.reference/1`

`papers registry abstracts [--key <key>]… [--limit <n>] [--dry-run] [--registry <path>]` adds two optional fields to an entry that has no `abstract`:

| Field | Meaning |
| --- | --- |
| `abstract` | The work's abstract as the source gives it, as one string with single spaces and no line breaks. The field is absent when no abstract is recorded. It is never `null` or empty. |
| `abstract_source` | Where the abstract is from: `arxiv:<id>v<n>` (the version asked for) or `openalex:<W id>`. It is present exactly when `abstract` is. |

- An entry with an arXiv id gets the abstract from arXiv, in the entry's recorded `version` when it has one, else the latest. Any other entry with a DOI or an OpenAlex id gets it from OpenAlex. An entry with none of these is skipped.
- The command sends only the arXiv id, the DOI or the OpenAlex id. It downloads no PDF. It changes no other field and no entry order, under the registry's lock.
- An entry that has an `abstract` is never changed. To replace an abstract, remove both fields by hand and run the command again.
- `registry build` keeps both fields.
- The abstract may contain LaTeX or markup as the source gives it. OpenAlex abstracts are rebuilt from its word index, so their punctuation spacing can differ from the paper's.

## Paper cards: `rf.paper-card/1`

`papers card --key <key>` writes `<card_dir>/<key>.json` (default `sources/cards/<key>.json`), one pretty-printed JSON object. It is written only when `[papers] cards = true` and the card keeps at least 80% of the items the model wrote. A card is class `generated`: never a source of a claim.

| Field | Value |
| --- | --- |
| `schema` | `rf.paper-card/1`. |
| `key` | The registry key. |
| `class` | `generated`. |
| `text` | `{path, sha256}`: the registry text the card was made from, as recorded in `texts`, and its sha256 then. |
| `model` | The model id, such as `claude-haiku-5-5`. |
| `created` | UTC RFC 3339 time of the call. |
| `items` | `[{field, value, quote, lines}]`. `field` is `task`, `dataset`, `method`, `metric`, `result` or `limit`. `value` is the model's statement. `quote` is verbatim text of the paper, at least 8 words (a CJK character counts as a word). `lines` is `[first, last]`, 1-based and inclusive, in the raw text. |
| `prompt_sha256` | The sha256 of the card prompt. |
| `checker` | The quote checker, such as `claimcheck 0.10.0`. |
| `usage` | `{input_tokens, output_tokens, cost_usd}`. Input tokens include cached ones. `cost_usd` is absent when the reply gives none. |
| `written`, `dropped`, `dropped_by` | The items the model wrote, those dropped, and the drops by reason: `field`, `short`, `not_found`, `number`. |

Every kept item meets these conditions, and a reader may rely on them:

- `quote` occurs in the text at `text.path`, as one contiguous passage after claimcheck's normalisation, within `lines`.
- Every number in `value` occurs in `quote`, as claimcheck matches numbers.

A card is stale when the file at `text.path` no longer has `text.sha256`. `papers registry check` reports it as `derivative-stale`, and `papers card --key <key> --force` makes it again. A card whose `prompt_sha256` or `model` differs from the present ones is not stale; `papers card` without `--force` makes it again.

### What a search index does with it

- Index a card as part of its registry entry: the `value` and `quote` of every item. Rank the entry, not the card.
- Skip a stale card.
- Show a hit's `quote` with `text.path` and `lines`, so the reader goes to the paper. Never cite the card.
- Find an entry's Docling text by `texts[].extraction == "docling"`, and its plain or layout text as the other text.
