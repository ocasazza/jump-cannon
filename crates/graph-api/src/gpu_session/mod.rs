//! On-demand GPU compute session controller.
//!
//! Owns the lifecycle of the session RayCluster CR in the compute namespace:
//! dispatch when a user picks a cluster engine (or explicitly), live state
//! derivation from cluster objects + broker status (Q3, see [`observe`]),
//! idle auto-park (Q5), a controller-side hard session cap, and head-pod log
//! surfacing into the `gpu-session` progress group (Q7, see [`logs`]).
//!
//! Kueue/KubeRay ground truth honoured here:
//! - `spec.suspend` is NEVER touched — Kueue owns it while admitted.
//! - Park = DELETE the RayCluster CR (pods GC via owner refs, quota freed).
//! - Dispatch = POST a fresh CR stamped from the chart-rendered template
//!   (see [`template`]); `AlreadyExists` while the old CR finalizes is
//!   absorbed by the `Parking`/`Dispatching` states.
//!
//! All kube access goes through the dynamic API (`serde_json::Value`) for
//! RayCluster/Workload and typed reads for core pods/pods/log (R12). Every
//! kube call is timeout-bound; nothing in the handler or controller paths
//! panics.

pub mod logs;
pub mod observe;
pub mod template;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, PostParams};
use kube::Client;
use serde::{Deserialize, Serialize};

use crate::compute_broker::{BrokerStatus, ComputeBroker};
use crate::progress::{ProgressLog, TaskId};

use logs::LogTailer;
use observe::{BrokerFacts, DeriveInput, HeadPodFacts, RayClusterFacts, WorkloadFacts};
use template::SessionTemplate;

/// Reconcile tick while the session is anything but parked (Q3 ~2s).
const ACTIVE_TICK: Duration = Duration::from_secs(2);
/// Reconcile tick while parked (Q5 default reconcile cadence).
const PARKED_TICK: Duration = Duration::from_secs(30);
/// Bound on every kube API call.
const KUBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Log-tail cadence while a head pod exists (Q7).
const LOG_POLL: Duration = Duration::from_secs(5);
/// How long a crash-looping CR is kept so the tailer can capture logs before
/// the auto-park delete (Q8).
const FAILURE_LOG_GRACE: Duration = Duration::from_secs(60);
/// Parking longer than this emits periodic warn events; finalizers are NEVER
/// force-removed (Q8).
const PARKING_WARN_AFTER: Duration = Duration::from_secs(5 * 60);
const PARKING_WARN_EVERY: Duration = Duration::from_secs(60);

/// Observed session state (Q3). Serialized snake_case on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Parked,
    Dispatching,
    Queued,
    Admitted,
    HeadStarting,
    Ready,
    Parking,
    Failed,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Parked => "parked",
            SessionState::Dispatching => "dispatching",
            SessionState::Queued => "queued",
            SessionState::Admitted => "admitted",
            SessionState::HeadStarting => "head_starting",
            SessionState::Ready => "ready",
            SessionState::Parking => "parking",
            SessionState::Failed => "failed",
        }
    }

    /// Progress-task label shown while in this state.
    fn progress_label(self, detail: Option<&str>) -> String {
        let base = match self {
            SessionState::Parked => "GPU session parked",
            SessionState::Dispatching => "Dispatch GPU session",
            SessionState::Queued => "GPU session queued",
            SessionState::Admitted => "GPU session admitted; starting head pod",
            SessionState::HeadStarting => "GPU head running; waiting for worker",
            SessionState::Ready => "GPU session ready",
            SessionState::Parking => "Parking GPU session",
            SessionState::Failed => "GPU session failed",
        };
        match detail {
            Some(d) if !d.is_empty() => format!("{base}: {d}"),
            _ => base.to_string(),
        }
    }
}

/// In-memory desired state (Q3: default Parked, never persisted).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Desired {
    Running,
    Parked,
}

/// Static configuration for the session controller (from clap flags / env).
pub struct GpuSessionConfig {
    pub cluster_name: String,
    pub namespace: String,
    pub template: SessionTemplate,
    pub idle_timeout: Duration,
    pub admission_timeout: Duration,
    pub max_session: Duration,
    pub head_start_timeout: Duration,
}

