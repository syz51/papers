# Paper text extractors on ten registered papers (November 2026)

Experiment E2 of the research-factory archive design (`docs/archive-design.md`, section 3.2).
Board task: research-factory-202. Run on 2026-11-12 on an Apple M3 with 16 GB.

## Result

- Docling on the saved PDF found every label: 216 of 216 headings, 10 of 10 reference starts, and 100 of 100 table numbers with their row label on the same line and their column label in the same column. It needs about 5 s per paper and 1.8 GB.
- arXiv HTML scored the same on the six papers that have it. Four of the ten papers have none.
- The current plain text is better than the task assumed. It holds all 100 numbers, each on one line with its row label. It has no heading marks and no column labels, 1,746 line-end hyphens, and NUL bytes in some files.
- `pdftotext -layout` is the worst text for structure: it puts two columns on one line.
- Marker is slower, needs more memory and an extra program, and loses row and column labels.

Recommendation: `papers read` keeps the plain text as it is and adds a Docling text as a second `texts` entry. See [Recommendation](#recommendation).

## Papers

Ten entries of the masters registry (`docs/research/references.jsonl`, `rf.reference/1`), each with a saved PDF in `docs/research/sources/text/`. `papers.json` lists them.

| Key | Paper | Pages | arXiv version | Structured source |
|---|---|---|---|---|
| 4 | JFinTEB | 5 | 2604.15882v1 | arXiv HTML |
| 62 | Ebisu (holds Japanese text) | 18 | 2602.01479v1 | arXiv HTML |
| 85 | PDF Parsing and Chunking for Financial QA | 12 | 2604.12047v1 | none: submitted as a PDF, no HTML, no LaTeX |
| 232 | FinanceBench | 16 | 2311.11944v1 | arXiv HTML |
| 237 | FinDER | 14 | 2504.15800v3 | arXiv HTML |
| 848 | Optimizing Retrieval Strategies for Financial QA | 15 | 2503.15191v1 | arXiv HTML |
| 849 | DROP | 12 | 1903.00161v2 | arXiv HTML |
| 49 | FinQA | 14 | none in the registry | none (ACL Anthology PDF) |
| 50 | TAT-QA | 11 | none in the registry | none (ACL Anthology PDF) |
| 484 | Dense Passage Retrieval | 13 | none in the registry | none (ACL Anthology PDF) |

Seven have an arXiv id. All ten report their main results in tables. Key 62 holds Japanese text. The arXiv version is the one printed on the saved PDF.

## Extractors

| Name | What it is | Version | Licence |
|---|---|---|---|
| `plain` | The registry's current text, as stored. `papers read` made most of them with the `pdf-extract` crate. It was not extracted again. | stored files | — |
| `layout` | `pdftotext -layout`, the existing `layout` mode of `papers read` | Poppler 26.11.0 | GPL (external program) |
| `arxiv-html` | arXiv's HTML of the same version (LaTeXML), turned into text by `latexml_to_text` in `extract.py`: `#` headings, one line per paragraph, one `\| cell \|` line per table row | — | paper content only |
| `docling` | Docling on the PDF: layout model and TableFormer, OCR off, Markdown export | docling 2.150.0, docling-core 2.106.0, torch 2.15.0 | Code MIT. Models `docling-project/docling-layout-heron` Apache-2.0 and `docling-project/docling-models` CDLA-Permissive-2.0 and Apache-2.0. |
| `marker` | Marker on the PDF: Surya layout, table and recognition models, Markdown output | marker-pdf 2.1.0, surya-ocr 0.23.0, llama.cpp 0.9.0 | Code Apache-2.0. Weights: modified AI Pubs Open RAIL-M, free for research, personal use and startups under 5 M USD; the model cards say `openrail`. |

Licences come from each project's `LICENSE` and README on GitHub and from the model cards on Hugging Face, read on 2026-11-12. All models ran locally, one at a time. No text was sent to a service. The only network requests were the downloads of packages, model weights and the six arXiv HTML pages.

## Hand labels

`labels/<key>.json` holds for each paper:

- every heading of level 1 and 2, in the body and the appendix, with its printed number and page (216 headings);
- the heading of the reference list, its page and the first six words of the first entry;
- ten numbers from the result tables, each with its table, page, row label and column label (100 numbers).

Who and how: two Claude subagents (model Claude Fable 5.2) labelled five papers each on 2026-11-12. They read every page as a rendered image and read each table value again at 150 to 300 dpi. They ran no text extractor and did not open the stored texts. No human checked the labels. All five extractors later found all 100 values, and Docling found every row label, column label and heading, so no label is contradicted by the PDFs' own text.

Label choices that affect the scores:

- Page numbers are PDF page indexes, not printed page numbers.
- Small-caps headings are written with capitals where the page shows full-size capitals. Matching ignores case.
- Paper 232 prints `104 (70%)` in a cell; the label is `104`. Papers 4, 62 and 232 have one result table each.
- Some column headers repeat under a group header, such as `EM` under `Dev` and `Test`. The label holds the innermost header only.

## Measures

`measure.py` states the exact rules. In short:

- **Headings found:** one output line equals the heading, with or without its number. **Marked:** that line starts with `#`.
- **Reference heading:** one line equals the reference heading. **Reference start:** in addition, the first four words of the first entry, as the PDF prints them, follow within eight lines.
- **Numbers found:** the digits occur as a whole number.
- **With row label:** one line holds the number and its row label.
- **With column label:** that line is a `|` table row, and a row above it holds the column header in the same column.
- **Seconds per paper:** wall time of the extraction call. **Failures:** papers with no output.

Every count has the labels of all ten papers as its denominator. A paper without output scores zero.

## Results

| Extractor | Papers extracted | Headings found | Headings marked | Reference heading | Reference start | Numbers found | With row label | With column label | Seconds per paper (median, max) | Peak memory | Failures (keys) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| plain | 10/10 | 199/216 | 0/216 | 10/10 | 10/10 | 100/100 | 100/100 | 0/100 | stored, not run | — | none |
| layout | 10/10 | 40/216 | 0/216 | 3/10 | 3/10 | 100/100 | 95/100 | 0/100 | 0.05, 0.07 | 28 MB | none |
| arxiv-html | 6/10 | 125/216 | 125/216 | 6/10 | 4/10 | 60/100 | 60/100 | 60/100 | 0.02, 0.33 | 56 MB | 85, 49, 50, 484 |
| docling | 10/10 | 216/216 | 216/216 | 10/10 | 10/10 | 100/100 | 100/100 | 100/100 | 5.2, 46.2 | 1,776 MB | none |
| marker | 10/10 | 204/216 | 204/216 | 6/10 | 5/10 | 100/100 | 85/100 | 60/100 | 11.2, 50.7 | 3,858 MB | none in the second run; 8 in the first |

On the six papers that have arXiv HTML, the label totals are 126 headings and 60 numbers. arXiv HTML found 125 headings and all 60 numbers with both labels.

Per paper. Cell: headings found (marked) · reference heading/start · numbers found · with row label · with column label · seconds.

| Key | Labels (headings, numbers) | plain | layout | arxiv-html | docling | marker |
|---|---|---|---|---|---|---|
| 4 | 14, 10 | 4(0) · 1/1 · 10 · 10 · 0 | 4(0) · 1/1 · 10 · 10 · 0 · 0.1 s | 13(13) · 1/0 · 10 · 10 · 10 · 0.3 s | 14(14) · 1/1 · 10 · 10 · 10 · 46.2 s | 14(14) · 1/1 · 10 · 10 · 9 · 50.7 s |
| 62 | 31, 10 | 29(0) · 1/1 · 10 · 10 · 0 | 12(0) · 1/1 · 10 · 7 · 0 · 0.1 s | 31(31) · 1/0 · 10 · 10 · 10 · 0.0 s | 31(31) · 1/1 · 10 · 10 · 10 · 5.0 s | 30(30) · 1/0 · 10 · 10 · 10 · 40.2 s |
| 85 | 17, 10 | 16(0) · 1/1 · 10 · 10 · 0 | 3(0) · 0/0 · 10 · 9 · 0 · 0.1 s | failed | 17(17) · 1/1 · 10 · 10 · 10 · 7.9 s | 15(15) · 0/0 · 10 · 8 · 7 · 3.5 s |
| 232 | 19, 10 | 17(0) · 1/1 · 10 · 10 · 0 | 4(0) · 0/0 · 10 · 10 · 0 · 0.0 s | 19(19) · 1/1 · 10 · 10 · 10 · 0.0 s | 19(19) · 1/1 · 10 · 10 · 10 · 5.4 s | 15(15) · 0/0 · 10 · 10 · 0 · 2.9 s |
| 237 | 18, 10 | 18(0) · 1/1 · 10 · 10 · 0 | 1(0) · 0/0 · 10 · 10 · 0 · 0.0 s | 18(18) · 1/1 · 10 · 10 · 10 · 0.0 s | 18(18) · 1/1 · 10 · 10 · 10 · 5.0 s | 18(18) · 1/1 · 10 · 10 · 9 · 13.8 s |
| 848 | 28, 10 | 28(0) · 1/1 · 10 · 10 · 0 | 0(0) · 0/0 · 10 · 10 · 0 · 0.0 s | 28(28) · 1/1 · 10 · 10 · 10 · 0.0 s | 28(28) · 1/1 · 10 · 10 · 10 · 4.4 s | 28(28) · 1/1 · 10 · 8 · 8 · 6.6 s |
| 849 | 16, 10 | 16(0) · 1/1 · 10 · 10 · 0 | 2(0) · 0/0 · 10 · 10 · 0 · 0.1 s | 16(16) · 1/1 · 10 · 10 · 10 · 0.0 s | 16(16) · 1/1 · 10 · 10 · 10 · 4.5 s | 15(15) · 0/0 · 10 · 9 · 6 · 29.8 s |
| 49 | 24, 10 | 24(0) · 1/1 · 10 · 10 · 0 | 6(0) · 0/0 · 10 · 10 · 0 · 0.0 s | failed | 24(24) · 1/1 · 10 · 10 · 10 · 4.9 s | 23(23) · 1/1 · 10 · 2 · 0 · 20.9 s |
| 50 | 24, 10 | 24(0) · 1/1 · 10 · 10 · 0 | 4(0) · 0/0 · 10 · 9 · 0 · 0.1 s | failed | 24(24) · 1/1 · 10 · 10 · 10 · 8.0 s | 21(21) · 0/0 · 10 · 10 · 3 · 6.3 s |
| 484 | 25, 10 | 23(0) · 1/1 · 10 · 10 · 0 | 4(0) · 1/1 · 10 · 10 · 0 · 0.1 s | failed | 25(25) · 1/1 · 10 · 10 · 10 · 6.0 s | 25(25) · 1/1 · 10 · 8 · 8 · 8.6 s |

Time and memory notes:

- Docling: ten papers took 99 s in one process. The first paper's 46.2 s includes the download and load of the weights (0.5 GB); the others took 4.4 to 8.0 s. Peak memory is the process's maximum resident set size; `/usr/bin/time -l` reported 1,862 MB.
- Marker: ten papers took 194 s. The first paper's 50.7 s includes the model load. The Python process peaked at 1,415 MB. With the `llama-server` process that Marker starts, the sampled sum peaked at 3,858 MB.
- arXiv HTML: the time is the conversion of a cached page. One download took 0.07 s; arXiv asks for three seconds between requests.
- Plain: the stored text was not extracted again, so it has no time.

## Failures

- **arxiv-html, keys 85, 49, 50, 484:** no source. Key 85 was submitted to arXiv as a PDF; arXiv has no HTML and its source download is the PDF. Keys 49, 50 and 484 have no arXiv id in the registry. arXiv versions of these three papers exist, but they are other versions than the registered ACL Anthology PDFs.
- **marker, first run, 8 of 10 papers:** `SpawnError: llama-server binary not found`. Marker 2.1 runs its recognition model through llama.cpp, which `uv` does not install. Only keys 85 and 232 finished. After `brew install llama.cpp` all ten finished; the table shows that second run.
- **marker, quality:** it put no heading mark on the reference list of keys 85, 232, 849 and 50. It split or merged cells in tables of keys 49, 85, 848, 849 and 484, so 15 numbers lost their row label; in key 49 it kept 2 of 10.
- **layout:** no crash. Two-column pages come out with both columns on one line, so 176 of 216 headings share a line with body text.
- **plain, key 4:** the stored text has both columns on the same lines; only 4 of 14 headings stand alone. The registry records no extraction mode for it.
- **docling:** no failure on the ten papers.

## Other findings

- **The plain text keeps table rows.** In the ten stored texts, every labelled number is on one line with its row label, such as `BiDAF 26.06 28.85 24.75 27.49`. The cells have no column marks, and the header row is not tied to them.
- **NUL bytes hide matches.** The stored text of key 849 holds 6 NUL bytes, so `grep` treats the file as binary and prints no matching line for `96.4`; `grep -a` prints three. Of the 85 registered texts, 8 hold NUL bytes: keys 25 (3,343), 121 (170), 849 (6), 86, 777 (3 each), 89, 139 (2 each) and 87 (1). This explains at least one of the failed searches named in the task. The structured outputs hold none.
- **Columns side by side.** Of the 85 registered texts, 4 have a wide gap in more than 30% of their lines: keys 4, 114, 157 and 772. Docling on key 772 (8 pages, not labelled) gave 7 page marks and the headings in reading order.
- **Line-end hyphens.** The ten texts have 1,746 lines that end in a hyphen in `plain` and 1,145 in `layout`. `arxiv-html` and `docling` have none: Docling joins the words and writes one line per paragraph.
- **Japanese text.** All four of plain, layout, arXiv HTML and Docling give the same 670 kana and kanji characters for key 62.
- **Docling drops display formulas.** The ten outputs hold 47 `<!-- formula-not-decoded -->` marks and 31 `<!-- image -->` marks. The plain text keeps the formula characters, in a broken order.
- **Docling's headings have one level.** Every heading is `##`. The depth is in the printed number (`5.1`). arXiv HTML keeps the depth (`##`, `###`).
- **Docling writes HTML entities.** `&` in a heading is `&amp;`. A reader must decode them; `measure.py` does.
- **Docling can mark pages.** `export_to_markdown(page_break_placeholder="<!-- page -->")` writes one mark between pages.
- **arXiv HTML has another reference list.** For keys 4 and 62 the entries are set in another style than in the PDF (`C. Chen, H. Huang` for `Chung-Chi Chen, Hen-Hsen Huang`), with added `Cited by` text. It also names a heading differently (`Acknowledgements` for `Acknowledgments` in key 4). A quote from the PDF can fail on this text.

## Recommendation

### Extraction order for `papers read`

1. **Plain text, always, unchanged.** It stays the first `texts` entry. Quote checks pin its hash, and it already answers a number search when the file is read as text (`grep -a`).
2. **Docling on the saved PDF, as the second entry, for every paper.** It was complete and correct on all ten papers, it needs no source besides the PDF that `read` already saves, and its text follows the registered PDF version, with page marks.
3. **arXiv HTML only as a fallback** when Docling is not installed or fails, and only for an entry with an arXiv id whose HTML exists for the PDF's version. Do not build it now: on these papers it adds nothing to Docling except heading depth and formulas, and it covers 6 of 10.
4. **Not recommended:** Marker (lower scores, 3.9 GB, an extra binary, weights not free for all use) and `pdftotext -layout` as a structured text (it stays an optional mode for a table that Docling gets wrong).

This differs from the design's order, which puts a structured source before a layout model. The reason is coverage and one text form for all papers, at equal scores.

### Storage form

The registry already keeps several `texts` per entry and keeps an unknown `extraction` value as it is. The Docling text is one more entry and one more file; nothing of the plain text changes.

```json
"texts": [
  {"path": "docs/research/sources/text/85.txt", "sha256": "<unchanged>", "origin": "https://arxiv.org/pdf/2604.12047",
   "retrieved": "2026-11-11T18:03:34Z", "extraction": "plain", "pdf_sha256": "<pdf>"},
  {"path": "docs/research/sources/text/85.docling.md", "sha256": "<of the new file>", "origin": "https://arxiv.org/pdf/2604.12047",
   "retrieved": "2026-11-11T18:03:34Z", "extraction": "docling", "pdf_sha256": "<the same pdf>", "extractor": "docling 2.150.0"}
]
```

- File `<name>.docling.md` beside `<name>.txt`; its manifest is `<name>.docling.json`, which is what `Path::with_extension("json")` gives, so it does not collide with `<name>.json`.
- `origin`, `retrieved` and `pdf_sha256` are those of the PDF. `extractor` records the program and version, because another version can give another text.
- The text is Docling's Markdown with `<!-- page -->` between pages, OCR off.
- A fallback text from arXiv HTML would be `<name>.arxiv-html.md` with `"extraction": "arxiv-html"`, `origin` the HTML URL with its version, and no `pdf_sha256`.
- Readers choose by `extraction`: quote checks keep using `plain`; search and structure use `docling` when it exists.

### Do tables keep row and column labels on one line?

| Extractor | Row label on the number's line | Column label |
|---|---|---|
| plain | yes, 100/100, cells divided by spaces only | no: the header is some lines above, not tied to a column |
| layout | 95/100; a line can also hold text of the other page column | no: aligned by spaces only |
| arxiv-html | yes, 60/60 where it exists | not on the line; in the same column of the header rows, 60/60 |
| docling | yes, 100/100 | not on the line; in the same column of the table's first row, 100/100. Group headers are joined into it, such as `Top-20 - NQ`. |
| marker | 85/100 | 60/100 |

No extractor puts the column label on the number's own line. A search index that wants row, column and value in one window must add the header row to each table row; with Docling's tables this is a split on `|`.

## Note for research-factory-198 (structure sidecar)

- Build `rf.paper-structure/1` for the plain text by heading patterns, as planned: 199 of 216 headings stand alone on a line there, and the reference heading does in 10 of 10. Expect `unknown` for texts with side-by-side columns (keys 4, 114, 157, 772).
- Read texts as bytes and accept NUL bytes; 8 of 85 texts hold them.
- When a `docling` text exists, the sidecar needs no detection: a heading is a line that starts with `## `, its depth is the depth of its number, the reference span starts at `## References`, and page starts are the `<!-- page -->` lines.
- Plain texts hold no page marks, so their sidecar has no page starts.

## Run it again

```sh
SRC=<masters>/docs/research/sources/text   # KEY.pdf and KEY.txt
OUT=<a scratch directory outside every repository>
export HF_HOME=$OUT/hf MODEL_CACHE_DIR=$OUT/marker-models
uv run extract.py plain      --sources $SRC --out $OUT
uv run extract.py layout     --sources $SRC --out $OUT
uv run extract.py arxiv-html --sources $SRC --out $OUT
uv run --with docling    extract.py docling --sources $SRC --out $OUT
uv run --with marker-pdf extract.py marker  --sources $SRC --out $OUT   # needs llama-server on PATH
uv run measure.py --out $OUT --missed
```

`plain` and `layout` peak-memory figures and all times come from `run.json`, which `extract.py` writes per extractor.

## Limits

- Ten papers, all born-digital PDFs from LaTeX or a PDF library, all in English with some Japanese. No scanned paper, no paper mainly in Japanese or Chinese.
- The labels were made by a model from page images, not by a person.
- The measures test presence and line position, not reading order or the correctness of running text.
- Docling's formula option (`do_formula_enrichment`) was tried on key 484 and stopped after twelve minutes without a result. The cause (model download or computation) was not examined, so its cost is not measured.
- LaTeX source and publisher XML were not tested: arXiv HTML existed for every paper that has LaTeX source.
