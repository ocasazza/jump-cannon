use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use data_loader::{HostedImporter, Importer};
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
    /// Don't auto-open the browser. Override with $GRAPH_API_NO_BROWSER=true.
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
    /// Periodic full-rescan fallback for filesystem importers, in seconds.
    /// This catches writes whose filesystem notifications do not propagate
    /// across another pod or mount. Set to 0 to disable the fallback.
    #[arg(
        long,
        env = "JUMP_CANNON_FILESYSTEM_RESCAN_SECONDS",
        default_value_t = 0
    )]
    filesystem_rescan_seconds: u64,
    /// Data source: obsidian (default), tvix, generate, kubernetes, okf, or pest.
    /// Runtime Pest packages are trusted administrator-installed code; the
    /// unauthenticated HTTP API does not accept grammar uploads.
    #[arg(long, env = "JUMP_CANNON_SOURCE", default_value = "obsidian")]
    source: String,
    /// When --source=tvix, the Nix expression to evaluate. If not provided,
    /// reads from the file at --vault-root (which must be a .nix file).
    #[arg(long, env = "JUMP_CANNON_TVIX_EXPR")]
    tvix_expr: Option<String>,
    /// JSON source-instance configuration used by --source=kubernetes.
    /// Credentials are resolved by kube-rs from kubeconfig or the pod's
    /// explicitly mounted service-account projection, never from this file.
    #[arg(long, env = "JUMP_CANNON_KUBERNETES_CONFIG")]
    kubernetes_config: Option<PathBuf>,
    /// Stable ASCII slug used to namespace OKF node IDs.
    #[arg(long, env = "JUMP_CANNON_OKF_SOURCE_ID", default_value = "default")]
    okf_source_id: String,
    /// Versioned TOML package containing a runtime Pest grammar and capture map.
    /// Required by --source=pest.
    #[arg(long, env = "JUMP_CANNON_IMPORTER_MANIFEST")]
    importer_manifest: Option<PathBuf>,
    /// Filesystem input bound to --importer-manifest. Required by --source=pest.
    #[arg(long, env = "JUMP_CANNON_IMPORTER_INPUT")]
    importer_input: Option<PathBuf>,
    /// When --source=generate, the number of nodes to create.
    #[arg(long, env = "JUMP_CANNON_GENERATE_NODES", default_value_t = 1000)]
    generate_nodes: usize,
    /// When --source=generate, the number of edges to create.
    #[arg(long, env = "JUMP_CANNON_GENERATE_EDGES", default_value_t = 2000)]
    generate_edges: usize,
    /// When --source=generate, partition nodes into this many clusters.
    /// Nodes within the same cluster connect more often (see --cluster-affinity).
    /// Default 0 = no clustering (purely random edges).
    #[arg(long, env = "JUMP_CANNON_GENERATE_CLUSTERS", default_value_t = 0)]
    generate_clusters: usize,
    /// When --source=generate with --clusters > 0, the probability (0.0–1.0)
    /// that an edge connects nodes within the same cluster. Default 0.8.
    #[arg(
        long,
        env = "JUMP_CANNON_GENERATE_CLUSTER_AFFINITY",
        default_value_t = 0.8
    )]
    generate_cluster_affinity: f64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let source_kind = data_loader::SourceKind::parse(&args.source).with_context(|| {
        format!(
            "unknown source {:?}; expected one of {}",
            args.source,
            data_loader::SourceKind::all().join(", ")
        )
    })?;

    let importer: Box<dyn Importer> = match source_kind {
        data_loader::SourceKind::Obsidian => {
            tracing::info!(vault_root = %vault_root.display(), "using obsidian loader");
            Box::new(vault_links::ObsidianLoader::new(vault_root.clone()))
        }
        data_loader::SourceKind::Tvix => {
            let expr = if let Some(ref e) = args.tvix_expr {
                e.clone()
            } else if vault_root.extension().is_some_and(|ext| ext == "nix") {
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
        data_loader::SourceKind::Generate => {
            tracing::info!(
                nodes = args.generate_nodes,
                edges = args.generate_edges,
                clusters = args.generate_clusters,
                affinity = args.generate_cluster_affinity,
                "using generate loader"
            );
            Box::new(tvix_loader::GenerateLoader::new(
                args.generate_nodes,
                args.generate_edges,
                args.generate_clusters,
                args.generate_cluster_affinity,
            ))
        }
        data_loader::SourceKind::Kubernetes => {
            let path = args.kubernetes_config.as_ref().with_context(|| {
                "--source=kubernetes requires --kubernetes-config / JUMP_CANNON_KUBERNETES_CONFIG"
            })?;
            let raw = std::fs::read_to_string(path).with_context(|| {
                format!("failed to read Kubernetes source config {}", path.display())
            })?;
            let config: kubernetes_importer::KubernetesSourceConfig = serde_json::from_str(&raw)
                .with_context(|| format!("invalid Kubernetes source config {}", path.display()))?;
            tracing::info!(
                source_id = %config.source_id,
                resources = config.resources.len(),
                "using Kubernetes importer"
            );
            Box::new(kubernetes_importer::build_importer(config)?)
        }
        data_loader::SourceKind::Okf => {
            tracing::info!(
                source_id = %args.okf_source_id,
                root = %vault_root.display(),
                "using Open Knowledge Format importer"
            );
            Box::new(okf_importer::OkfImporter::new(
                vault_root.clone(),
                args.okf_source_id.clone(),
            )?)
        }
        data_loader::SourceKind::Pest => {
            let manifest_path = args.importer_manifest.as_ref().with_context(|| {
                "--source=pest requires --importer-manifest / JUMP_CANNON_IMPORTER_MANIFEST"
            })?;
            let input_path = args.importer_input.as_ref().with_context(|| {
                "--source=pest requires --importer-input / JUMP_CANNON_IMPORTER_INPUT"
            })?;
            let manifest_len = std::fs::metadata(manifest_path)
                .with_context(|| {
                    format!(
                        "failed to inspect importer package {}",
                        manifest_path.display()
                    )
                })?
                .len() as usize;
            anyhow::ensure!(
                manifest_len <= pest_importer::HARD_LIMITS.manifest_bytes,
                "importer package {} is {} bytes; hard limit is {} bytes",
                manifest_path.display(),
                manifest_len,
                pest_importer::HARD_LIMITS.manifest_bytes
            );
            let raw = std::fs::read(manifest_path).with_context(|| {
                format!(
                    "failed to read importer package {}",
                    manifest_path.display()
                )
            })?;
            let package = pest_importer::ValidatedPackage::from_toml_bytes(&raw)
                .with_context(|| format!("invalid importer package {}", manifest_path.display()))?;
            tracing::info!(
                importer = %package.manifest().metadata.id,
                version = %package.manifest().metadata.version,
                input = %input_path.display(),
                "using trusted runtime Pest importer"
            );
            Box::new(pest_importer::FilesystemImporter::new(
                package,
                input_path.clone(),
            ))
        }
    };

    // This CLI is the trusted host configuration surface: every selectable
    // source is either compiled into graph-api (including OKF) or an explicitly
    // bound, administrator-installed native Pest package. Descriptor requests do not
    // grant themselves; the host records the exact tuples here. A future
    // untrusted upload/control-plane path must select a reviewed subset instead
    // of applying this trusted-source policy.
    let host_grants = importer.descriptor().capabilities;
    let importer = HostedImporter::new(importer, host_grants)?;

    // Shared progress log. Surfaces "Scanning vault / Computing metrics /
    // Seeding positions / Rebuilding search index" task bars to the
    // frontend footer via GET /progress.
    let progress = Arc::new(ProgressLog::new());

    // Initial graph load — emit progress events so the bootstrap fetch
    // sees a populated /progress response on the first poll.
    let loaded = vault_loader::load_with_progress(&importer, Some(&progress)).await?;

    if let Some(dir) = &args.assets_dir {
        tracing::info!(assets_dir = %dir.display(), "dev mode: serving assets from disk");
    }

    let compute_broker = ComputeBroker::new();

    let state = AppState::new(
        vault_root.clone(),
        importer,
        loaded,
        args.assets_dir,
        compute_broker.clone(),
        progress.clone(),
    )?;

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

    // Live reload follows the active importer's watch plan (filesystem,
    // polling, push, or static) rather than assuming an Obsidian directory.
    if !args.no_watch {
        graph_api::watcher::spawn(state.clone(), args.filesystem_rescan_seconds);
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