/// `GET /compute/session` payload when the feature is enabled (exact wire
/// contract). The disabled form (`{"enabled": false}`) is produced inline by
/// the handler.
#[derive(Clone, Debug, Serialize)]
pub struct SessionStatus {
    pub enabled: bool,
    pub state: SessionState,
    pub desired: Desired,
    pub cluster_name: String,
    pub state_since_ms: u64,
    /// Seconds until the idle auto-park fires; `null` while parked.
    pub idle_seconds_remaining: Option<u64>,
    pub detail: Option<String>,
    pub failure_reason: Option<String>,
    /// The existing `BrokerStatus` serialized shape, reused as-is.
    pub broker: BrokerStatus,
}

/// Mutable controller state. Everything here is cheap to read/write; the
/// reconcile loop is the only writer of the observed fields.
struct Shared {
    desired: Desired,
    state: SessionState,
    state_since: Instant,
    state_since_ms: u64,
    detail: Option<String>,
    /// Latched failure for this episode; surfaced as `failure_reason` (also
    /// after a later park, per Q8 eviction case). Cleared by a new dispatch.
    failure_reason: Option<String>,
    last_activity: Instant,
    /// When the current running episode started (hard-cap clock).
    episode_started: Option<Instant>,
    /// When the failure latched (crashloop log-grace clock).
    failed_at: Option<Instant>,
    /// Delete already issued for the current CR (awaiting NotFound).
    delete_issued: bool,
    /// We have observed a CR in this episode — distinguishes "never
    /// created" (dispatch should create) from "deleted under us" (Q8:
    /// external delete → surface + park, not silent redispatch).
    had_cluster: bool,
    parking_since: Option<Instant>,
    last_parking_warn: Option<Instant>,
    /// Open progress task for the current lifecycle, if any.
    task: Option<TaskId>,
}

impl Shared {
    fn new() -> Self {
        Self {
            desired: Desired::Parked,
            state: SessionState::Parked,
            state_since: Instant::now(),
            state_since_ms: unix_ms(),
            detail: None,
            failure_reason: None,
            last_activity: Instant::now(),
            episode_started: None,
            failed_at: None,
            delete_issued: false,
            had_cluster: false,
            parking_since: None,
            last_parking_warn: None,
            task: None,
        }
    }
}

struct Controller {
    config: GpuSessionConfig,
    client: Client,
    broker: ComputeBroker,
    progress: Arc<ProgressLog>,
    shared: Mutex<Shared>,
    /// Wakes the reconcile loop immediately on a desired-state change.
    wake: tokio::sync::Notify,
}

/// Cheap-clone handle stored in `AppStateInner`.
#[derive(Clone)]
pub struct GpuSessionHandle {
    inner: Arc<Controller>,
}

impl GpuSessionHandle {
    /// Build the controller and spawn its reconcile loop. Boot-adoption (Q8:
    /// a pre-existing CR is adopted with desired=Running) happens on the
    /// loop's first tick.
    pub fn spawn(
        config: GpuSessionConfig,
        client: Client,
        broker: ComputeBroker,
        progress: Arc<ProgressLog>,
    ) -> Self {
        let inner = Arc::new(Controller {
            config,
            client,
            broker,
            progress,
            shared: Mutex::new(Shared::new()),
            wake: tokio::sync::Notify::new(),
        });
        let controller = inner.clone();
        tokio::spawn(async move { controller.run().await });
        Self { inner }
    }

    /// `GET /compute/session` payload. Never fails; broker status has its
    /// own internal timeout.
    pub async fn status(&self) -> SessionStatus {
        let broker = self.inner.broker.status().await;
        let shared = self.inner.shared.lock().expect("gpu session lock");
        // Project a dispatching state when desired=Running but the cached
        // observation still says parked/failed (e.g. right after a PUT).
        let mut state = shared.state;
        if shared.desired == Desired::Running
            && matches!(state, SessionState::Parked | SessionState::Failed)
        {
            state = SessionState::Dispatching;
        }
        let idle_remaining = match shared.desired {
            Desired::Running => Some(
                self.inner
                    .config
                    .idle_timeout
                    .saturating_sub(shared.last_activity.elapsed())
                    .as_secs(),
            ),
            Desired::Parked => None,
        };
        SessionStatus {
            enabled: true,
            state,
            desired: shared.desired,
            cluster_name: self.inner.config.cluster_name.clone(),
            state_since_ms: shared.state_since_ms,
            idle_seconds_remaining: idle_remaining,
            detail: shared.detail.clone(),
            failure_reason: shared.failure_reason.clone(),
            broker,
        }
    }

