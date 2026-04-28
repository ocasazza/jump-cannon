# vault-search

Standalone HTTP full-text search backend for an Obsidian vault, backed by
[Tantivy](https://github.com/quickwit-oss/tantivy) (Lucene-class FTS in
Rust).

Designed as a reusable component: any of the `vault-*` visualization or
analysis tools can spawn a `vault-search` instance (or hit a long-lived
one) and offload search off the main UI thread instead of each tool
re-implementing it.

## Quick start

```sh
nix run .#vault-search -- --vault ./vault --port 0
# stderr: vault-search: listening on http://127.0.0.1:NNNNN
```

The bound port is printed to stderr as one line so parent processes can
scrape it cheaply. With `--port 0` (default) the OS picks a free port.

## CLI

```
vault-search [OPTIONS]

  --vault PATH         Vault root          (default: $OBSIDIAN_VAULT or ./vault)
  --port N             HTTP port           (default: 0 = auto-pick)
  --host HOST          Bind host           (default: 127.0.0.1)
  --cache-dir PATH     Index location      (default: ~/.cache/vault-search/<vault-hash>/)
  --rebuild            Force clean reindex
  --include-hippo      Index 30-Knowledge-Base/_hippo/** too
  --watch              Incremental on file changes (NOT YET IMPLEMENTED)
  --log LEVEL          trace|debug|info|warn|error (default: info)
```

## HTTP endpoints

All responses are JSON. `Access-Control-Allow-Origin: *` is set for
local-only browser consumers.

| Endpoint              | Description |
| --------------------- | ----------- |
| `GET /health`         | `{ status: "ready"\|"indexing", indexed, total, vault }` |
| `GET /search?q=…`     | Ranked BM25 results with snippets. `limit` (default 200, max 5000), `offset`. Returns `{ total, results: [{id, score, snippet}] }`. |
| `GET /ids?q=…`        | Hot path for filter UIs — id list only, no snippets. Returns `{ ids, total }`. |
| `GET /node/:id`       | URL-encoded id lookup. Returns `{ id, title, tags, mtime }` or 404. |
| `POST /reindex`       | Triggers a background full reindex; returns `202 { status: "reindexing" }`. |

The `QueryParser` searches across `body`, `title`, and `tags`, with
`title` boosted ×3 and `tags` ×2. Default conjunctive matching (every
term must appear); use Tantivy's standard query syntax for OR / phrase /
prefix / fuzzy.

## Schema

| Field   | Type                        | Notes |
| ------- | --------------------------- | ----- |
| `id`    | STRING + STORED             | Vault-relative path with `.md` stripped (e.g. `30-Knowledge-Base/IT-Ops/README`). Same id scheme as `vault-links`. |
| `title` | TEXT (en_stem) + STORED     | First H1 in body, fallback to file basename. |
| `body`  | TEXT (en_stem) + STORED     | Frontmatter-stripped body, capped at 64 KiB. |
| `tags`  | TEXT (en_stem) + STORED     | Best-effort regex extraction from frontmatter `tags:` (inline list, block list, or scalar). |
| `mtime` | U64 + STORED + INDEXED + FAST | Seconds since epoch; used for incremental reindex. |

## Caching & incremental reindex

The Tantivy index lives at `~/.cache/vault-search/<vault-hash>/`
(override with `--cache-dir`). `<vault-hash>` is the first 16 hex chars
of `sha256(canonical_vault_path)` — different vaults get different cache
dirs.

On warm start, `vault-search` walks the vault, compares each file's
mtime against the stored mtime, and only re-indexes notes that changed
or are new. Notes that vanished from disk are deleted from the index.
This makes warm starts effectively free even on the ~10k-note real
vault.

Pass `--rebuild` to wipe and start over.

## Exclusion contract

Defaults exclude (recursively, at any depth):

- `**/.obsidian/**`
- `**/.git/**`
- `**/.jj/**`
- `**/Excalidraw/**`
- `**/Ink/**`
- `**/_hippo/**` — toggle back in with `--include-hippo`
- `**/*.base`, `**/*.canvas`

This **fixes a bug in `vault-links`** (the bash-based wikilink
extractor): its `--glob '!.obsidian/**'` form doesn't recurse the way
`ignore`'s `**/.obsidian/**` override does, so vault-links
historically indexed nested `.obsidian/` directories. `vault-search`
gets this right out of the box via the `ignore` crate's
`OverrideBuilder`.

## Build

```sh
# from the repo root
nix build .#vault-search
./result/bin/vault-search --help

# inside the package dir, with a Rust toolchain on PATH
cd nix/packages/vault-search
cargo build --release
cargo test
```
