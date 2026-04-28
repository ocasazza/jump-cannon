{
  pkgs,
  lib,
  ...
}:
pkgs.rustPlatform.buildRustPackage {
  pname = "vault-search";
  version = "0.1.0";

  src = ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  # Tantivy + ignore + axum are pure-Rust; no native dependencies needed.
  # The default test runner spins up the binary against a temp vault — that
  # works inside the Nix sandbox because everything stays local to /tmp.
  doCheck = true;

  meta = {
    description = "HTTP full-text search backend for an Obsidian vault (Tantivy-backed)";
    longDescription = ''
      Walks an Obsidian vault, builds a Tantivy index of .md bodies with
      frontmatter stripped, and serves a minimal HTTP API for ranked search,
      id-only matching, and per-node lookup. Persists the index under
      ~/.cache/vault-search/<vault-hash>/ so warm starts are instant.

      Designed as a reusable backend component: any vault-* tool can spawn
      it (or hit a long-lived instance) and offload search off the main UI
      thread. Mirrors vault-links's exclusion contract by default
      (.obsidian/, .git/, .jj/, Excalidraw/, Ink/, _hippo/, *.base, *.canvas)
      and fixes the recursive-glob bug that hides nested .obsidian/
      directories from the bash extractor. Pass --include-hippo to opt the
      persistent hippo memory store back in.

      HTTP endpoints: /health, /search, /ids, /node/:id, POST /reindex.
    '';
    mainProgram = "vault-search";
    platforms = lib.platforms.unix;
  };
}