    /// `PUT /compute/session` — set the desired state and return immediately
    /// (idempotent); the reconcile loop converges asynchronously.
    pub fn set_desired(&self, desired: Desired) -> SessionState {
        let mut shared = self.inner.shared.lock().expect("gpu session lock");
        shared.desired = desired;
        match desired {
            Desired::Running => {
                shared.last_activity = Instant::now();
                shared.failure_reason = None;
                shared.failed_at = None;
                if shared.episode_started.is_none() {
                    shared.episode_started = Some(Instant::now());
                }
                if matches!(shared.state, SessionState::Parked | SessionState::Failed) {
                    transition(
                        &mut shared,
                        &self.inner.progress,
                        SessionState::Dispatching,
                        Some("creating RayCluster".to_string()),
                    );
                }
            }
            Desired::Parked => {
                if !matches!(shared.state, SessionState::Parked | SessionState::Parking) {
                    transition(
                        &mut shared,
                        &self.inner.progress,
                        SessionState::Parking,
                        None,
                    );
                }
            }
        }
        drop(shared);
        self.inner.wake.notify_one();
        desired_projection(&self.inner.shared.lock().expect("gpu session lock"))
    }

    /// Reset the idle auto-park clock. Hosts that observe user activity
    /// outside the broker's subscriber count (the session-manager's
    /// per-world mux touches on every `/worlds/:name/*` request and VCS
    /// mutation) call this so idle auto-park measures real use. No-op cheap;
    /// also wakes the reconcile loop so the projection stays fresh.
    pub fn touch_activity(&self) {
        self.inner
            .shared
            .lock()
            .expect("gpu session lock")
            .last_activity = Instant::now();
        self.inner.wake.notify_one();
    }

    /// Auto-dispatch hook (Q4): called from `compute_layout_put` after a
    /// successful remote reselect. Non-blocking; only fires when the
    /// observed state is parked/failed.
    pub fn auto_dispatch_if_parked(&self) {
        let should = {
            let shared = self.inner.shared.lock().expect("gpu session lock");
            matches!(shared.state, SessionState::Parked | SessionState::Failed)
        };
        if should {
            tracing::info!("auto-dispatching GPU session (cluster engine selected)");
            self.set_desired(Desired::Running);
        }
    }
}

/// The state we report after a desired-state write, before the next tick.
fn desired_projection(shared: &Shared) -> SessionState {
    match shared.desired {
        Desired::Running => match shared.state {
            SessionState::Parked | SessionState::Failed | SessionState::Dispatching => {
                SessionState::Dispatching
            }
            s => s,
        },
        Desired::Parked => match shared.state {
            SessionState::Parked => SessionState::Parked,
            _ => SessionState::Parking,
        },
    }
}

/// Record a state transition and mirror it onto the lifecycle progress task
/// (one task per session lifecycle: Start → UpdateLabel per transition →
/// Finish on ready / Fail on failure).
fn transition(
    shared: &mut Shared,
    progress: &ProgressLog,
    state: SessionState,
    detail: Option<String>,
) {
    if shared.state == state && shared.detail == detail {
        return;
    }
    shared.state = state;
    shared.state_since = Instant::now();
    shared.state_since_ms = unix_ms();
    shared.detail = detail;

    let mut started = false;
    match state {
        SessionState::Dispatching | SessionState::Queued if shared.task.is_none() => {
            let id = progress.start(logs::LOG_GROUP, state.progress_label(shared.detail.as_deref()));
            shared.task = Some(id);
            started = true;
        }
        SessionState::Ready => {
            if let Some(id) = shared.task.take() {
                progress.finish(id);
            }
        }
        SessionState::Failed => {
            if let Some(id) = shared.task.take() {
                progress.fail(
                    id,
                    shared
                        .detail
                        .clone()
                        .unwrap_or_else(|| "unknown failure".into()),
                );
            }
        }
        SessionState::Parked => {
            if let Some(id) = shared.task.take() {
                progress.finish(id);
            }
        }
        _ => {}
    }
    if !started {
        if let Some(id) = shared.task {
            progress.update_label(id, state.progress_label(shared.detail.as_deref()));
        }
    }
}

