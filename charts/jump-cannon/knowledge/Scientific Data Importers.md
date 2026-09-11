---
doctype: guide
area: data
audience: [user, developer, operator, agent]
status: current
tags: [jump-cannon, importer, scientific-data, proteomics, genomics, chemistry]
---

# Scientific Data Importers

Scientific and research datasets import through the shared package
framework: a **format is a package** (mechanism), a **source is an instance
binding** (configuration). See `AGENTS.md` "Importers: packages, not
crates" and [[Importer Runtime]].

Eleven packages ship under `charts/jump-cannon/packages/`:

- **JSON APIs**: `pride-archive` (PRIDE project + file graph with
  `contains` edges), `pride-search` (PRIDE free-text discovery),
  `datacite-dois` (DataCite DOI registry — Zenodo, Dryad, Figshare,
  PANGAEA, … chosen by the query, not code; DOI→DOI `related` citation
  edges and subject tags via the pluck transform).
- **Pest grammars**: `sdrf-proteomics` (SDRF sample↔run tables),
  `isa-tab-assay` (ISA-Tab assay tables, e.g. MetaboLights), `mztab`
  (proteins + peptides with PSM evidence edges), `fasta` (sequence
  databases), `gff3` (genomic annotations), `smiles` (compound lists with
  chemistry feature tags: aromatic, chiral, cyclic, halogenated, charged),
  `mzml` (PSI XML spectrum index: MS1/MS2 + polarity tags), `sdf` (V3000
  molecular graphs: atoms as nodes with element types, bonds as edges,
  ions as cation/anion tags — molecular force layout designed in
  `docs/molecular-force-layout.md`). Each ships an example input under
  `packages/examples/` that a cargo test parses and validates.

User options are the package's declared variables at bind time —
`accession` (PRIDE), `keyword` (PRIDE search), `query` (DataCite) — via
`JUMP_CANNON_IMPORTER_VAR`, plus the endpoint/token/instance knobs.
Pest packages take no variables; their variant surface is the
runtime-editable grammar in the Importers panel.

Engine mechanics added for this family: `page_number` pagination with
configurable param names (PRIDE `page`/`pageSize` 0-based, DataCite
`page[number]`/`page[size]` 1-based), root-array collections
(`items_pointer = ""`), array-valued edge pointers (one edge per array
element), percent-encoded variable values (spaces/colons/slashes legal),
page-number duplicate tolerance (first occurrence wins, like the existing
`limit_offset` rule), the `pluck` edge transform (array-of-objects → one
edge per element, e.g. DataCite relatedIdentifiers), pest `tag_labels`
(static feature tags from grammar-rule matches), and
`tags_element_pointer` (object-array tags, e.g. DataCite subjects).

Known limits: pest cannot emit node+edge from one row (samples are tags in
SDRF/ISA-Tab), pest dedupe is identity-strict (`DuplicateNodeId`),
molecules are one node per compound (atom/bond graphs need a decoder, not
a grammar), mzML imports are index-sized subsets, binary formats
(NetCDF/HDF5) need the connector/decoder path, and pest runtime binding is
filesystem-only today.

Live-verified 2026-09: PRIDE PXD084037 (28 nodes, 27 contains edges),
PRIDE silkworm search (35 projects), DataCite Zenodo silkworm query (683
DOIs, 7 pages, 338 citation edges, 682 subject-tagged), SMILES demo set
(24 compounds grouped by feature tag). Full design and mix-and-match
matrix: `docs/scientific-data-importers.md`.

See also [[Hindsight Importer]], [[Import and Generate]].
