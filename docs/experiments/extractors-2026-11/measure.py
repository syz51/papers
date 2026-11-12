# /// script
# requires-python = ">=3.11"
# ///
"""Score extractor outputs against the hand labels.

    uv run measure.py --out <dir written by extract.py> [--json results.json]

Prints one Markdown table over all papers and one per paper. Every count has
the labels of all ten papers as its denominator: a paper that an extractor
could not extract scores zero and is listed under failures.

Matching rules (all on NFKC-normalised, lower-cased text with runs of white
space collapsed, HTML entities decoded, and Markdown emphasis marks and backslash
escapes removed):

- heading found: one output line, without leading `#` marks and without its
  section number, equals the labelled title. `marked` also needs the `#`.
- reference heading found: a line equals the labelled reference heading.
- reference start found: as above, and the first four words of the first entry,
  as the PDF prints them, occur in the eight non-empty lines after that line.
- number found: the labelled digits occur as a whole number token, not as part
  of a longer number.
- with row label: one line holds both that token and the labelled row label.
- with column label: that line is a `|` table row, and a row above it in the
  same table holds the labelled column header in the same column.
"""

import argparse
import html
import json
import re
import unicodedata
from pathlib import Path

HERE = Path(__file__).parent
EXTRACTORS = ["plain", "layout", "arxiv-html", "docling", "marker"]


def norm(text):
    text = unicodedata.normalize("NFKC", html.unescape(text))  # Docling writes `&amp;`
    text = re.sub(r"<[^>]+>", " ", text)  # <br>, <sub> and the like inside Markdown cells
    text = re.sub(r"\\([_*#|&%$\[\]()])", r"\1", text)
    text = re.sub(r"\*+|(?<!\w)_+|_+(?!\w)|`", "", text)
    return re.sub(r"\s+", " ", text).strip().lower()


def number_pattern(value):
    return re.compile(r"(?<![\d.,])" + re.escape(value) + r"(?![\d]|[.,]\d)")


def heading_text(line, number):
    """The line without `#` marks and without its section number."""
    text = re.sub(r"^#+\s*", "", line)
    if number:
        text = re.sub(r"^(appendix\s+)?" + re.escape(number.lower()) + r"[.:]?\s+", "", text)
    else:
        text = re.sub(r"^(appendix\s+)?([a-z]|\d+(\.\d+)*)[.:]?\s+", "", text)
    return text.rstrip(".:")


def cells(line):
    return [c.strip() for c in line.strip().strip("|").split("|")]


