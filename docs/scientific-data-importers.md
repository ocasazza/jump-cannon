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
| `smiles.toml` | SMILES compound list (text) | pest | one node per compound; feature tags (aromatic/chiral/cyclic/halogenated/charged) from the notation itself |
| `mzml.toml` | mzML 1.1 (PSI XML) | pest | spectrum index: one node per spectrum; MS1/MS2 + polarity tags |
| `sdf.toml` | SDF/MOL V3000 (text) | pest | molecular graph: one node per atom (element as type), one edge per bond, ions as cation/anion tags |

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
| **PRIDE / MetaboLights / Metabolomics Workbench** raw runs | `mzml` | mzML downloads; bind index-sized files (see the size note in the package header) |
| **PubChem / ChEBI / RDKit / Open Babel** | `smiles` | SMILES list exports, one compound per line, optional tab + name |
| **Maestro / RDKit / Open Babel / PubChem** structures | `sdf` | V3000 exports, one molecule per file; molecular force layout is designed in `docs/molecular-force-layout.md` |

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
6. **`pluck` edge transform** — `transform = "pluck"` + `element_pointer`
   reduces each object in an array to one value (DataCite
   `relatedIdentifiers[].relatedIdentifier` → 338 `related` DOI→DOI citation
   edges inside the Zenodo silkworm set; those edges are what Louvain
   communities compute over).
7. **Pest `tag_labels`** — `[parser.captures.tag_labels]` maps a grammar
   rule to a static tag pushed onto every node whose subtree matches it
   (SMILES `aromatic`/`chiral`/`cyclic`/`halogenated`/`charged`; mzML
   `MS1`/`MS2`/`positive`/`negative`). Scalar captures still recurse, so
   labels fire inside a composite id rule (the SMILES id tokenizes itself).
8. **`tags_element_pointer`** — the same pluck for node tags: DataCite
   `subjects[].subject` → 682 tagged works, so the Tags view groups the
   registry by subject instead of showing `(untagged)`.

## Honest limits (documented in each package header)

- **Pest capture contract**: one rule per role, and a node record consumes
  its whole subtree — a single table row cannot emit both a node and an
  edge. SDRF/ISA-Tab therefore carry samples as *tags* rather than
  sample→file edges; GFF3 `Parent=` hierarchy cannot become edges yet.
  Reserved edge types mark where a future engine release can attach these.
- **No dedupe in pest**: duplicate node ids fail loudly (`DuplicateNodeId`)
  — correct for FASTA/GFF3/SMILES identity semantics, and the reason
  SDRF/ISA-Tab key nodes on the unique-per-row data file.
- **mzML is an index subset, not a full parse**: the `mzml` package reads
  spectrum ids + cvParam tags and consumes (never decodes) base64 binary
  arrays; bind index-sized inputs (`limits.input_bytes`). mzIdentML/pepXML
  grammars are not written yet.
- **Molecules are not yet graphs of atoms**: the `smiles` package is one
  node per compound with feature tags. Rendering one molecule as its own
  atom/bond graph needs a decoder that explodes SMILES/SDF into atom/bond
  records — the pest capture contract is one node per record with no
  nested graph records or sibling pairing (ring closures, branches).
- **Binary formats stay out of pest's reach** (project decision: pest is
  text-only): NetCDF/HDF5 (mz5, Andi-MS), proprietary vendor formats.
  These need the connector/decoder unwrap path
  (`crates/importer-connectors`), not grammars.
- **Pest runtime binding is filesystem** today (`--source pest <file>`);
  fetching SDRF/mzTab/FASTA over HTTP through `importer-connectors`' https
  `SourceConnector` into the pest engine is the designed composition, not
  yet wired into graph-api's runtime.
- **DataCite deep walks**: page-number pagination caps at 10k records
  server-side; `/meta/total` is checked against `limits.nodes` so an
  over-broad query fails loudly. Cursor pagination is the follow-up
  mechanic if bulk harvest becomes a need.

## Verification

- `cargo test -p importer` — 67 tests, including:
  - `shipped_packages_validate`: every TOML under `packages/` validates;
  - `shipped_pest_packages_parse_their_examples`: every pest package parses
    its example input and satisfies `schema.validate_result`;
  - page-number walk/exhaustion/custom param names/first-page/record-bound;
    root-array mapping; array edge pointers; variable percent-encoding;
    page-number duplicate tolerance; pluck edges; plucked tags;
    `tag_labels` (labels, binding validation, composite-id recursion).
- Live smoke (against the real APIs; throwaway test, since removed):
  - `pride-archive` + PXD084037 → 28 nodes (1 project + 27 files), 27
    `contains` edges, 0 unresolved;
  - `pride-search` + `keyword=silkworm` → 35 project nodes;
  - `datacite-dois` + `query=publisher:Zenodo AND silkworm` → 683 work
    nodes across 7 page-number pages.
- Live graph-api + browser (2026-09, `nix build .#app-web` dist):
  - `datacite-dois` instance → 683 nodes, **338 `related` citation edges**
    (pluck), 682 tagged works grouped by subject in the Tags view;
  - `smiles` instance → 24 compounds grouped by feature tag: aromatic 11,
    cyclic 17, chiral 5, charged 2, halogenated 2.