impl Controller {
    async fn run(self: Arc<Self>) {
        let mut tailer = LogTailer::new(
            self.client.clone(),
            self.config.namespace.clone(),
            self.progress.clone(),
        );
        let mut first = true;
        let mut last_log_poll = Instant::now() - LOG_POLL;
        loop {
            let tick = {
                let shared = self.shared.lock().expect("gpu session lock");
                match shared.state {
                    SessionState::Parked => PARKED_TICK,
                    _ => ACTIVE_TICK,
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(tick) => {}
                _ = self.wake.notified() => {}
            }
            if let Err(e) = self.reconcile_once(first, &mut tailer, &mut last_log_poll).await {
                tracing::warn!(error = %format!("{e:#}"), "gpu session reconcile tick failed");
            }
            first = false;
        }
    }

    /// One reconcile tick. NEVER holds the `shared` lock across an `.await`
    /// (a std MutexGuard is not Send); each phase snapshots what it needs
    /// under the lock, drops it, then does IO.
    async fn reconcile_once(
        &self,
        initial: bool,
        tailer: &mut LogTailer,
        last_log_poll: &mut Instant,
    ) -> anyhow::Result<()> {
        // --- Observe (IO half; every call timeout-bound) ---
        let raycluster = match self.get_raycluster().await {
            Ok(rc) => rc,
            Err(e) => {
                // Observation failure must not flap the state machine; keep
                // the cached state and retry next tick.
                tracing::warn!(error = %format!("{e:#}"), "gpu session: RayCluster get failed");
                return Ok(());
            }
        };

        // Boot adoption (Q8): a pre-existing, non-terminating CR means we
        // restarted mid-session — adopt it as desired=Running with a fresh
        // idle timer.
        if initial {
            if let Some(rc) = &raycluster {
                if !rc.terminating {
                    let mut shared = self.shared.lock().expect("gpu session lock");
                    shared.desired = Desired::Running;
                    shared.last_activity = Instant::now();
                    shared.episode_started = Some(Instant::now());
                    drop(shared);
                    tracing::info!("gpu session: adopted pre-existing RayCluster");
                }
            }
        }

        let workload = match &raycluster {
            Some(rc) if !rc.terminating => self.get_workload(&rc.uid).await?,
            _ => None,
        };
        // The head pod is also fetched while the CR is terminating so the
        // tailer keeps working through the Parking state (Q7).
        let head_pod = match &raycluster {
            Some(_) => self.get_head_pod().await?,
            None => None,
        };
        let broker = self.broker.status().await;
        let subscribers = self.broker.subscriber_count().await;

        let (desired, latched) = {
            let shared = self.shared.lock().expect("gpu session lock");
            (shared.desired, shared.failure_reason.clone())
        };

        let derived = observe::derive(&DeriveInput {
            desired,
            raycluster: raycluster.clone(),
            workload: workload.clone(),
            head_pod: head_pod.clone(),
            broker: BrokerFacts {
                connected: broker.connected,
                worker_ok: broker.worker_ok,
            },
            latched_failure: latched,
            admission_timeout: self.config.admission_timeout,
            workload_grace: observe::WORKLOAD_GRACE,
            restart_limit: observe::RESTART_LIMIT,
        });

        // --- Decide (lock held, no IO) ---
        enum PostAction {
            None,
            Delete,
            Create,
            /// External delete mid-episode: surface + park (Q8), no create.
            ExternalDeletePark,
        }
        let mut action = PostAction::None;
        {
            let mut shared = self.shared.lock().expect("gpu session lock");

            if let Some(reason) = derived.new_failure.clone() {
                tracing::warn!(reason = %reason, "gpu session failed");
                shared.failure_reason = Some(reason.clone());
                shared.failed_at = Some(Instant::now());
                // Q8: quota never granted / evicted → Fail event + auto-park
                // (don't leave a pending Workload). Crashloop holds the CR
                // for the log-grace window; that delete fires below once the
                // grace elapses.
                if !reason.contains("crash-looping") {
                    shared.desired = Desired::Parked;
                    shared.episode_started = None;
                }
            }

            transition(&mut shared, &self.progress, derived.state, derived.detail.clone());

            if raycluster.is_some() {
                shared.had_cluster = true;
            }

            match derived.state {
                SessionState::Failed => {
                    let grace_elapsed = shared
                        .failed_at
                        .is_some_and(|t| t.elapsed() >= FAILURE_LOG_GRACE);
                    let crashloop_hold = shared
                        .failure_reason
                        .as_deref()
                        .is_some_and(|r| r.contains("crash-looping"))
                        && !grace_elapsed;
                    if raycluster.is_some() && !shared.delete_issued && !crashloop_hold {
                        // Crashloop log-grace elapsed (other failures already
                        // parked in the latch block above): auto-park now.
                        shared.desired = Desired::Parked;
                        shared.episode_started = None;
                        action = PostAction::Delete;
                    }
                }
                // Head-start timeout: admitted/head_starting stuck past the
                // limit fails the session (the pod is up but the worker never
                // came healthy — usually a crash the pod-level checks miss).
                SessionState::Admitted | SessionState::HeadStarting
                    if shared.state_since.elapsed() >= self.config.head_start_timeout =>
                {
                    let reason = format!(
                        "head pod did not become ready within {}s",
                        self.config.head_start_timeout.as_secs()
                    );
                    tracing::warn!(reason = %reason, "gpu session failed");
                    shared.failure_reason = Some(reason.clone());
                    shared.failed_at = Some(Instant::now());
                    shared.desired = Desired::Parked;
                    shared.episode_started = None;
                    transition(&mut shared, &self.progress, SessionState::Failed, Some(reason));
                    if raycluster.is_some() && !shared.delete_issued {
                        action = PostAction::Delete;
                    }
                }
                _ => {
                    if shared.desired == Desired::Running
                        && raycluster.is_none()
                        && !shared.delete_issued
                    {
                        if shared.had_cluster {
                            // The CR vanished under us (eviction /
                            // max-exec-time / janitor) — surface and park
                            // instead of silently re-dispatching (Q8).
                            action = PostAction::ExternalDeletePark;
                        } else {
                            action = PostAction::Create;
                        }
                    } else if shared.desired == Desired::Parked
                        && raycluster
                            .as_ref()
                            .is_some_and(|rc| !rc.terminating)
                        && !shared.delete_issued
                    {
                        action = PostAction::Delete;
                    }
                }
            }

            // Stuck-terminating watchdog (Q8): warn periodically, never
            // touch finalizers.
            if derived.state == SessionState::Parking {
                let parking_since = *shared.parking_since.get_or_insert_with(Instant::now);
                let stale = parking_since.elapsed() >= PARKING_WARN_AFTER;
                let due = shared
                    .last_parking_warn
                    .is_none_or(|t| t.elapsed() >= PARKING_WARN_EVERY);
                if stale && due {
                    self.progress.warn(
                        logs::LOG_GROUP,
                        "RayCluster stuck terminating for over 5 minutes; \
                         waiting on finalizers (never force-removing)",
                    );
                    shared.last_parking_warn = Some(Instant::now());
                }
            } else {
                shared.parking_since = None;
                shared.last_parking_warn = None;
            }

            // CR fully gone → a fresh dispatch may create again.
            if raycluster.is_none() {
                shared.delete_issued = false;
                if shared.desired == Desired::Parked {
                    shared.had_cluster = false;
                }
            }

            // Hard session cap (controller half; the Kueue max-exec-time
            // label is the cluster-side half).
            if shared.desired == Desired::Running && derived.state == SessionState::Ready {
                if let Some(started) = shared.episode_started {
                    if started.elapsed() >= self.config.max_session {
                        self.progress.warn(
                            logs::LOG_GROUP,
                            format!(
                                "max session length ({}s) reached; parking",
                                self.config.max_session.as_secs()
                            ),
                        );
                        shared.desired = Desired::Parked;
                        shared.episode_started = None;
                    }
                }
            }

            // Idle auto-park (Q5): the server cannot observe frontend-local
            // engine selections, so the in-process signal is the broker's
            // live subscriber count — zero `/graph/layout/stream` consumers
            // means no renderer is using the cluster.
            if derived.state == SessionState::Ready {
                if subscribers > 0 {
                    shared.last_activity = Instant::now();
                } else if shared.desired == Desired::Running
                    && shared.last_activity.elapsed() >= self.config.idle_timeout
                {
                    self.progress.info(
                        logs::LOG_GROUP,
                        format!(
                            "idle for {}s with no layout subscribers; parking",
                            self.config.idle_timeout.as_secs()
                        ),
                    );
                    shared.desired = Desired::Parked;
                    shared.episode_started = None;
                }
            }
        }

        // --- Act (IO, lock released) ---
        match action {
            PostAction::None => {}
            PostAction::Create => match self.create_raycluster().await {
                Ok(()) => {
                    self.progress.info(
                        logs::LOG_GROUP,
                        "RayCluster created; waiting for Kueue admission",
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "gpu session: create failed; retrying next tick");
                }
            },
            PostAction::Delete => match self.delete_raycluster().await {
                Ok(()) => {
                    self.shared
                        .lock()
                        .expect("gpu session lock")
                        .delete_issued = true;
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "gpu session: delete failed; retrying next tick");
                }
            },
            PostAction::ExternalDeletePark => {
                let mut shared = self.shared.lock().expect("gpu session lock");
                shared.had_cluster = false;
                shared.desired = Desired::Parked;
                shared.episode_started = None;
                if shared.failure_reason.is_none() {
                    shared.failure_reason = Some(
                        "RayCluster deleted externally (evicted, max-exec-time, or janitor)"
                            .to_string(),
                    );
                }
                let reason = shared
                    .failure_reason
                    .clone()
                    .unwrap_or_else(|| "cluster deleted externally".into());
                self.progress.warn(logs::LOG_GROUP, reason);
            }
        }

        // Head-pod log tailing (Q7) on its own 5s cadence. Failed is
        // included so the crashloop log-grace window actually captures logs.
        let tail_states = matches!(
            derived.state,
            SessionState::Admitted
                | SessionState::HeadStarting
                | SessionState::Ready
                | SessionState::Parking
                | SessionState::Failed
        );
        if tail_states && last_log_poll.elapsed() >= LOG_POLL {
            if let Some(head) = &head_pod {
                tailer.poll(head).await;
                *last_log_poll = Instant::now();
            }
        }

        Ok(())
    }

