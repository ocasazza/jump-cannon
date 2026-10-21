//! Per-world GPU compute broker (feature `server`, native only).
//!
//! Generalizes graph-api's proven single-tenant GPU session loop
//! ([`graph_api::gpu_session`]) to N worlds: each world gets its own
//! Kueue-admitted compute Job in the shared compute namespace,
//! lifecycle-managed by one [`GpuSessionHandle`] per world (dispatch → wait
//! for Kueue admission → compute pod ready; park = delete the Job and the
//! Service; idle auto-park; admission watchdog; hard caps via the Job's Kueue
//! labels — all inherited from the proven loop).
//!
//! What this module adds on top:
//!
//! - **Per-world naming.** The Job and its compute Service share one
//!   DNS-1123 name: `<release-prefix>-<world-slug>` sanitized and truncated to
//!   63 chars ([`cluster_name`]). Caveat: distinct world slugs that sanitize to
//!   the same DNS-1123 name (e.g. `my.world` vs `my-world`, or long slugs
//!   diverging only past the truncation point) would share a Job name; the
//!   second world's dispatch absorbs the first's `AlreadyExists` and both
//!   point at one Job. Avoid such slugs.
//! - **Per-world compute Service.** The session loop creates and deletes one
//!   ClusterIP Service per world next to the Job (selector
//!   `batch.kubernetes.io/job-name=<name>`, port `compute_port` → targetPort
//!   `grpc`). The world's compute URL is
//!   `http://<cluster-name>.<namespace>.svc:<port>`.
//! - **Activity-driven idle.** graph-api's loop measures idleness by layout
//!   stream subscribers; the broker additionally resets each world's idle clock
//!   on any `/worlds/:name/*` request or VCS mutation (`touch`, wired in the
//!   serving mux).
//!
//! Session state is derived live from cluster objects by the wrapped session
//! handle, never stored. The broker itself is OPTIONAL: no template path → no
//! broker, `WorldHost::compute` keeps returning [`ComputeHandle::Null`] (same
//! philosophy as graph-api's unset `--compute-url`).
//!
//! Known limitation: retiring a world (close) parks its session and forgets
//! the handle, but the wrapped reconcile task has no shutdown signal and keeps
//! ticking (parked, every 30 s) until process exit — one detached task per
//! retired world.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use graph_api::compute_broker::ComputeBroker;
use graph_api::gpu_session::template::SessionTemplate;
use graph_api::gpu_session::{
    Desired, GpuSessionConfig, GpuSessionHandle, SessionState, SessionStatus,
};
use graph_api::progress::ProgressLog;
use kube::Client;

use crate::types::WorldId;

/// Default idle auto-park timeout (seconds) — mirrors graph-api's
/// `JUMP_CANNON_GPU_SESSION_IDLE_SECONDS` default.
pub const DEFAULT_IDLE_SECONDS: u64 = 900;
/// Default Kueue admission timeout (seconds).
pub const DEFAULT_ADMISSION_TIMEOUT_SECONDS: u64 = 600;
/// Default hard session cap (seconds); also the default stamped onto the Job's
/// `kueue.x-k8s.io/max-exec-time-seconds` label when the chart left it unset.
pub const DEFAULT_MAX_SECONDS: u64 = 14400;
/// Default start timeout (seconds): admitted → compute pod ready.
pub const DEFAULT_START_TIMEOUT_SECONDS: u64 = 900;
/// Default release prefix for per-world Job names.
pub const DEFAULT_RELEASE_PREFIX: &str = "jump-cannon";
/// Default graph-compute gRPC port (the chart's `graphCompute.service.port`).
pub const DEFAULT_COMPUTE_PORT: u16 = 50051;

/// Static broker configuration (from the server's CLI/env).
pub struct GpuBrokerConfig {
    /// Release prefix for Job names (`JUMP_CANNON_SM_RELEASE`).
    pub release_prefix: String,
    /// Namespace holding the per-world Jobs, Workloads, and Services.
    pub namespace: String,
    /// Validated Job manifest with a placeholder `metadata.name`, stamped per
    /// world at dispatch time.
    pub template: SessionTemplate,
    pub idle_timeout: Duration,
    pub admission_timeout: Duration,
    pub max_session: Duration,
    pub start_timeout: Duration,
    /// graph-compute gRPC port exposed by the per-world Service.
    pub compute_port: u16,
}

struct Inner {
    config: GpuBrokerConfig,
    client: Client,
    /// Live per-world session controllers, created lazily on first
    /// dispatch/status. Keyed by world id.
    worlds: Mutex<HashMap<WorldId, GpuSessionHandle>>,
}

