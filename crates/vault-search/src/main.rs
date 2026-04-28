use anyhow::{Context, Result};
use clap::Parser;
use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

mod error;
mod index;
mod server;
mod walker;

use crate::index::{build_query_parser, open_or_create, IndexState, IndexStatus};
use crate::server::{router, run_startup_index};

#[derive(Parser, Debug)]
#[command(
    name = "vault-search",
    about = "HTTP full-text search backend for an Obsidian vault (Tantivy)",
    version
)]
struct Args {
    /// Path to the Obsidian vault root.
    #[arg(long, env = "OBSIDIAN_VAULT", default_value = "./vault")]
    vault: PathBuf,

    /// HTTP port (0 = auto-pick free port; the chosen port is printed on stderr).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Bind host.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Index location. Defaults to ~/.cache/vault-search/<vault-hash>/.
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Force a clean reindex even if a cache exists.
    #[arg(long)]
    rebuild: bool,

    /// Include vault/30-Knowledge-Base/_hippo/** in the index.
    #[arg(long)]
    include_hippo: bool,

    /// Watch vault for file changes and incrementally reindex (NOT YET IMPLEMENTED).
    #[arg(long)]
    watch: bool,

    /// Log level (trace|debug|info|warn|error).
    #[arg(long, default_value = "info")]
    log: String,
}

fn init_logging(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn vault_hash(path: &std::path::Path) -> String {
    let mut h = Sha256::new();
    h.update(path.to_string_lossy().as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..8])
}

fn resolve_cache_dir(opt: Option<PathBuf>, vault: &std::path::Path) -> Result<PathBuf> {
    if let Some(d) = opt {
        return Ok(d);
    }
    let base = dirs::cache_dir()
        .context("could not determine ~/.cache directory")?
        .join("vault-search");
    Ok(base.join(vault_hash(vault)))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args.log);

    let vault = std::fs::canonicalize(&args.vault)
        .with_context(|| format!("vault path: {}", args.vault.display()))?;
    if !vault.is_dir() {
        anyhow::bail!("vault is not a directory: {}", vault.display());
    }
    let cache_dir = resolve_cache_dir(args.cache_dir.clone(), &vault)?;

    if args.watch {
        tracing::warn!("--watch is not yet implemented; running in one-shot mode");
    }

    tracing::info!(?vault, ?cache_dir, include_hippo = args.include_hippo, "starting");

    let (idx, fields) = open_or_create(&cache_dir).context("open/create index")?;
    let writer = idx
        .writer(64 * 1024 * 1024)
        .context("acquire index writer")?;
    let reader = idx
        .reader_builder()
        .reload_policy(tantivy::ReloadPolicy::OnCommitWithDelay)
        .try_into()
        .context("build reader")?;

    let qp = build_query_parser(&idx, &fields);
    let state = IndexState {
        vault: vault.clone(),
        include_hippo: args.include_hippo,
        index: idx.clone(),
        reader,
        fields,
        writer: Arc::new(RwLock::new(writer)),
        query_parser: Arc::new(qp),
        indexed: Arc::new(AtomicUsize::new(0)),
        total: Arc::new(AtomicUsize::new(0)),
        status: Arc::new(RwLock::new(IndexStatus::Indexing)),
    };

    // Index synchronously before binding so /health reports accurate counts.
    let state_clone = state.clone();
    let rebuild = args.rebuild;
    tokio::task::spawn_blocking(move || run_startup_index(&state_clone, rebuild))
        .await
        .context("startup index join")?
        .context("startup index")?;

    let addr: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("bad bind addr: {}:{}", args.host, args.port))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let bound = listener
        .local_addr()
        .context("local_addr after bind")?;
    // The single line parent processes scrape for the chosen port.
    eprintln!("vault-search: listening on http://{bound}");

    let app = router(state);

    let shutdown = async {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        #[cfg(unix)]
        let term = async {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut sig) = signal(SignalKind::terminate()) {
                sig.recv().await;
            }
        };
        #[cfg(not(unix))]
        let term = std::future::pending::<()>();
        tokio::select! {
            _ = ctrl_c => {},
            _ = term => {},
        }
        tracing::info!("shutdown signal received");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("axum serve")?;

    Ok(())
}