    fn raycluster_api(&self) -> Api<DynamicObject> {
        let gvk = GroupVersionKind::gvk("ray.io", "v1", "RayCluster");
        Api::namespaced_with(
            self.client.clone(),
            &self.config.namespace,
            &ApiResource::from_gvk(&gvk),
        )
    }

    fn workload_api(&self) -> Api<DynamicObject> {
        let gvk = GroupVersionKind::gvk("kueue.x-k8s.io", "v1beta1", "Workload");
        Api::namespaced_with(
            self.client.clone(),
            &self.config.namespace,
            &ApiResource::from_gvk(&gvk),
        )
    }

    async fn get_raycluster(&self) -> anyhow::Result<Option<RayClusterFacts>> {
        let api = self.raycluster_api();
        let name = &self.config.cluster_name;
        match tokio::time::timeout(KUBE_TIMEOUT, api.get(name)).await {
            Ok(Ok(obj)) => {
                let uid = obj
                    .metadata
                    .uid
                    .clone()
                    .ok_or_else(|| anyhow!("RayCluster has no uid"))?;
                let age = obj
                    .metadata
                    .creation_timestamp
                    .as_ref()
                    .map(|t| {
                        let elapsed =
                            k8s_openapi::jiff::Timestamp::now().duration_since(t.0);
                        std::time::Duration::try_from(elapsed).unwrap_or_default()
                    })
                    .unwrap_or_default();
                Ok(Some(RayClusterFacts {
                    uid,
                    terminating: obj.metadata.deletion_timestamp.is_some(),
                    age,
                }))
            }
            Ok(Err(kube::Error::Api(e))) if e.code == 404 => Ok(None),
            Ok(Err(e)) => Err(e).context("get RayCluster"),
            Err(_) => Err(anyhow!("get RayCluster timed out")),
        }
    }

