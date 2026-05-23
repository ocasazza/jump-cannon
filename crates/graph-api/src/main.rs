use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;

use graph_api::{
    compute_broker::ComputeBroker,
    progress::ProgressLog,
    router,
    vault_loader,
    AppState,
};

/// Resolution order: CLI flag > env var (VAULT_ROOT) > .env file > current directory.
#[derive(Parser)]
#[command(name = "graph-api")]
struct Args {
    /// Vault root directory. Falls back to $VAULT_ROOT, then .env, then $PWD.
    #[arg(short, long, env = "VAULT_ROOT")]
    vault_root: Option<PathBuf>,
    /// Listen port (0 = OS picks). Override with $GRAPH_API_PORT.
    #[arg(short, long, env = "GRAPH_API_PORT", default_value_t = 0)]
    port: u16,
    /// Listen host. Defaults to 127.0.0.1; set 0.0.0.0 (or [::]) for
    /// container bind. `$GRAPH_API_HOST` is the matching env var.
    #[arg(long, env = "GRAPH_API_HOST", default_value = "127.0.0.1")]
    host: String,
    /// Don't auto-open the browser. Override with $GRAPH_API_NO_BROWSER=1.
    #[arg(long, env = "GRAPH_API_NO_BROWSER")]
    no_browser: bool,
    /// Dev mode: serve /assets and / from this directory at request time
    /// instead of from the embedded bundle.
    #[arg(long, env = "GRAPH_RENDERER_ASSETS_DIR")]
    assets_dir: Option<PathBuf>,
    /// URL of the graph-compute gRPC worker. When unset, the compute broker
    /// is disabled.
    #[arg(long, env = "JUMP_CANNON_COMPUTE_URL")]
    compute_url: Option<String>,
    /// Disable the filesystem watcher. Useful for one-shot CLI usage; the
    /// docker container leaves this unset so live reload works.
    #[arg(long, env = "GRAPH_API_NO_WATCH")]
    no_watch: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    // Shared progress log. Surfaces "Scanning vault / Computing metrics /
    // Seeding positions / Rebuilding search index" task bars to the
    // frontend footer via GET /progress.
    let progress = Arc::new(ProgressLog::new());

    // Initial vault load — emit progress events so the bootstrap fetch
    // sees a populated /progress response on the first poll.
    let graph = vault_loader::load_with_progress(&vault_root, Some(&progress));

    // Spawn vault-search before binding so /search can proxy to it.
    let vault_search_id = progress.start("ingest", "Spawning vault-search");
    let vault_search = match graph_api::subprocess::VaultSearch::spawn(&vault_root).await {
        Ok(vs) => {
            tracing::info!(port = vs.port, "vault-search subprocess up");
            progress.finish(vault_search_id);
            Some(std::sync::Arc::new(vs))
        }
        Err(e) => {
            tracing::warn!(
                "vault-search unavailable: {e}; /search falls back to title-contains"
            );
            progress.fail(vault_search_id, format!("{e}"));
            None
        }
    };

    if let Some(dir) = &args.assets_dir {
        tracing::info!(assets_dir = %dir.display(), "dev mode: serving assets from disk");
    }

    let compute_broker = ComputeBroker::new();
    if let Some(compute_url) = args.compute_url.clone() {
        let broker = compute_broker.clone();
        tokio::spawn(async move {
            match broker.connect(compute_url.clone()).await {
                Ok(()) => tracing::info!(url = %compute_url, "connected to graph-compute worker"),
                Err(e) => tracing::warn!(
                    url = %compute_url,
                    "graph-compute unreachable: {e}; /graph/layout/stream will return 503"
                ),
            }
        });
    } else {
        tracing::info!(
            "compute broker disabled (no --compute-url / JUMP_CANNON_COMPUTE_URL); \
             /graph/layout/stream will return 503"
        );
    }

    let state = AppState::new(
        vault_root.clone(),
        graph,
        vault_search,
        args.assets_dir,
        compute_broker,
        progress.clone(),
    );

    // Live reload: watch $VAULT_ROOT for `.md` changes and atomically
    // swap a new GraphSnapshot into AppState. Skipped if --no-watch.
    if !args.no_watch {
        graph_api::watcher::spawn(state.clone());
    } else {
        tracing::info!("filesystem watcher disabled (--no-watch)");
    }

    let app = router(state);

    let host: std::net::IpAddr = args.host.parse().unwrap_or_else(|_| {
        tracing::warn!(host = %args.host, "invalid --host, defaulting to 127.0.0.1");
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
    });
    let addr = std::net::SocketAddr::new(host, args.port);
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
