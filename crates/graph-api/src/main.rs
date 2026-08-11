use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use data_loader::{HostedImporter, Importer};
use graph_api::{
    compute_broker::{ComputeBroker, RemoteLayout},
    gpu_session::{GpuSessionConfig, GpuSessionHandle},
    importer_catalog::ImporterCatalog,
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
    /// Data source: obsidian (default), tvix, generate, kubernetes, okf, pest,
    /// or github.
    /// Runtime Pest packages are trusted administrator-installed code; the
    /// unauthenticated HTTP API does not accept grammar uploads.
    #[arg(long, env = "JUMP_CANNON_SOURCE", default_value = "obsidian")]
    source: String,
    /// Bounded JSON catalog of deployment-owned importer source instances.
    /// It is exposed read-only at GET /importers; switching still requires a
    /// Helm rollout.
    #[arg(long, env = "JUMP_CANNON_IMPORTER_CATALOG_JSON")]
    importer_catalog_json: Option<String>,
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
    /// GitHub repository slug (`owner/repo`) backing --source=github.
    #[arg(long, env = "JUMP_CANNON_GITHUB_REPO")]
    github_repo: Option<String>,
    /// GitHub branch, tag, or commit SHA to poll.
    #[arg(long, env = "JUMP_CANNON_GITHUB_REF", default_value = "main")]
    github_ref: String,
    /// Subdirectory within the repository holding the vault corpus.
    #[arg(
        long,
        env = "JUMP_CANNON_GITHUB_PATH",
        default_value = "charts/jump-cannon/knowledge"
    )]
    github_path: String,
    /// Bearer token for private repositories. Prefer the env var so the token
    /// stays out of process argument lists; it is never logged.
    #[arg(long, env = "JUMP_CANNON_GITHUB_TOKEN")]
    github_token: Option<String>,
    /// Codeload poll cadence in milliseconds (ETag-revalidated). 0 disables
    /// polling and advertises a static one-shot snapshot.
    #[arg(
        long,
        env = "JUMP_CANNON_GITHUB_POLL_INTERVAL_MS",
        default_value_t = 60000
    )]
    github_poll_interval_ms: u64,
    /// Root of the tarball extraction cache. Defaults to a per-host tempdir.
    #[arg(long, env = "JUMP_CANNON_GITHUB_CACHE_DIR")]
    github_cache_dir: Option<PathBuf>,
    /// Stable ASCII slug used to namespace GitHub node IDs. Defaults to the
    /// sanitized repository slug (e.g. "ocasazza-jump-cannon").
    #[arg(long, env = "JUMP_CANNON_GITHUB_SOURCE_ID")]
    github_source_id: Option<String>,
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
    /// When --source=generate, the deterministic RNG seed for edge topology.
    /// The same flags with the same seed always produce the identical graph.
    #[arg(long = "seed", env = "JUMP_CANNON_GENERATE_SEED", default_value_t = 0)]
    generate_seed: u64,
    /// Path to the chart-rendered RayCluster session template (JSON, mounted
    /// from a ConfigMap). When set (and a kube client is available), the
    /// on-demand GPU session controller runs; unset = the feature is fully
    /// disabled and /compute/session reports {"enabled": false}.
    #[arg(long, env = "JUMP_CANNON_GPU_SESSION_TEMPLATE")]
    gpu_session_template: Option<PathBuf>,
    /// Stable RayCluster name for the session (the chart passes its fullname
    /// so Rust never duplicates Helm naming logic). Required with
    /// --gpu-session-template.
    #[arg(long, env = "JUMP_CANNON_GPU_SESSION_CLUSTER_NAME")]
    gpu_session_cluster_name: Option<String>,
    /// Namespace holding the session RayCluster + Kueue Workloads.
    #[arg(
        long,
        env = "JUMP_CANNON_GPU_SESSION_NAMESPACE",
        default_value = "gpu-workloads"
    )]
    gpu_session_namespace: String,
    /// Idle auto-park timeout in seconds (no layout subscribers).
    #[arg(
        long,
        env = "JUMP_CANNON_GPU_SESSION_IDLE_SECONDS",
        default_value_t = 900
    )]
    gpu_session_idle_seconds: u64,
    /// Kueue admission timeout in seconds before the session fails.
    #[arg(
        long,
        env = "JUMP_CANNON_GPU_SESSION_ADMISSION_TIMEOUT",
        default_value_t = 600
    )]
    gpu_session_admission_timeout_seconds: u64,
    /// Controller-side hard session cap in seconds (ready → parking). Also
    /// stamped as the Kueue max-exec-time label default.
    #[arg(
        long,
        env = "JUMP_CANNON_GPU_SESSION_MAX_SECONDS",
        default_value_t = 14400
    )]
    gpu_session_max_seconds: u64,
    /// Head-start timeout in seconds (admitted → ready) before the session
    /// fails.
    #[arg(
        long,
        env = "JUMP_CANNON_GPU_SESSION_HEAD_START_TIMEOUT",
        default_value_t = 900
    )]
    gpu_session_head_start_timeout_seconds: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let vault_root = args
        .vault_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    // Select the data loader.
    let source_kind = data_loader::SourceKind::parse(&args.source).with_context(|| {
        format!(
            "unknown source {:?}; expected one of {}",
            args.source,
            data_loader::SourceKind::all().join(", ")
        )
    })?;

    let importer_catalog =
        ImporterCatalog::parse(args.importer_catalog_json.as_deref(), source_kind.clone())
            .map_err(anyhow::Error::msg)
            .context("invalid deployment importer catalog")?;

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
                seed = args.generate_seed,
                "using generate loader"
            );
            Box::new(tvix_loader::GenerateLoader::new(
                args.generate_nodes,
                args.generate_edges,
                args.generate_clusters,
                args.generate_cluster_affinity,
                args.generate_seed,
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
        data_loader::SourceKind::GitHub => {
            let repo = args.github_repo.clone().with_context(|| {
                "--source=github requires --github-repo / JUMP_CANNON_GITHUB_REPO"
            })?;
            let source_id = args
                .github_source_id
                .clone()
                .unwrap_or_else(|| github_importer::sanitize_source_id(&repo));
            let cache_dir = args
                .github_cache_dir
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("jump-cannon-github"));
            let config = github_importer::GitHubSourceConfig {
                source_id,
                repo,
                git_ref: args.github_ref.clone(),
                path: args.github_path.clone(),
                token: args.github_token.clone(),
                poll_interval_ms: args.github_poll_interval_ms,
                cache_dir,
                max_bytes: github_importer::DEFAULT_MAX_TARBALL_BYTES,
            };
            // The token is deliberately absent from this log line.
            tracing::info!(
                source_id = %config.source_id,
                repo = %config.repo,
                git_ref = %config.git_ref,
                path = %config.path,
                poll_interval_ms = config.poll_interval_ms,
                "using GitHub importer"
            );
            Box::new(github_importer::GitHubImporter::new(config)?)
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

    // On-demand GPU session controller. Spawned only when the chart mounted
    // a session template AND a kube client is available (in-cluster SA or
    // kubeconfig); every failure path disables the feature with one log line
    // — local `just dev-up` is completely unaffected (R11).
    let gpu_session = build_gpu_session(&args, compute_broker.clone(), progress.clone()).await;

    let state = AppState::new_with_importer_catalog(
        vault_root.clone(),
        importer,
        loaded,
        args.assets_dir,
        compute_broker.clone(),
        progress.clone(),
        importer_catalog,
    )?
    .with_gpu_session(gpu_session);

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

/// Build the GPU session controller from CLI/env config. Returns `None`
/// (feature disabled) when no template is configured or any boot-time
/// prerequisite fails — each branch logs exactly once and the server keeps
/// running (the session feature fails loudly, never the process).
async fn build_gpu_session(
    args: &Args,
    broker: ComputeBroker,
    progress: Arc<ProgressLog>,
) -> Option<GpuSessionHandle> {
    let Some(template_path) = &args.gpu_session_template else {
        tracing::info!(
            "gpu session controller disabled (no --gpu-session-template / \
             JUMP_CANNON_GPU_SESSION_TEMPLATE)"
        );
        return None;
    };
    let Some(cluster_name) = args.gpu_session_cluster_name.clone() else {
        tracing::error!(
            "gpu session disabled: --gpu-session-template is set but \
             --gpu-session-cluster-name / JUMP_CANNON_GPU_SESSION_CLUSTER_NAME is missing"
        );
        return None;
    };
    let template = match graph_api::gpu_session::template::SessionTemplate::load(
        template_path,
        &cluster_name,
        &args.gpu_session_namespace,
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "gpu session disabled: invalid session template");
            return None;
        }
    };
    let client = match kube::Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "gpu session disabled: no kube client (not in cluster, no kubeconfig)");
            return None;
        }
    };
    tracing::info!(
        cluster = %cluster_name,
        namespace = %args.gpu_session_namespace,
        "gpu session controller enabled"
    );
    Some(GpuSessionHandle::spawn(
        GpuSessionConfig {
            cluster_name,
            namespace: args.gpu_session_namespace.clone(),
            template,
            idle_timeout: std::time::Duration::from_secs(args.gpu_session_idle_seconds),
            admission_timeout: std::time::Duration::from_secs(
                args.gpu_session_admission_timeout_seconds,
            ),
            max_session: std::time::Duration::from_secs(args.gpu_session_max_seconds),
            head_start_timeout: std::time::Duration::from_secs(
                args.gpu_session_head_start_timeout_seconds,
            ),
        },
        client,
        broker,
        progress,
    ))
}