    /// Find the Kueue Workload owned by the RayCluster CR (ownerReferences
    /// UID match) and extract its admission facts from conditions.
    async fn get_workload(&self, owner_uid: &str) -> anyhow::Result<Option<WorkloadFacts>> {
        let api = self.workload_api();
        let list = tokio::time::timeout(KUBE_TIMEOUT, api.list(&ListParams::default()))
            .await
            .map_err(|_| anyhow!("list Workloads timed out"))?
            .context("list Workloads")?;
        for wl in &list.items {
            let owned = wl
                .metadata
                .owner_references
                .as_ref()
                .is_some_and(|refs| refs.iter().any(|r| r.uid == owner_uid));
            if !owned {
                continue;
            }
            let conditions = wl
                .data
                .pointer("/status/conditions")
                .and_then(serde_json::Value::as_array);
            let mut facts = WorkloadFacts::default();
            if let Some(conditions) = conditions {
                for c in conditions {
                    let ty = c.get("type").and_then(serde_json::Value::as_str);
                    let status = c.get("status").and_then(serde_json::Value::as_str);
                    match (ty, status) {
                        (Some("Admitted"), Some("True")) => facts.admitted = true,
                        (Some("Evicted"), Some("True")) => facts.evicted = true,
                        _ => {}
                    }
                    if let Some(msg) = c.get("message").and_then(serde_json::Value::as_str) {
                        if !msg.is_empty() {
                            facts.message = Some(msg.to_string());
                        }
                    }
                }
            }
            return Ok(Some(facts));
        }
        Ok(None)
    }

