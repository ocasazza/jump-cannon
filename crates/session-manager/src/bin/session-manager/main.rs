//! The multi-user session-manager server binary (feature `server`).
//!
//! Serves the world/session/VCS REST API (`/api/*`) plus per-world graph
//! serving (`/worlds/:name/*`) backed by the [`KubernetesSessionManager`].
//! Authentication is by trusted identity header — run behind an
//! authenticating ingress that sets `--user-header`.
//!
//! When a per-world RayCluster template is mounted (`--gpu-template`), the
//! GPU broker manages one Kueue-admitted RayCluster per world; without it
//! the broker is disabled and `compute` reports `ComputeHandle::Null`.

use clap::Parser;
use session_manager::gpu_broker::{self, GpuBroker, GpuBrokerConfig};
use session_manager::kubernetes::KubernetesSessionManager;
use session_manager::server::{router, ServerConfig};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "session-manager")]
struct Args {
    /// Listen port. Override with $SESSION_MANAGER_PORT.
    #[arg(short, long, env = "SESSION_MANAGER_PORT", default_value_t = 8080)]
    port: u16,
    /// Listen host. Override with $SESSION_MANAGER_HOST.
    #[arg(long, env = "SESSION_MANAGER_HOST", default_value = "127.0.0.1")]
    host: String,
    /// Directory holding one `<world-slug>.graph` file per world plus the
    /// `worlds.json` metadata/ACL manifest.
    #[arg(long, env = "JUMP_CANNON_WORLDS_DIR", default_value = "./worlds")]
    worlds_dir: PathBuf,
    /// World store backend: `minigraf` (one file per world, default) or
    /// `terminusdb` (one TerminusDB database per world; needs
    /// TERMINUSDB_URL/TERMINUSDB_PASSWORD — see graph-vcs TerminusConfig).
    #[arg(long, env = "JUMP_CANNON_SM_STORE", default_value = "minigraf")]
    store: String,
    /// Header carrying the authenticated user identity (set by the
    /// authenticating ingress). Requests without it get 401.
    #[arg(long, env = "JUMP_CANNON_USER_HEADER", default_value = "x-user")]
    user_header: String,
    /// Path to the chart-rendered per-world RayCluster template (YAML/JSON,
    /// mounted from a ConfigMap; `metadata.name` is a placeholder stamped
    /// per world). When set (and a kube client is available), the per-world
    /// GPU broker runs; unset = the broker is fully disabled and compute
    /// session endpoints report `{"enabled": false}`.
    #[arg(long, env = "JUMP_CANNON_SM_GPU_TEMPLATE")]
    gpu_template: Option<PathBuf>,
    /// Namespace holding the per-world RayClusters, Kueue Workloads, and
    /// compute Services.
    #[arg(
        long,
        env = "JUMP_CANNON_SM_COMPUTE_NAMESPACE",
        default_value = "gpu-workloads"
    )]
    gpu_namespace: String,
    /// Release prefix for per-world cluster names (`<prefix>-<world-slug>`).
    #[arg(
        long,
        env = "JUMP_CANNON_SM_RELEASE",
        default_value = gpu_broker::DEFAULT_RELEASE_PREFIX
    )]
    gpu_release: String,
    /// graph-compute gRPC port exposed by each world's compute Service.
    #[arg(
        long,
        env = "JUMP_CANNON_SM_COMPUTE_PORT",
        default_value_t = gpu_broker::DEFAULT_COMPUTE_PORT
    )]
    gpu_compute_port: u16,
    /// Idle auto-park timeout in seconds (no world activity).
    #[arg(
        long,
        env = "JUMP_CANNON_SM_IDLE_SECONDS",
        default_value_t = gpu_broker::DEFAULT_IDLE_SECONDS
    )]
    gpu_idle_seconds: u64,
    /// Kueue admission timeout in seconds before a world session fails.
    #[arg(
        long,
        env = "JUMP_CANNON_SM_ADMISSION_TIMEOUT",
        default_value_t = gpu_broker::DEFAULT_ADMISSION_TIMEOUT_SECONDS
    )]
    gpu_admission_timeout_seconds: u64,
    /// Hard per-world session cap in seconds. Also the fallback for the
    /// Kueue max-exec-time label when the chart left it unset.
    #[arg(
        long,
        env = "JUMP_CANNON_SM_MAX_SECONDS",
        default_value_t = gpu_broker::DEFAULT_MAX_SECONDS
    )]
    gpu_max_seconds: u64,
    /// Head-start timeout in seconds (admitted → ready) before a world
    /// session fails.
    #[arg(
        long,
        env = "JUMP_CANNON_SM_HEAD_START_TIMEOUT",
        default_value_t = gpu_broker::DEFAULT_HEAD_START_TIMEOUT_SECONDS
    )]
    gpu_head_start_timeout_seconds: u64,
}

