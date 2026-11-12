# Test fixtures

Hand-written, synthetic API responses. Every work, person, venue, and URL in
them is invented: DOIs use the `10.5555/` example prefix, arXiv ids use
numbers no month reaches (`2601.999xx`, `hep-th/9901999`), and hosts are
`example.org`, `example.edu`, or the API's own. They keep the response
shapes the parsers read, as documented by each API:

- `crossref-*.json`: a `/works/{doi}` reply from the Crossref REST API.
- `openalex-*.json`: a single work and a `/works?search=` page from OpenAlex.
- `arxiv-*.xml`: Atom feeds from the arXiv API (`/api/query`).
- `arxiv-oai-*.xml`: `GetRecord` replies in the `arXivRaw` format from arXiv's
  OAI-PMH interface (`oaipmh.arxiv.org/oai`), modelled on a recorded reply
  for a paper whose v2 is withdrawn (`<source_type>I</source_type>`, `0kb`).
- `crossref-retracted.json` and `crossref-retraction-notice.json` follow
  recorded Retraction Watch `updated-by` and `update-to` entries.
