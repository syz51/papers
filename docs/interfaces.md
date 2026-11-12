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
- When no recorded text can be reused, `read` prints a line that names `papers read --key <key> --record` and then reads the paper as before.
- The last stdout line of a successful `read` stays the text's manifest as one JSON object.

### `search`, `refs` and `cited-by`

- A result that the archive (registry or notes) holds starts with the line `seen: key <key>`. With several keys the line is `seen: key <key>, key <key>`, in key order.
- A result matches by DOI, arXiv id, OpenAlex id, URL or normalised title. A normalised title is lower case, without accents, punctuation or spaces.
- With `--json`, each result has a `seen` array of keys, empty when the archive does not hold it.
- When the search log holds a `search` with the same source and the same normalised query, `search` prints this line on stderr before it searches: `papers: searched before: last run <RFC 3339 time>, <n> results (<m> runs of this <source> query in <log path>)`. `<n>` is the number of results the last run logged. A normalised query is lower case, without accents, with single spaces. The search still runs.
