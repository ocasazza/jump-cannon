use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

use data_loader::Loader;
use graph_api::{
    compute_broker::{ComputeBroker, RemoteLayout},
    progress::ProgressLog,
    router, vault_loader, AppState,
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
    /// Serve /assets and / from this directory at request time (the
    /// frontend dist, e.g. app/ui/dist). Without it, assets 404.
    #[arg(long, env = "JUMP_CANNON_ASSETS_DIR")]
    assets_dir: Option<PathBuf>,
    /// URL of the graph-compute gRPC worker. When unset, the compute broker
    /// is disabled.
    #[arg(long, env = "JUMP_CANNON_COMPUTE_URL")]
    compute_url: Option<String>,
    /// Disable the filesystem watcher. Useful for one-shot CLI usage; the
    /// docker container leaves this unset so live reload works.
    #[arg(long, env = "GRAPH_API_NO_WATCH")]
    no_watch: bool,
    /// Data source: "obsidian" (default) or "tvix". When "tvix", --vault-root
    /// is interpreted as a path to a .nix file to evaluate.
    #[arg(long, env = "JUMP_CANNON_SOURCE", default_value = "obsidian")]
    source: String,
    /// When --source=tvix, the Nix expression to evaluate. If not provided,
    /// reads from the file at --vault-root (which must be a .nix file).
    #[arg(long, env = "JUMP_CANNON_TVIX_EXPR")]
    tvix_expr: Option<String>,
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

    // Select the data loader.
    let source_kind = data_loader::SourceKind::parse(&args.source)
        .unwrap_or_else(|| {
            tracing::warn!(
                source = %args.source,
                "unknown source; falling back to obsidian"
            );
            data_loader::SourceKind::Obsidian
        });

    let loader: Box<dyn Loader> = match source_kind {
        data_loader::SourceKind::Obsidian => {
            tracing::info!(vault_root = %vault_root.display(), "using obsidian loader");
            Box::new(vault_links::ObsidianLoader::new(vault_root.clone()))
        }
        data_loader::SourceKind::Tvix => {
            let expr = if let Some(ref e) = args.tvix_expr {
                e.clone()
            } else if vault_root.extension().map_or(false, |ext| ext == "nix") {
                std::fs::read_to_string(&vault_root).unwrap_or_else(|e| {
                    tracing::error!(path = %vault_root.display(), error = %e, "failed to read tvix expression file");
                    String::new()
                })
            } else {
                tracing::warn!("--source=tvix but no --tvix-expr and --vault-root is not a .nix file; using empty graph");
                String::new()
            };
            tracing::info!(expr_len = expr.len(), "using tvix loader");
            Box::new(tvix_loader::TvixLoader::new(expr))
        }
    };

    // Shared progress log. Surfaces "Scanning vault / Computing metrics /
    // Seeding positions / Rebuilding search index" task bars to the
    // frontend footer via GET /progress.
    let progress = Arc::new(ProgressLog::new());

    // Initial graph load — emit progress events so the bootstrap fetch
    // sees a populated /progress response on the first poll.
    let graph = vault_loader::load_with_progress(loader.as_ref(), Some(&progress));

    // Spawn vault-search before binding so /search can proxy to it.
    // Only for obsidian — tvix graphs have no filesystem to index.
    let vault_search = if source_kind == data_loader::SourceKind::Obsidian {
        let vault_search_id = progress.start("ingest", "Spawning vault-search");
        match graph_api::subprocess::VaultSearch::spawn(&vault_root).await {
            Ok(vs) => {
                tracing::info!(port = vs.port, "vault-search subprocess up");
                progress.finish(vault_search_id);
                Some(std::sync::Arc::new(vs))
            }
            Err(e) => {
                tracing::warn!("vault-search unavailable: {e}; /search falls back to title-contains");
                progress.fail(vault_search_id, format!("{e}"));
                None
            }
        }
    } else {
        tracing::info!("skipping vault-search (tvix source has no filesystem index)");
        None
    };

    if let Some(dir) = &args.assets_dir {
        tracing::info!(assets_dir = %dir.display(), "dev mode: serving assets from disk");
    }

    let compute_broker = ComputeBroker::new();

    let state = AppState::new(
        vault_root.clone(),
        loader,
        graph,
        vault_search,
        args.assets_dir,
        compute_broker.clone(),
        progress.clone(),
    );

    if let Some(compute_url) = args.compute_url.clone() {
        let broker = compute_broker.clone();
        // ADR-002: pick + tune the remote layout engine from env. Empty/unset
        // ⇒ the worker's startup default (backward compatible).
        let remote_layout = RemoteLayout::from_env();
        let push_state = state.clone();
        tokio::spawn(async move {
            match broker
                .connect_with(compute_url.clone(), remote_layout)
                .await
            {
                Ok(()) => {
                    tracing::info!(url = %compute_url, "connected to graph-compute worker");
                    // Hand the worker the vault graph — its boot graph is a
                    // demo placeholder; remote engines must simulate ours.
                    graph_api::server::push_graph_to_worker(&push_state).await;
                }
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

    // When bound to IPv4 loopback (the dev default), also serve on IPv6
    // loopback at the same port. Safari/WebKit resolve `localhost` to `::1`
    // first, so without this a user who opens `http://localhost:<port>` can't
    // even reach the page. Best-effort: a bind failure here (no IPv6 stack,
    // port race) is non-fatal — the IPv4 listener still serves.
    if host == std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)) {
        let v6 = std::net::SocketAddr::new(
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            bound.port(),
        );
        match tokio::net::TcpListener::bind(v6).await {
            Ok(l6) => {
                tracing::info!(url = %format!("http://{}/", v6), "also listening (IPv6 loopback)");
                let app6 = app.clone();
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(l6, app6).await {
                        tracing::warn!(error = %e, "IPv6 loopback server exited");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not also bind [::1]; localhost-over-IPv6 won't reach this server")
            }
        }
    }

    axum::serve(listener, app).await?;
    Ok(())
}
