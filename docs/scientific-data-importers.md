# Scientific data importers: formats × sources on the package framework

Status: implemented (2026-09). Eight shipped packages, five engine mechanics
added, live-verified against PRIDE Archive and DataCite.

This note covers importing common scientific and research data — proteomics,
metabolomics, genomics, DOI metadata — through jump-cannon's importer
grammar framework (`crates/importer`, package format 3). The worked example
throughout is PRIDE Archive project
[PXD084037](https://www.ebi.ac.uk/pride/archive/projects/PXD084037)
(silkworm-cocoon paleoproteomics, LC-MS/MS).

The framework rule from `AGENTS.md` applies unchanged: **importers are
packages, not crates**. A *format* is a package (mechanism). A *source* is
an instance binding (configuration): endpoint, variables, token, poll
interval. Mixing and matching means binding any package to any source that
speaks its format — no Rust is added per source.

## Formats (the packages)

Seven distinct formats across eight packages under
`charts/jump-cannon/packages/`:

| Package | Format | Engine | Graph shape |
|---|---|---|---|
| `pride-archive.toml` | PRIDE Archive REST v2 (JSON) | json | project + files, `contains` edges, CV facets |
| `pride-search.toml` | PRIDE Archive REST v2 (JSON) | json | discovery: projects clustered by shared facets |
| `datacite-dois.toml` | DataCite REST (JSON) | json | DOI works; publisher/type/year facets |
| `sdrf-proteomics.toml` | SDRF-Proteomics (TSV) | pest | one node per raw MS file; sample as tag; extension as type |
| `isa-tab-assay.toml` | ISA-Tab assay table (TSV) | pest | one node per vendor raw file; sample as tag |
| `mztab.toml` | mzTab 1.0 (TSV) | pest | proteins + peptides; PSM evidence edges |
| `fasta.toml` | FASTA (text) | pest | one node per sequence; sp/tr type |
| `gff3.toml` | GFF3 (TSV) | pest | one node per feature; type column as canvas type |

Each pest package ships a sibling example input
(`packages/examples/<stem>.txt`) that a cargo test parses and validates
against the full discovery contract — the examples are executable
documentation, and a grammar edit that breaks its example fails CI.

## Sources (the instance bindings)

Seven sources bind to those formats today, with zero new code:

| Source | Binds | How |
|---|---|---|
| **PRIDE Archive** (EBI) | `pride-archive`, `pride-search` (JSON); `sdrf-proteomics`, `mztab`, `fasta` (files it publishes) | endpoint `https://www.ebi.ac.uk/pride/ws/archive/v2`; SDRF/mzTab/FASTA fetched from the project's public file locations |
| **ProteomeXchange / MassIVE** (UCSD) | `sdrf-proteomics`, `mztab`, `fasta` | same SDRF/mzTab formats, MSV accessions |
| **MetaboLights** (EBI) | `isa-tab-assay` | study `a_*.txt` assay tables from the FTP tree |
| **BioStudies / ArrayExpress** (EBI) | `isa-tab-assay`, `sdrf-proteomics` | SDRF/MAGE-TAB-flavored assay exports |
| **ENA / Ensembl / RefSeq** | `fasta`, `gff3` | sequence and annotation downloads |
| **UniProt** | `fasta` | proteome FASTA (`sp`/`tr` headers become the node type) |
| **Zenodo + any DataCite member** (Dryad, Figshare, PANGAEA, …) | `datacite-dois` | endpoint `https://api.datacite.org`; the repository is a query choice, not code |

Mix-and-match examples:

- PXD084037 end-to-end: `pride-archive` (project + file graph) → the same
  accession's `SDRF` file through `sdrf-proteomics` (sample→run graph) →
  its MaxQuant search database through `fasta` (protein catalog) → the
  citing dataset DOIs through `datacite-dois`.
- One grammar, many archives: `sdrf-proteomics` reads PRIDE SDRF, MassIVE
  SDRF, and quantms pipeline output unchanged.
- One registry, many repositories: `datacite-dois` with
  `query=publisher:Zenodo AND silkworm`, `query=publisher:Dryad`, …

## Selectable importers and user-supplied options

A *selectable importer* is a package plus a bound instance. User options
are the package's declared `[[parser.variables]]`, supplied at bind time
(`--importer-var name=value` / `JUMP_CANNON_IMPORTER_VAR`), never baked
into the package:

| Package | Variable | Default | Meaning |
|---|---|---|---|
| `pride-archive` | `accession` | *(required)* | PXD/MSV accession; preflight fails loudly naming near matches when mistyped |
| `pride-search` | `keyword` | *(required)* | free text / organism / instrument / disease / accession |
| `datacite-dois` | `query` | *(required)* | DataCite query: dataset DOI, free text, or fielded (`publisher:Zenodo AND …`); percent-encoded on the wire |

Instance-level knobs (endpoint, token, poll interval, `source_id`) are the
existing `JUMP_CANNON_IMPORTER_*` surface. Binding example:

```bash
graph-api --source httpjson \
  --importer-manifest pride-archive.toml \
  --importer-endpoint https://www.ebi.ac.uk/pride/ws/archive/v2 \
  --importer-var accession=PXD084037 \
  --importer-source-id pride-pxd084037
```

Pest packages take no variables (the `[parser]` grammar table admits no
placeholders by design); their "user options" are the input path and the
grammar itself, which is runtime-editable in the Importers panel — that is
how layout variants (a different SDRF column family, an NMR extension
list) are handled without recompiles.

## Engine mechanics this added (`crates/importer`)

All shared-engine, no per-source code, per "packages, not crates":

1. **`page_number` pagination** — `paginate = { style = "page_number",
   page_param = "page", size_param = "pageSize", first_page = 0 }`.
   Configurable names cover PRIDE (`page`/`pageSize`, 0-based) and DataCite
   (`page[number]`/`page[size]`, 1-based). Param names are validated
   against query-smuggling characters.
2. **Root-array collections** — `items_pointer = ""` selects the whole
   response document (PRIDE returns bare JSON arrays).
3. **Array-valued edge pointers** — an edge rule's `value_pointer` naming
   a JSON array of scalars yields one edge per element (PRIDE's
   `projectFileNames` → 27 `contains` edges for PXD084037).
4. **Percent-encoded variables** — variable values may carry spaces,
   slashes, and colons; they are RFC 3986 percent-encoded at interpolation
   (static template text is never encoded). Validation now only rejects
   braces and control characters, ≤512 chars.
5. **Page-number duplicate tolerance** — paginated walks against
   live-mutating remotes (DataCite ranking drift) re-observe records;
   first occurrence wins, consistent with the existing `limit_offset`
   rule. `Pagination::None` collections still hard-fail on duplicates.

## Honest limits (documented in each package header)

- **Pest capture contract**: one rule per role, and a node record consumes
  its whole subtree — a single table row cannot emit both a node and an
  edge. SDRF/ISA-Tab therefore carry samples as *tags* rather than
  sample→file edges; GFF3 `Parent=` hierarchy cannot become edges yet.
  Reserved edge types mark where a future engine release can attach these.
- **No dedupe in pest**: duplicate node ids fail loudly (`DuplicateNodeId`)
  — correct for FASTA/GFF3 identity semantics, and the reason SDRF/ISA-Tab
  key nodes on the unique-per-row data file.
- **Binary formats stay out of pest's reach** (project decision: pest is
  text-only): mzML binary index blocks, NetCDF/HDF5 (mz5, Andi-MS),
  proprietary vendor formats. These need the connector/decoder unwrap path
  (`crates/importer-connectors`), not grammars.
