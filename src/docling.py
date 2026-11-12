# Run by `papers read --extraction docling` as `<[papers] docling> -c <this script> ...`.
#   --version         print {"docling": "<version>"}
#   <pdf> <out.md>    convert the PDF and write Docling's Markdown to <out.md>,
#                     with "<!-- page -->" between pages; then print the version line
# Options as measured in the extractor comparison of November 2026: no OCR,
# table structure on.
import json
import sys
from importlib.metadata import version

VERSION = version("docling")
if sys.argv[1:] == ["--version"]:
    print(json.dumps({"docling": VERSION}))
    sys.exit(0)
pdf, out = sys.argv[1:3]

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption

options = PdfPipelineOptions(do_ocr=False, do_table_structure=True)
converter = DocumentConverter(
    format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=options)}
)
markdown = converter.convert(pdf).document.export_to_markdown(
    page_break_placeholder="<!-- page -->"
)
with open(out, "w", encoding="utf-8") as f:
    f.write(markdown if markdown.endswith("\n") else markdown + "\n")
print(json.dumps({"docling": VERSION}))
