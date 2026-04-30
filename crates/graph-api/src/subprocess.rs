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
    /// to a probe (or times out after ~10s).
    pub async fn spawn(vault_root: &Path) -> Result<Self, String> {
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
            .arg("127.0.0.1")
            .stdout(Stdio::piped())
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
