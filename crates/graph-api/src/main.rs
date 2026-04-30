use std::path::PathBuf;
use clap::Parser;

use graph_api::{router, vault_loader, AppState};

/// Resolution order: CLI flag > env var (VAULT_ROOT) > .env file > current directory.
//
// Future: --backend-url for a remote graph-api split deploy; --config-file for
// TOML config in ~/.config/jump-cannon/.
#[derive(Parser)]
#[command(name = "graph-api")]
struct Args {
    /// Vault root directory. Falls back to $VAULT_ROOT, then .env, then $PWD.
    #[arg(short, long, env = "VAULT_ROOT")]
    vault_root: Option<PathBuf>,
    /// Listen port (0 = OS picks). Override with $GRAPH_API_PORT.
    #[arg(short, long, env = "GRAPH_API_PORT", default_value_t = 0)]
    port: u16,
    /// Don't auto-open the browser. Override with $GRAPH_API_NO_BROWSER=1.
    #[arg(long, env = "GRAPH_API_NO_BROWSER")]
    no_browser: bool,
    /// Dev mode: serve /assets and / from this directory at request time
    /// instead of from the embedded bundle. JS/CSS/HTML edits show up on
    /// browser refresh without rebuild. Set $GRAPH_RENDERER_ASSETS_DIR or
    /// pass --assets-dir.
    #[arg(long, env = "GRAPH_RENDERER_ASSETS_DIR")]
    assets_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env if present in cwd or any parent. No error if missing.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let vault_root = args
        .vault_root
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let graph = vault_loader::load(&vault_root);

    // Spawn vault-search before binding so /search can proxy to it.
    // Falls back gracefully to title-contains if the binary isn't on PATH
    // or fails to start (see crates/graph-api/src/server.rs::search).
    let vault_search = match graph_api::subprocess::VaultSearch::spawn(&vault_root).await {
        Ok(vs) => {
            tracing::info!(port = vs.port, "vault-search subprocess up");
            Some(std::sync::Arc::new(vs))
        }
        Err(e) => {
            tracing::warn!(
                "vault-search unavailable: {e}; /search falls back to title-contains"
            );
            None
        }
    };

    if let Some(dir) = &args.assets_dir {
        tracing::info!(assets_dir = %dir.display(), "dev mode: serving assets from disk");
    }
    let state = AppState::new(vault_root, graph, vault_search, args.assets_dir);

    let app = router(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let url = format!("http://{}/", bound);
    tracing::info!(%url, "listening");
    println!("{}", url);

    if !args.no_browser {
        graph_api::browser::open_url(&url);
    }

    axum::serve(listener, app).await?;
    Ok(())
}