/// Build the per-world GPU broker from CLI/env config. Returns `None`
/// (broker disabled) when no template is configured or any boot-time
/// prerequisite fails — each branch logs exactly once and the server keeps
/// running (mirrors graph-api's `build_gpu_session` stance).
async fn build_gpu_broker(args: &Args) -> Option<GpuBroker> {
    let Some(template_path) = &args.gpu_template else {
        tracing::info!(
            "gpu broker disabled (no --gpu-template / JUMP_CANNON_SM_GPU_TEMPLATE)"
        );
        return None;
    };
    let template = match graph_api::gpu_session::template::SessionTemplate::load_templated(
        template_path,
        &args.gpu_namespace,
    ) {
        Ok(template) => template,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "gpu broker disabled: invalid world template");
            return None;
        }
    };
    let broker = match GpuBroker::new(GpuBrokerConfig {
        release_prefix: args.gpu_release.clone(),
        namespace: args.gpu_namespace.clone(),
        template,
        idle_timeout: std::time::Duration::from_secs(args.gpu_idle_seconds),
        admission_timeout: std::time::Duration::from_secs(args.gpu_admission_timeout_seconds),
        max_session: std::time::Duration::from_secs(args.gpu_max_seconds),
        head_start_timeout: std::time::Duration::from_secs(args.gpu_head_start_timeout_seconds),
        compute_port: args.gpu_compute_port,
    })
    .await
    {
        Ok(broker) => broker,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "gpu broker disabled: no kube client");
            return None;
        }
    };
    tracing::info!(
        release = %args.gpu_release,
        namespace = %args.gpu_namespace,
        "per-world gpu broker enabled"
    );
    Some(broker)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let backend = match args.store.as_str() {
        "minigraf" => session_manager::kubernetes::StoreBackend::Minigraf,
        "terminusdb" => {
            let config = graph_vcs::TerminusConfig::from_env()?;
            tracing::info!(
                url = %config.base_url,
                org = %config.org,
                "terminusdb world store selected"
            );
            session_manager::kubernetes::StoreBackend::Terminusdb(config)
        }
        other => anyhow::bail!("invalid --store {other:?}: expected minigraf or terminusdb"),
    };
    let manager = Arc::new(KubernetesSessionManager::open_with_backend(
        &args.worlds_dir,
        backend,
    )?);
    tracing::info!(worlds_dir = %args.worlds_dir.display(), "adopted world files");
    manager.set_gpu_broker(build_gpu_broker(&args).await);

    let app = router(
        manager,
        ServerConfig {
            user_header: args.user_header,
            ..ServerConfig::default()
        },
    );

    let host: std::net::IpAddr = args.host.parse().unwrap_or_else(|_| {
        tracing::warn!(host = %args.host, "invalid --host, defaulting to 127.0.0.1");
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
    });
    let listener = tokio::net::TcpListener::bind((host, args.port)).await?;
    let bound = listener.local_addr()?;
    tracing::info!(url = %format!("http://{bound}/"), "listening");
    println!("http://{bound}/");

    axum::serve(listener, app).await?;
    Ok(())
}