/// Cheap-clone handle to the per-world GPU broker.
#[derive(Clone)]
pub struct GpuBroker {
    inner: Arc<Inner>,
}

impl GpuBroker {
    /// Build the broker: connect the kube client (in-cluster SA or
    /// kubeconfig). Callers disable the feature on `Err` (logged once), the
    /// same stance as graph-api's `build_gpu_session`.
    pub async fn new(config: GpuBrokerConfig) -> anyhow::Result<Self> {
        let client = Client::try_default()
            .await
            .context("kube client for the gpu broker (not in cluster, no kubeconfig)")?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                client,
                worlds: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// The Job/Service name for a world.
    pub fn cluster_name(&self, world: &WorldId) -> String {
        cluster_name(&self.inner.config.release_prefix, &world.0)
    }

    /// The in-cluster gRPC URL of the world's compute Service. Stable whether
    /// or not the session is currently dispatched (the Service exists only
    /// while the session does).
    pub fn compute_url(&self, world: &WorldId) -> String {
        format!(
            "http://{}.{}.svc:{}",
            self.cluster_name(world),
            self.inner.config.namespace,
            self.inner.config.compute_port
        )
    }

    /// Get-or-create the world's session controller. The wrapped loop's
    /// first tick adopts a pre-existing Job (desired=Running), so a broker
    /// restart never strands a running session.
    fn handle_for(
        &self,
        world: &WorldId,
        broker: &ComputeBroker,
        progress: &Arc<ProgressLog>,
    ) -> GpuSessionHandle {
        let mut worlds = self.inner.worlds.lock().expect("gpu broker lock");
        if let Some(handle) = worlds.get(world) {
            return handle.clone();
        }
        let handle = GpuSessionHandle::spawn(
            GpuSessionConfig {
                cluster_name: self.cluster_name(world),
                namespace: self.inner.config.namespace.clone(),
                template: self.inner.config.template.clone(),
                compute_port: self.inner.config.compute_port,
                idle_timeout: self.inner.config.idle_timeout,
                admission_timeout: self.inner.config.admission_timeout,
                max_session: self.inner.config.max_session,
                start_timeout: self.inner.config.start_timeout,
            },
            self.inner.client.clone(),
            broker.clone(),
            progress.clone(),
        );
        worlds.insert(world.clone(), handle.clone());
        handle
    }

    /// Reset the world's idle clock. Called from the serving mux on any
    /// `/worlds/:name/*` request and from the VCS mutation paths. No-op for
    /// worlds with no controller yet — read-only traffic on a never-dispatched
    /// world must not spawn one.
    pub fn touch(&self, world: &WorldId) {
        let worlds = self.inner.worlds.lock().expect("gpu broker lock");
        if let Some(handle) = worlds.get(world) {
            handle.touch_activity();
        }
    }

    /// Dispatch the world's GPU session: ask the session loop to converge to
    /// Running (it creates the Service and the Kueue-admitted Job), and
    /// connect the world's compute broker to the session URL (its internal
    /// dial loop retries until the compute pod is up, so connecting before
    /// admission is fine).
    pub async fn dispatch(
        &self,
        world: &WorldId,
        broker: &ComputeBroker,
        progress: &Arc<ProgressLog>,
    ) -> SessionState {
        let handle = self.handle_for(world, broker, progress);
        let state = handle.set_desired(Desired::Running);
        let url = self.compute_url(world);
        if let Err(e) = broker.connect(url.clone()).await {
            // Not fatal: the session controller keeps converging and the next
            // dispatch retries the connect.
            tracing::warn!(url = %url, error = %format!("{e:#}"), "world compute broker connect failed");
        }
        state
    }

    /// Park the world's GPU session: desired=Parked (the loop deletes the
    /// Service and the Job). Idempotent; a world that was never dispatched
    /// reports Parked.
    pub fn park(&self, world: &WorldId) -> SessionState {
        let handle = self
            .inner
            .worlds
            .lock()
            .expect("gpu broker lock")
            .get(world)
            .cloned();
        match handle {
            Some(handle) => handle.set_desired(Desired::Parked),
            None => SessionState::Parked,
        }
    }

    /// Live session status, derived from cluster objects by the wrapped
    /// session loop. Spawns the world's controller on first call so a
    /// pre-existing Job is adopted rather than reported as absent.
    pub async fn status(
        &self,
        world: &WorldId,
        broker: &ComputeBroker,
        progress: &Arc<ProgressLog>,
    ) -> SessionStatus {
        self.handle_for(world, broker, progress).status().await
    }

    /// Park and forget a world's session (world closed). The Service and Job
    /// deletes converge asynchronously inside the wrapped loop.
    pub fn retire(&self, world: &WorldId) {
        let handle = self
            .inner
            .worlds
            .lock()
            .expect("gpu broker lock")
            .remove(world);
        if let Some(handle) = handle {
            handle.set_desired(Desired::Parked);
        }
    }
}

/// Sanitize to a DNS-1123 subdomain segment: lowercase alphanumerics and
/// single dashes, alphanumeric start/end, truncated to 63 bytes. An input
/// with no alphanumeric characters at all yields the empty string (callers
/// substitute a hash).
pub fn dns1123(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(63));
    let mut pending_dash = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() && out.len() < 63 {
                out.push('-');
            }
            pending_dash = false;
            if out.len() < 63 {
                out.push(c.to_ascii_lowercase());
            }
        } else {
            pending_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// FNV-1a hex of the raw slug — the fallback for slugs with no DNS-1123
/// content (e.g. a world literally named `...`).
fn slug_hash(slug: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in slug.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("w-{hash:016x}")
}

/// The per-world Job/Service name: `<release-prefix>-<world-slug>` as a
/// DNS-1123 subdomain (≤63 chars).
pub fn cluster_name(release_prefix: &str, world_slug: &str) -> String {
    let prefix = dns1123(release_prefix);
    let slug = match dns1123(world_slug) {
        empty if empty.is_empty() => slug_hash(world_slug),
        slug => slug,
    };
    let prefix = match prefix.is_empty() {
        true => DEFAULT_RELEASE_PREFIX.to_string(),
        false => prefix,
    };
    dns1123(&format!("{prefix}-{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns1123_lowercases_and_dashes() {
        assert_eq!(dns1123("My.World_Name"), "my-world-name");
        assert_eq!(dns1123("already-fine-1"), "already-fine-1");
        assert_eq!(dns1123("--lead.trail--"), "lead-trail");
        assert_eq!(dns1123("a..b__c--d"), "a-b-c-d");
    }

    #[test]
    fn dns1123_truncates_to_63_trimming_dash() {
        let long = "a".repeat(80);
        assert_eq!(dns1123(&long).len(), 63);
        // Truncation must not leave a trailing dash.
        let with_dash = format!("{}-{}", "b".repeat(62), "c".repeat(10));
        let out = dns1123(&with_dash);
        assert!(out.len() <= 63);
        assert!(!out.ends_with('-'));
    }

    #[test]
    fn dns1123_empties_non_alnum_input() {
        assert_eq!(dns1123("...___---"), "");
    }

    #[test]
    fn cluster_name_prefixes_and_truncates() {
        assert_eq!(cluster_name("jump-cannon", "my-world"), "jump-cannon-my-world");
        // WorldId slugs may carry `.`/`_`; they become dashes.
        assert_eq!(cluster_name("jump-cannon", "my.world_2"), "jump-cannon-my-world-2");
        // Overlong names truncate to 63.
        let name = cluster_name("jump-cannon", &"x".repeat(100));
        assert_eq!(name.len(), 63);
        assert!(name.starts_with("jump-cannon-"));
        // A slug with no DNS-1123 content falls back to a stable hash.
        let hashed = cluster_name("jump-cannon", "...");
        assert!(hashed.starts_with("jump-cannon-w-"));
        assert_eq!(hashed, cluster_name("jump-cannon", "..."));
        assert_ne!(hashed, cluster_name("jump-cannon", "___"));
    }

    #[test]
    fn template_placeholder_stamps_per_world_name() {
        // The chart-mounted world template carries a placeholder name; the
        // session loop stamps the per-world Job name at render time.
        let dir = std::env::temp_dir().join(format!("gpu-broker-template-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("job.yaml");
        std::fs::write(
            &path,
            "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: __world__\n  namespace: gpu-workloads\n  labels:\n    kueue.x-k8s.io/queue-name: gpu\n    kueue.x-k8s.io/max-exec-time-seconds: \"14400\"\nspec:\n  suspend: true\n",
        )
        .expect("write");
        let template = SessionTemplate::load_templated(&path, "gpu-workloads").expect("loads");
        for slug in ["alpha", "beta.world"] {
            let name = cluster_name("jump-cannon", slug);
            let stamped = template.render(&name, "gpu-workloads", 14400);
            assert_eq!(
                stamped.pointer("/metadata/name").and_then(serde_json::Value::as_str),
                Some(name.as_str())
            );
            assert_eq!(
                stamped.pointer("/spec/suspend").and_then(serde_json::Value::as_bool),
                Some(true)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