- **Pest runtime binding is filesystem** today (`--source pest <file>`);
  fetching SDRF/mzTab/FASTA over HTTP through `importer-connectors`' https
  `SourceConnector` into the pest engine is the designed composition, not
  yet wired into graph-api's runtime.
- **mzML/mzIdentML/pepXML** (PSI XML) are text and *pest-reachable*, but
  metadata-subset grammars for them are not written yet; the line-oriented
  TSV family was the higher-value first cut.
- **DataCite deep walks**: page-number pagination caps at 10k records
  server-side; `/meta/total` is checked against `limits.nodes` so an
  over-broad query fails loudly. Cursor pagination is the follow-up
  mechanic if bulk harvest becomes a need.

## Verification

- `cargo test -p importer` — 61 tests, including:
  - `shipped_packages_validate`: every TOML under `packages/` validates;
  - `shipped_pest_packages_parse_their_examples`: every pest package parses
    its example input and satisfies `schema.validate_result`;
  - page-number walk/exhaustion/custom param names/first-page/record-bound;
    root-array mapping; array edge pointers; variable percent-encoding;
    page-number duplicate tolerance.
- Live smoke (throwaway test, since removed, against the real APIs):
  - `pride-archive` + PXD084037 → 28 nodes (1 project + 27 files), 27
    `contains` edges, 0 unresolved;
  - `pride-search` + `keyword=silkworm` → 35 project nodes;
  - `datacite-dois` + `query=publisher:Zenodo AND silkworm` → 683 work
    nodes across 7 page-number pages.