    async fn get_head_pod(&self) -> anyhow::Result<Option<HeadPodFacts>> {
        let pods = logs::list_head_pods(&self.client, &self.config.namespace, &self.config.cluster_name)
            .await?;
        Ok(pods.first().map(head_pod_facts))
    }

    async fn create_raycluster(&self) -> anyhow::Result<()> {
        let manifest = self.config.template.render(
            &self.config.cluster_name,
            &self.config.namespace,
            self.config.max_session.as_secs(),
        );
        let obj: DynamicObject =
            serde_json::from_value(manifest).context("template does not decode as DynamicObject")?;
        let api = self.raycluster_api();
        match tokio::time::timeout(KUBE_TIMEOUT, api.create(&PostParams::default(), &obj)).await {
            Ok(Ok(_)) => Ok(()),
            // Old CR still finalizing — the Dispatching state absorbs this;
            // next tick retries.
            Ok(Err(kube::Error::Api(e))) if e.code == 409 => Ok(()),
            Ok(Err(e)) => Err(e).context("create RayCluster"),
            Err(_) => Err(anyhow!("create RayCluster timed out")),
        }
    }

    async fn delete_raycluster(&self) -> anyhow::Result<()> {
        let api = self.raycluster_api();
        let name = self.config.cluster_name.clone();
        match tokio::time::timeout(KUBE_TIMEOUT, api.delete(&name, &DeleteParams::default())).await
        {
            Ok(Ok(_)) => {
                self.progress
                    .info(logs::LOG_GROUP, "RayCluster delete issued (parking)");
                Ok(())
            }
            Ok(Err(kube::Error::Api(e))) if e.code == 404 => Ok(()),
            Ok(Err(e)) => Err(e).context("delete RayCluster"),
            Err(_) => Err(anyhow!("delete RayCluster timed out")),
        }
    }
}

