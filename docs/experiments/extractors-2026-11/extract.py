# /// script
# requires-python = ">=3.11"
# dependencies = ["beautifulsoup4>=4.12", "lxml>=5"]
# ///
"""Extract the ten papers of papers.json with one extractor.

    uv run extract.py plain      --sources <dir with KEY.pdf, KEY.txt> --out <scratch>
    uv run extract.py layout     --sources ... --out ...
    uv run extract.py arxiv-html --sources ... --out ...
    uv run --with docling    extract.py docling --sources ... --out ...
    uv run --with marker-pdf extract.py marker  --sources ... --out ...

It writes <out>/<extractor>/<key>.txt and <out>/<extractor>/run.json, which
holds the seconds per paper, the failures and the peak memory of the process.
Give --out a directory outside every repository. The model extractors download
their weights on the first run; set HF_HOME and MODEL_CACHE_DIR to choose where.
"""

import argparse
import json
import re
import resource
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).parent
USER_AGENT = "extractor-comparison-script"


def plain(paper, sources, work):
    """The registry's current text, as stored. It is not extracted again."""
    return (sources / f"{paper['key']}.txt").read_text(encoding="utf-8")


def layout(paper, sources, work):
    """`pdftotext -layout`, the `layout` mode of `papers read`."""
    done = subprocess.run(
        ["pdftotext", "-layout", "-enc", "UTF-8", str(sources / f"{paper['key']}.pdf"), "-"],
        check=True,
        capture_output=True,
    )
    return done.stdout.decode("utf-8")


def arxiv_html(paper, sources, work):
    """arXiv's HTML (LaTeXML) of the same version as the PDF, as Markdown-like text."""
    if not paper["arxiv"]:
        raise LookupError("no arXiv id in the registry")
    if not paper["arxiv_html"]:
        raise LookupError("arXiv has no HTML or LaTeX source for this version")
    cached = work / f"{paper['key']}.html"
    if not cached.exists():
        request = urllib.request.Request(
            f"https://arxiv.org/html/{paper['arxiv']}", headers={"User-Agent": USER_AGENT}
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            cached.write_bytes(response.read())
        time.sleep(3)  # arXiv asks for at most one request every three seconds
    return latexml_to_text(cached.read_bytes())


def latexml_to_text(html):
    """One line per heading, paragraph, list item, caption and table row.

    Headings start with `#`. A table row is `| cell | cell |`; a cell that spans
    rows or columns repeats its text in each place, so that a value keeps its
    row label on its line and its column label in its column.
    """
    from bs4 import BeautifulSoup, NavigableString, Tag

    soup = BeautifulSoup(html, "lxml")
    root = soup.select_one("article.ltx_document") or soup.body
    for junk in root.select("script, style, nav, annotation, annotation-xml, button, .ltx_pagination"):
        junk.decompose()

    def classes(node):
        return node.get("class") or []

    def inline(node):
        parts = []

        def walk(n):
            if isinstance(n, NavigableString):
                parts.append(str(n))
            elif n.name == "math":
                parts.append(n.get("alttext") or n.get_text())
            elif n.name == "br":
                parts.append(" ")
            else:
                for child in n.children:
                    walk(child)

        walk(node)
        return re.sub(r"\s+", " ", "".join(parts)).strip()

    def table_rows(table):
        grid = {}
        rows = [r for r in table.find_all(class_="ltx_tr") if r.find_parent(class_="ltx_tabular") is table]
        for y, row in enumerate(rows):
            x = 0
            for cell in row.find_all(class_=["ltx_td", "ltx_th"]):
                if cell.find_parent(class_="ltx_tr") is not row:
                    continue
                while (y, x) in grid:
                    x += 1
                text = inline(cell).replace("|", "\\|")
                for dy in range(int(cell.get("rowspan") or 1)):
                    for dx in range(int(cell.get("colspan") or 1)):
                        grid[(y + dy, x + dx)] = text
                x += int(cell.get("colspan") or 1)
        width = 1 + max((x for _, x in grid), default=0)
        for y in range(len(rows)):
            yield "| " + " | ".join(grid.get((y, x), "") for x in range(width)) + " |"

    lines = []

    def block(node):
        if not isinstance(node, Tag):
            return
        if re.fullmatch(r"h[1-6]", node.name):
            lines.append("#" * int(node.name[1]) + " " + inline(node))
        elif "ltx_tabular" in classes(node):
            lines.append("")
            lines.extend(table_rows(node))
            lines.append("")
        elif node.name in ("p", "figcaption", "li", "dt", "dd") and not node.find(class_="ltx_tabular"):
            text = inline(node)
            if text:
                lines.append(text)
                lines.append("")
        else:
            for child in node.children:
                block(child)

    block(root)
    return re.sub(r"\n{3,}", "\n\n", "\n".join(lines)).strip() + "\n"


_docling = None


def docling(paper, sources, work):
    """Docling: layout model and TableFormer on the PDF, no OCR, Markdown export."""
    global _docling
    if _docling is None:
        from docling.datamodel.base_models import InputFormat
        from docling.datamodel.pipeline_options import PdfPipelineOptions
        from docling.document_converter import DocumentConverter, PdfFormatOption

        options = PdfPipelineOptions(do_ocr=False, do_table_structure=True)
        _docling = DocumentConverter(
            format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=options)}
        )
    return _docling.convert(sources / f"{paper['key']}.pdf").document.export_to_markdown()


_marker = None


def marker(paper, sources, work):
    """Marker: Surya layout, table and text models on the PDF, Markdown output."""
    global _marker
    if _marker is None:
        from marker.converters.pdf import PdfConverter
        from marker.models import create_model_dict

        _marker = PdfConverter(artifact_dict=create_model_dict())
    from marker.output import text_from_rendered

    text, _, _ = text_from_rendered(_marker(str(sources / f"{paper['key']}.pdf")))
    return text


EXTRACTORS = {
    "plain": plain,
    "layout": layout,
    "arxiv-html": arxiv_html,
    "docling": docling,
    "marker": marker,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument("extractor", choices=EXTRACTORS)
    parser.add_argument("--sources", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--keys", nargs="*", help="default: all of papers.json")
    args = parser.parse_args()

    papers = json.loads((HERE / "papers.json").read_text())["papers"]
    if args.keys:
        papers = [p for p in papers if p["key"] in args.keys]
    out = args.out / args.extractor
    work = args.out / "download"
    out.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    if args.extractor == "layout" and not shutil.which("pdftotext"):
        sys.exit("pdftotext is not on PATH")

    run_path = out / "run.json"
    run = json.loads(run_path.read_text()) if run_path.exists() else {"papers": {}}
    for paper in papers:
        key = paper["key"]
        started = time.monotonic()
        record = {}
        try:
            text = EXTRACTORS[args.extractor](paper, args.sources, work)
            if not text.strip():
                raise ValueError("empty output")
            (out / f"{key}.txt").write_text(text, encoding="utf-8")
        except Exception as error:  # every failure of an extractor is a result to report
            record["failure"] = f"{type(error).__name__}: {error}"
            (out / f"{key}.txt").unlink(missing_ok=True)
        record["seconds"] = round(time.monotonic() - started, 2)
        run["papers"][key] = record
        print(key, record, flush=True)
    # ru_maxrss is bytes on macOS and kilobytes on Linux.
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    run["peak_rss_mb"] = round(peak / (1024 * 1024 if sys.platform == "darwin" else 1024))
    run_path.write_text(json.dumps(run, indent=2) + "\n")


if __name__ == "__main__":
    main()
