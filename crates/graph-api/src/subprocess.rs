//! Spawn and manage the vault-search Tantivy subprocess.
//
// Future: when graph-api lives on luna, vault-search runs alongside it on
// the same machine; the renderer doesn't need to know.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

pub struct VaultSearch {
    pub child: Child,
    pub port: u16,
}

impl VaultSearch {
    /// Spawn vault-search on a free port. Blocks until the server responds
    /// to a probe (or times out after ~10s). `rebuild = true` passes
    /// `--rebuild`, forcing a clean reindex (used by the watcher path
    /// after a vault reload).
    pub async fn spawn(vault_root: &Path) -> Result<Self, String> {
        Self::spawn_inner(vault_root, false).await
    }

    /// Like [`spawn`] but passes `--rebuild` to force a clean reindex.
    /// GUESS: vault-search has no explicit "refresh" RPC, so the
    /// reload path drops the old child and spawns a fresh one with
    /// `--rebuild`. If a refresh hook lands later, prefer that.
    pub async fn spawn_rebuild(vault_root: &Path) -> Result<Self, String> {
        Self::spawn_inner(vault_root, true).await
    }

    async fn spawn_inner(vault_root: &Path, rebuild: bool) -> Result<Self, String> {
        // Pick a free port up-front so we can both pass it as --port and
        // know what to probe. There's a tiny TOCTOU window between drop
        // and child bind, but it's fine in practice for localhost dev.
        let port = pick_port()?;
        let mut cmd = Command::new("vault-search");
        cmd.arg("--vault")
            .arg(vault_root)
            .arg("--port")
            .arg(port.to_string())
            .arg("--host")
            .arg("127.0.0.1");
        if rebuild {
            cmd.arg("--rebuild");
        }
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd
            .spawn()
            .map_err(|e| format!("spawn vault-search: {e}"))?;
        // Wait up to ~10s for it to come up. vault-search runs the initial
        // index synchronously before binding, so first start on a fresh
        // vault may need a bit; bump if you see startup races.
        for _ in 0..50 {
            if probe(port).await {
                return Ok(Self { child, port });
            }
            sleep(Duration::from_millis(200)).await;
        }
        Err("vault-search did not respond on port within 10s".into())
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Ask vault-search to incrementally re-index the given vault-relative
    /// paths via `POST /refresh`. On success, returns
    /// `(updated, deleted, skipped)`. On any failure (HTTP, JSON, non-2xx)
    /// returns `Err` so the caller can fall back to a full respawn.
    pub async fn refresh(&self, paths: &[String]) -> Result<(usize, usize, usize), String> {
        let url = format!("{}/refresh", self.url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("client: {e}"))?;
        let body = serde_json::json!({ "paths": paths });
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("POST {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("refresh HTTP {status}: {text}"));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("decode refresh response: {e}"))?;
        let updated = v.get("updated").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let deleted = v.get("deleted").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let skipped = v.get("skipped").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        Ok((updated, deleted, skipped))
    }
}

impl Drop for VaultSearch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pick_port() -> Result<u16, String> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

async fn probe(port: u16) -> bool {
    // vault-search exposes /health (not /healthz). It returns 200 even
    // while indexing — the body's `status` field carries the readiness.
    // For our purposes any 2xx means the HTTP server is up; the proxied
    // /ids handler will surface NotReady itself if a query lands early.
    let url = format!("http://127.0.0.1:{}/health", port);
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}