def score(text, labels):
    raw = text.split("\n")
    lines = [norm(line) for line in raw]
    marked = [line.lstrip().startswith("#") for line in raw]
    result = {"headings": 0, "headings_marked": 0, "references_heading": 0, "references": 0, "numbers": 0, "with_row": 0, "with_column": 0}
    missed = {"headings": [], "numbers": [], "with_row": []}

    for heading in labels["headings"]:
        title = norm(heading["title"]).rstrip(".:")
        hits = [i for i, line in enumerate(lines) if heading_text(line, heading["number"]) == title]
        if hits:
            result["headings"] += 1
            result["headings_marked"] += any(marked[i] for i in hits)
        else:
            missed["headings"].append(heading["title"])

    reference = labels["references"]
    entry = " ".join(norm(reference["first_entry"]).split()[:4])
    for i, line in enumerate(lines):
        if heading_text(line, None) == norm(reference["heading"]):
            result["references_heading"] = 1
            after = [later for later in lines[i + 1 :] if later][:8]
            if entry in " ".join(after):
                result["references"] = 1
                break

    for number in labels["numbers"]:
        pattern = number_pattern(number["value"])
        row = norm(number["row"])
        column = norm(number["column"])
        hits = [i for i, line in enumerate(lines) if pattern.search(line)]
        if not hits:
            missed["numbers"].append(f'{number["table"]} {number["row"]} / {number["column"]} = {number["value"]}')
            continue
        result["numbers"] += 1
        on_row = [i for i in hits if row in lines[i]]
        if not on_row:
            missed["with_row"].append(f'{number["table"]} {number["row"]} / {number["column"]} = {number["value"]}')
            continue
        result["with_row"] += 1
        for i in on_row:
            if not lines[i].startswith("|"):
                continue
            at = [x for x, cell in enumerate(cells(lines[i])) if pattern.search(cell)]
            above = i - 1
            found = False
            while above >= 0 and lines[above].startswith("|") and not found:
                header = cells(lines[above])
                found = any(x < len(header) and column in header[x] for x in at)
                above -= 1
            if found:
                result["with_column"] += 1
                break
    return result, missed


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--json", type=Path, help="write the full result here")
    parser.add_argument("--missed", action="store_true", help="list the labels that were not found")
    args = parser.parse_args()

    papers = json.loads((HERE / "papers.json").read_text())["papers"]
    labels = {p["key"]: json.loads((HERE / "labels" / f'{p["key"]}.json').read_text()) for p in papers}
    totals = {
        "headings": sum(len(l["headings"]) for l in labels.values()),
        "references": len(labels),
        "numbers": sum(len(l["numbers"]) for l in labels.values()),
    }
    full = {"totals": totals, "extractors": {}}

    for name in EXTRACTORS:
        run_path = args.out / name / "run.json"
        if not run_path.exists():
            continue
        run = json.loads(run_path.read_text())
        per_paper = {}
        for paper in papers:
            key = paper["key"]
            record = dict(run["papers"].get(key, {"failure": "not run"}))
            path = args.out / name / f"{key}.txt"
            if "failure" not in record:
                record["score"], record["missed"] = score(path.read_text(encoding="utf-8"), labels[key])
            per_paper[key] = record
        full["extractors"][name] = {"peak_rss_mb": run.get("peak_rss_mb"), "papers": per_paper}

    measures = ["headings", "headings_marked", "references_heading", "references", "numbers", "with_row", "with_column"]
    denominators = [totals["headings"], totals["headings"], totals["references"], totals["references"]] + [totals["numbers"]] * 3
    print("| Extractor | Papers extracted | Headings found | Headings marked | Reference heading | Reference start | Numbers found | With row label | With column label | Seconds per paper (median, max) | Peak memory MB | Failures |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for name, data in full["extractors"].items():
        done = {k: r for k, r in data["papers"].items() if "failure" not in r}
        failed = [k for k, r in data["papers"].items() if "failure" in r]
        sums = [sum(r["score"][m] for r in done.values()) for m in measures]
        seconds = sorted(r["seconds"] for r in done.values())
        median = seconds[len(seconds) // 2] if len(seconds) % 2 else (seconds[len(seconds) // 2 - 1] + seconds[len(seconds) // 2]) / 2
        row = [name, f"{len(done)}/{len(papers)}"]
        row += [f"{s}/{d}" for s, d in zip(sums, denominators)]
        row += [f"{median:.1f}, {seconds[-1]:.1f}", str(data["peak_rss_mb"]), ", ".join(failed) or "none"]
        print("| " + " | ".join(row) + " |")

    print()
    print("| Paper | Labels (headings, numbers) | " + " | ".join(full["extractors"]) + " |")
    print("|---|---|" + "---|" * len(full["extractors"]))
    for paper in papers:
        key = paper["key"]
        row = [key, f'{len(labels[key]["headings"])}, {len(labels[key]["numbers"])}']
        for data in full["extractors"].values():
            record = data["papers"][key]
            if "failure" in record:
                row.append("failed")
            else:
                s = record["score"]
                row.append(f'{s["headings"]}({s["headings_marked"]}) · {s["references_heading"]}/{s["references"]} · {s["numbers"]} · {s["with_row"]} · {s["with_column"]} · {record["seconds"]:.1f} s')
        print("| " + " | ".join(row) + " |")
    print()
    print("Cell: headings found (marked) · reference heading/start · numbers found · with row label · with column label · seconds.")

    if args.missed:
        for name, data in full["extractors"].items():
            for key, record in data["papers"].items():
                for kind, items in record.get("missed", {}).items():
                    for item in items:
                        print(f"missed {name} {key} {kind}: {item}")
    if args.json:
        args.json.write_text(json.dumps(full, indent=2, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