/// Extract head-pod facts from a typed Pod (pure; unit-tested below).
fn head_pod_facts(pod: &k8s_openapi::api::core::v1::Pod) -> HeadPodFacts {
    let status = pod.status.as_ref();
    let running = status
        .and_then(|s| s.phase.as_deref())
        .is_some_and(|p| p == "Running");
    let mut crash_loop_back_off = false;
    let mut restart_count = 0u32;
    for cs in status
        .and_then(|s| s.container_statuses.as_ref())
        .into_iter()
        .flatten()
    {
        restart_count = restart_count.saturating_add(cs.restart_count.max(0) as u32);
        if cs
            .state
            .as_ref()
            .and_then(|s| s.waiting.as_ref())
            .and_then(|w| w.reason.as_deref())
            .is_some_and(|r| r == "CrashLoopBackOff")
        {
            crash_loop_back_off = true;
        }
    }
    HeadPodFacts {
        name: pod.metadata.name.clone().unwrap_or_default(),
        uid: pod.metadata.uid.clone().unwrap_or_default(),
        running,
        crash_loop_back_off,
        restart_count,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{ContainerState, ContainerStateWaiting, ContainerStatus, Pod, PodStatus};

    fn pod(phase: &str, waiting: Option<&str>, restarts: i32) -> Pod {
        Pod {
            metadata: kube::api::ObjectMeta {
                name: Some("head-0".into()),
                uid: Some("uid-9".into()),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some(phase.into()),
                container_statuses: Some(vec![ContainerStatus {
                    name: "ray-head".into(),
                    ready: false,
                    restart_count: restarts,
                    image: "img".into(),
                    image_id: String::new(),
                    started: None,
                    state: Some(ContainerState {
                        waiting: waiting.map(|r| ContainerStateWaiting {
                            reason: Some(r.into()),
                            message: None,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn head_pod_facts_running() {
        let f = head_pod_facts(&pod("Running", None, 0));
        assert!(f.running);
        assert!(!f.crash_loop_back_off);
        assert_eq!(f.restart_count, 0);
        assert_eq!(f.name, "head-0");
    }

    #[test]
    fn head_pod_facts_crashloop() {
        let f = head_pod_facts(&pod("Pending", Some("CrashLoopBackOff"), 7));
        assert!(!f.running);
        assert!(f.crash_loop_back_off);
        assert_eq!(f.restart_count, 7);
    }

    #[test]
    fn desired_projection_dispatch_and_park() {
        let mut shared = Shared::new();
        shared.desired = Desired::Running;
        assert_eq!(desired_projection(&shared), SessionState::Dispatching);
        shared.state = SessionState::Ready;
        assert_eq!(desired_projection(&shared), SessionState::Ready);
        shared.desired = Desired::Parked;
        assert_eq!(desired_projection(&shared), SessionState::Parking);
        shared.state = SessionState::Parked;
        assert_eq!(desired_projection(&shared), SessionState::Parked);
    }

    #[test]
    fn transition_lifecycle_progress_task() {
        let progress = ProgressLog::new();
        let mut shared = Shared::new();
        transition(
            &mut shared,
            &progress,
            SessionState::Dispatching,
            Some("creating RayCluster".into()),
        );
        assert!(shared.task.is_some());
        let events = progress.since(0).events;
        assert!(matches!(
            events.last().map(|e| &e.event),
            Some(crate::progress::ProgressEvent::Start { .. })
        ));
        transition(&mut shared, &progress, SessionState::Queued, None);
        assert!(matches!(
            progress.since(0).events.last().map(|e| &e.event),
            Some(crate::progress::ProgressEvent::UpdateLabel { .. })
        ));
        transition(&mut shared, &progress, SessionState::Ready, None);
        assert!(shared.task.is_none());
        assert!(matches!(
            progress.since(0).events.last().map(|e| &e.event),
            Some(crate::progress::ProgressEvent::Finish { .. })
        ));
        // A failure with an open task emits Fail.
        transition(
            &mut shared,
            &progress,
            SessionState::Dispatching,
            None,
        );
        transition(
            &mut shared,
            &progress,
            SessionState::Failed,
            Some("boom".into()),
        );
        assert!(matches!(
            progress.since(0).events.last().map(|e| &e.event),
            Some(crate::progress::ProgressEvent::Fail { .. })
        ));
    }
}
