//! On-demand GPU compute session controller.
//!
//! Owns the lifecycle of the session's compute Job and its Service in the
//! compute namespace: dispatch when a user picks a cluster engine (or
//! explicitly), live state derivation from the Job, its Kueue Workload, and
//! the Job's pod plus broker status (Q3, see [`observe`]), idle auto-park
//! (Q5), a controller-side hard session cap, and compute-pod log surfacing
//! into the `gpu-session` progress group (Q7, see [`logs`]).
//!
//! Kueue ground truth honoured here:
//! - `spec.suspend` is NEVER touched — Kueue owns it (the chart creates the
//!   Job suspended; Kueue unsuspends on admission and re-suspends on
//!   preemption, re-queuing the Workload).
//! - Park = DELETE the Job with background propagation (pods die with it,
//!   quota is freed), then the Service.
//! - Dispatch = POST the Service, then a fresh Job stamped from the
//!   chart-rendered template (see [`template`]); `AlreadyExists` while the
//!   old Job finalizes is absorbed by the `Parking`/`Dispatching` states.
//!
//! All kube access goes through the dynamic API (`serde_json::Value`) for
//! Job/Workload and typed reads/writes for core pods/Services/pods/log (R12). Every
//! kube call is timeout-bound; nothing in the handler or controller paths
//! panics.

pub mod logs;
pub mod observe;
pub mod template;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use k8s_openapi::jiff::Timestamp;
use kube::api::{
    Api, ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, ObjectMeta,
    PostParams,
};
use kube::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::compute_broker::{BrokerStatus, ComputeBroker};
use crate::progress::{ProgressLog, TaskId};

use logs::LogTailer;
use observe::{BrokerFacts, ComputePodFacts, DeriveInput, JobFacts, WorkloadFacts};
use template::SessionTemplate;

/// Reconcile tick while the session is anything but parked (Q3 ~2s).
const ACTIVE_TICK: Duration = Duration::from_secs(2);
/// Reconcile tick while parked (Q5 default reconcile cadence).
const PARKED_TICK: Duration = Duration::from_secs(30);
/// Bound on every kube API call.
const KUBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Log-tail cadence while a compute pod exists (Q7).
const LOG_POLL: Duration = Duration::from_secs(5);
/// How long a crash-looping Job is kept so the tailer can capture logs before
/// the auto-park delete (Q8).
const FAILURE_LOG_GRACE: Duration = Duration::from_secs(60);
/// Parking longer than this emits periodic warn events; finalizers are NEVER
/// force-removed (Q8).
const PARKING_WARN_AFTER: Duration = Duration::from_secs(5 * 60);
const PARKING_WARN_EVERY: Duration = Duration::from_secs(60);

/// Label the Job controller stamps on every pod of a Job; the Service
/// selects the session's pod through it.
const JOB_NAME_LABEL: &str = "batch.kubernetes.io/job-name";
/// Name of the container port the chart declares for graph-compute's gRPC
/// server (the Service's `targetPort`).
const GRPC_PORT_NAME: &str = "grpc";
/// Label prefix owned by Kueue; never copied onto the Service.
const KUEUE_LABEL_PREFIX: &str = "kueue.x-k8s.io/";

/// Observed session state (Q3). Serialized snake_case on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Parked,
    Dispatching,
    Queued,
    Admitted,
    Starting,
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
            SessionState::Starting => "starting",
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
            SessionState::Admitted => "GPU session admitted; starting compute pod",
            SessionState::Starting => "Compute pod running; waiting for the worker",
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
    /// Name of the session Job and of the Service that fronts it.
    pub cluster_name: String,
    pub namespace: String,
    pub template: SessionTemplate,
    /// graph-compute gRPC port the Service exposes.
    pub compute_port: u16,
    pub idle_timeout: Duration,
    pub admission_timeout: Duration,
    pub max_session: Duration,
    /// Admitted/starting longer than this fails the session.
    pub start_timeout: Duration,
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
    /// Delete already issued for the current Job (awaiting NotFound).
    delete_issued: bool,
    /// We have observed a Job in this episode — distinguishes "never
    /// created" (dispatch should create) from "deleted under us" (Q8:
    /// external delete → surface + park, not silent redispatch).
    had_job: bool,
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
            had_job: false,
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
    /// a pre-existing Job is adopted with desired=Running) happens on the
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
                        Some("creating compute Job".to_string()),
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
        let job = match self.get_job().await {
            Ok(j) => j,
            Err(e) => {
                // Observation failure must not flap the state machine; keep
                // the cached state and retry next tick.
                tracing::warn!(error = %format!("{e:#}"), "gpu session: Job get failed");
                return Ok(());
            }
        };

        // Boot adoption (Q8): a pre-existing, non-terminating Job means we
        // restarted mid-session — adopt it as desired=Running with a fresh
        // idle timer, and make sure its Service still exists.
        if initial {
            if let Some(j) = &job {
                if !j.terminating {
                    {
                        let mut shared = self.shared.lock().expect("gpu session lock");
                        shared.desired = Desired::Running;
                        shared.last_activity = Instant::now();
                        shared.episode_started = Some(Instant::now());
                    }
                    tracing::info!("gpu session: adopted pre-existing compute Job");
                    let (_, service) = self.stamped();
                    if let Err(e) = self.ensure_service(&service).await {
                        tracing::warn!(error = %format!("{e:#}"), "gpu session: adopted Job's Service check failed");
                    }
                }
            }
        }

        let workload = match &job {
            Some(j) if !j.terminating => self.get_workload(&j.uid).await?,
            _ => None,
        };
        // The compute pod is also fetched while the Job is terminating so the
        // tailer keeps working through the Parking state (Q7).
        let compute_pod = match &job {
            Some(_) => self.get_compute_pod().await?,
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
            job: job.clone(),
            workload: workload.clone(),
            compute_pod: compute_pod.clone(),
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
                // Q8: quota never granted / deactivated → Fail event + auto-park
                // (don't leave a pending Workload). Crashloop holds the Job
                // for the log-grace window; that delete fires below once the
                // grace elapses.
                if !reason.contains("crash-looping") {
                    shared.desired = Desired::Parked;
                    shared.episode_started = None;
                }
            }

            transition(&mut shared, &self.progress, derived.state, derived.detail.clone());

            if job.is_some() {
                shared.had_job = true;
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
                    if job.is_some() && !shared.delete_issued && !crashloop_hold {
                        // Crashloop log-grace elapsed (other failures already
                        // parked in the latch block above): auto-park now.
                        shared.desired = Desired::Parked;
                        shared.episode_started = None;
                        action = PostAction::Delete;
                    }
                }
                // Start timeout: admitted/starting stuck past the limit fails
                // the session (the pod is up but the worker never came
                // healthy — usually a crash the pod-level checks miss).
                SessionState::Admitted | SessionState::Starting
                    if shared.state_since.elapsed() >= self.config.start_timeout =>
                {
                    let reason = format!(
                        "compute pod did not become ready within {}s",
                        self.config.start_timeout.as_secs()
                    );
                    tracing::warn!(reason = %reason, "gpu session failed");
                    shared.failure_reason = Some(reason.clone());
                    shared.failed_at = Some(Instant::now());
                    shared.desired = Desired::Parked;
                    shared.episode_started = None;
                    transition(&mut shared, &self.progress, SessionState::Failed, Some(reason));
                    if job.is_some() && !shared.delete_issued {
                        action = PostAction::Delete;
                    }
                }
                _ => {
                    if shared.desired == Desired::Running
                        && job.is_none()
                        && !shared.delete_issued
                    {
                        if shared.had_job {
                            // The Job vanished under us (manual delete,
                            // janitor) — surface and park instead of
                            // silently re-dispatching (Q8).
                            action = PostAction::ExternalDeletePark;
                        } else {
                            action = PostAction::Create;
                        }
                    } else if shared.desired == Desired::Parked
                        && job
                            .as_ref()
                            .is_some_and(|j| !j.terminating)
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
                        "compute Job stuck terminating for over 5 minutes; \
                         waiting on finalizers (never force-removing)",
                    );
                    shared.last_parking_warn = Some(Instant::now());
                }
            } else {
                shared.parking_since = None;
                shared.last_parking_warn = None;
            }

            // Job fully gone → a fresh dispatch may create again.
            if job.is_none() {
                shared.delete_issued = false;
                if shared.desired == Desired::Parked {
                    shared.had_job = false;
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
            PostAction::Create => match self.create_session().await {
                Ok(()) => {
                    self.progress.info(
                        logs::LOG_GROUP,
                        "compute Job created; waiting for Kueue admission",
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "gpu session: create failed; retrying next tick");
                }
            },
            PostAction::Delete => match self.delete_session().await {
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
                let reason = {
                    let mut shared = self.shared.lock().expect("gpu session lock");
                    shared.had_job = false;
                    shared.desired = Desired::Parked;
                    shared.episode_started = None;
                    if shared.failure_reason.is_none() {
                        shared.failure_reason = Some("compute Job deleted externally".to_string());
                    }
                    shared
                        .failure_reason
                        .clone()
                        .unwrap_or_else(|| "Job deleted externally".into())
                };
                self.progress.warn(logs::LOG_GROUP, reason);
                // The Service belonged to the vanished Job.
                if let Err(e) = self.delete_service().await {
                    tracing::warn!(error = %format!("{e:#}"), "gpu session: Service cleanup after external delete failed");
                }
            }
        }

        // Compute-pod log tailing (Q7) on its own 5s cadence. Failed is
        // included so the crashloop log-grace window actually captures logs.
        let tail_states = matches!(
            derived.state,
            SessionState::Admitted
                | SessionState::Starting
                | SessionState::Ready
                | SessionState::Parking
                | SessionState::Failed
        );
        if tail_states && last_log_poll.elapsed() >= LOG_POLL {
            if let Some(pod) = &compute_pod {
                tailer.poll(pod).await;
                *last_log_poll = Instant::now();
            }
        }

        Ok(())
    }

    fn job_api(&self) -> Api<DynamicObject> {
        let gvk = GroupVersionKind::gvk("batch", "v1", "Job");
        Api::namespaced_with(
            self.client.clone(),
            &self.config.namespace,
            &ApiResource::from_gvk(&gvk),
        )
    }

    /// Kueue Workloads are read at `v1beta2`: it is the storage version since
    /// Kueue 0.15 and the only version served from 0.20 onward (`v1beta1` is
    /// `served: false`). The fields read — `metadata.ownerReferences`,
    /// `spec.active`, and `status.conditions[].{type,status,reason,message,
    /// lastTransitionTime}` for `QuotaReserved` / `Admitted` / `Evicted` /
    /// `Finished` — are identical in both versions.
    fn workload_api(&self) -> Api<DynamicObject> {
        let gvk = GroupVersionKind::gvk("kueue.x-k8s.io", "v1beta2", "Workload");
        Api::namespaced_with(
            self.client.clone(),
            &self.config.namespace,
            &ApiResource::from_gvk(&gvk),
        )
    }

    fn service_api(&self) -> Api<Service> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    async fn get_job(&self) -> anyhow::Result<Option<JobFacts>> {
        let api = self.job_api();
        match tokio::time::timeout(KUBE_TIMEOUT, api.get(&self.config.cluster_name)).await {
            Ok(Ok(obj)) => Ok(Some(observe::job_facts(&obj, Timestamp::now())?)),
            Ok(Err(kube::Error::Api(e))) if e.code == 404 => Ok(None),
            Ok(Err(e)) => Err(e).context("get Job"),
            Err(_) => Err(anyhow!("get Job timed out")),
        }
    }

    /// Find the Kueue Workload owned by the session Job (ownerReferences
    /// UID match) and extract its admission facts from conditions.
    async fn get_workload(&self, owner_uid: &str) -> anyhow::Result<Option<WorkloadFacts>> {
        let api = self.workload_api();
        let list = tokio::time::timeout(KUBE_TIMEOUT, api.list(&ListParams::default()))
            .await
            .map_err(|_| anyhow!("list Workloads timed out"))?
            .context("list Workloads")?;
        Ok(observe::owned_workload(&list.items, owner_uid)
            .map(|wl| observe::workload_facts(wl, Timestamp::now())))
    }

    async fn get_compute_pod(&self) -> anyhow::Result<Option<ComputePodFacts>> {
        let pods = logs::list_compute_pods(
            &self.client,
            &self.config.namespace,
            &self.config.cluster_name,
        )
        .await?;
        Ok(observe::pick_current_pod(&pods).map(observe::compute_pod_facts))
    }

    /// The stamped Job manifest and the Service that fronts it.
    fn stamped(&self) -> (Value, Service) {
        let job = self.config.template.render(
            &self.config.cluster_name,
            &self.config.namespace,
            self.config.max_session.as_secs(),
        );
        let service = service_manifest(
            &job,
            &self.config.cluster_name,
            &self.config.namespace,
            self.config.compute_port,
        );
        (job, service)
    }

    /// Create the Service, or keep the existing one when it already routes
    /// to this Job's pod; an existing Service with a different selector or
    /// ports is replaced.
    async fn ensure_service(&self, desired: &Service) -> anyhow::Result<()> {
        let api = self.service_api();
        let name = &self.config.cluster_name;
        match tokio::time::timeout(KUBE_TIMEOUT, api.create(&PostParams::default(), desired)).await
        {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(kube::Error::Api(e))) if e.code == 409 => {}
            Ok(Err(e)) => return Err(e).context("create compute Service"),
            Err(_) => return Err(anyhow!("create compute Service timed out")),
        }

        let existing = match tokio::time::timeout(KUBE_TIMEOUT, api.get(name)).await {
            Ok(Ok(s)) => s,
            Ok(Err(kube::Error::Api(e))) if e.code == 404 => {
                return Err(anyhow!("compute Service vanished while ensuring it"))
            }
            Ok(Err(e)) => return Err(e).context("get compute Service"),
            Err(_) => return Err(anyhow!("get compute Service timed out")),
        };
        if service_routes_like(&existing, desired) {
            return Ok(());
        }
        tracing::warn!(service = %name, "gpu session: replacing compute Service whose selector/ports differ");
        self.delete_service().await?;
        match tokio::time::timeout(KUBE_TIMEOUT, api.create(&PostParams::default(), desired)).await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e).context("re-create compute Service"),
            Err(_) => Err(anyhow!("re-create compute Service timed out")),
        }
    }

    /// Delete the Service; `NotFound` is absorbed.
    async fn delete_service(&self) -> anyhow::Result<()> {
        let api = self.service_api();
        let name = &self.config.cluster_name;
        match tokio::time::timeout(KUBE_TIMEOUT, api.delete(name, &DeleteParams::default())).await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(kube::Error::Api(e))) if e.code == 404 => Ok(()),
            Ok(Err(e)) => Err(e).context("delete compute Service"),
            Err(_) => Err(anyhow!("delete compute Service timed out")),
        }
    }

    /// Dispatch: Service first (so the DNS name exists before the pod), then
    /// the suspended Job Kueue admits.
    async fn create_session(&self) -> anyhow::Result<()> {
        let (job, service) = self.stamped();
        self.ensure_service(&service).await?;
        let obj: DynamicObject =
            serde_json::from_value(job).context("template does not decode as DynamicObject")?;
        let api = self.job_api();
        match tokio::time::timeout(KUBE_TIMEOUT, api.create(&PostParams::default(), &obj)).await {
            Ok(Ok(_)) => Ok(()),
            // Old Job still finalizing — the Dispatching state absorbs this;
            // next tick retries.
            Ok(Err(kube::Error::Api(e))) if e.code == 409 => Ok(()),
            Ok(Err(e)) => Err(e).context("create Job"),
            Err(_) => Err(anyhow!("create Job timed out")),
        }
    }

    /// Park: delete the Job (releases the quota), then the Service. A failed
    /// Service delete is only logged — the leftover Service has no endpoints
    /// and the next dispatch reuses it.
    async fn delete_session(&self) -> anyhow::Result<()> {
        self.delete_job().await?;
        if let Err(e) = self.delete_service().await {
            tracing::warn!(error = %format!("{e:#}"), "gpu session: compute Service delete failed; the next dispatch reuses it");
        }
        Ok(())
    }

    async fn delete_job(&self) -> anyhow::Result<()> {
        let api = self.job_api();
        let name = self.config.cluster_name.clone();
        // Background cascade: pods die with the Job; quota frees immediately.
        match tokio::time::timeout(
            KUBE_TIMEOUT,
            api.delete(&name, &DeleteParams::background()),
        )
        .await
        {
            Ok(Ok(_)) => {
                self.progress
                    .info(logs::LOG_GROUP, "compute Job delete issued (parking)");
                Ok(())
            }
            Ok(Err(kube::Error::Api(e))) if e.code == 404 => Ok(()),
            Ok(Err(e)) => Err(e).context("delete Job"),
            Err(_) => Err(anyhow!("delete Job timed out")),
        }
    }
}

/// ClusterIP Service fronting the session Job's gRPC port. It selects the
/// Job's pod by the Job controller's `job-name` label and inherits the
/// stamped Job's labels minus Kueue's.
fn service_manifest(job: &Value, name: &str, namespace: &str, port: u16) -> Service {
    let labels: BTreeMap<String, String> = job
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(key, _)| !key.starts_with(KUEUE_LABEL_PREFIX))
        .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
        .collect();
    Service {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            type_: Some("ClusterIP".to_string()),
            selector: Some(BTreeMap::from([(
                JOB_NAME_LABEL.to_string(),
                name.to_string(),
            )])),
            ports: Some(vec![ServicePort {
                name: Some(GRPC_PORT_NAME.to_string()),
                port: i32::from(port),
                target_port: Some(IntOrString::String(GRPC_PORT_NAME.to_string())),
                protocol: Some("TCP".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        status: None,
    }
}

/// True when `existing` already routes like `desired`: same selector and the
/// same port → targetPort pairs.
fn service_routes_like(existing: &Service, desired: &Service) -> bool {
    let (Some(e), Some(d)) = (&existing.spec, &desired.spec) else {
        return false;
    };
    let ports = |s: &ServiceSpec| {
        s.ports
            .iter()
            .flatten()
            .map(|p| (p.port, p.target_port.clone()))
            .collect::<Vec<_>>()
    };
    e.selector == d.selector && ports(e) == ports(d)
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
    use serde_json::json;

    fn stamped_job() -> Value {
        json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {
                "name": "jump-cannon-alpha",
                "namespace": "gpu-workloads",
                "labels": {
                    "app.kubernetes.io/name": "jump-cannon",
                    "app.kubernetes.io/component": "graph-compute",
                    "kueue.x-k8s.io/queue-name": "gpu",
                    "kueue.x-k8s.io/priority-class": "batch",
                    "kueue.x-k8s.io/max-exec-time-seconds": "14400"
                }
            }
        })
    }

    #[test]
    fn session_state_wire_names_match_as_str() {
        for state in [
            SessionState::Parked,
            SessionState::Dispatching,
            SessionState::Queued,
            SessionState::Admitted,
            SessionState::Starting,
            SessionState::Ready,
            SessionState::Parking,
            SessionState::Failed,
        ] {
            assert_eq!(
                serde_json::to_value(state).expect("serialize"),
                Value::String(state.as_str().to_string())
            );
        }
        assert_eq!(SessionState::Starting.as_str(), "starting");
    }

    #[test]
    fn service_manifest_selects_the_job_pod() {
        let svc = service_manifest(&stamped_job(), "jump-cannon-alpha", "gpu-workloads", 50051);
        assert_eq!(svc.metadata.name.as_deref(), Some("jump-cannon-alpha"));
        assert_eq!(svc.metadata.namespace.as_deref(), Some("gpu-workloads"));
        let spec = svc.spec.expect("spec");
        assert_eq!(spec.type_.as_deref(), Some("ClusterIP"));
        assert_eq!(
            spec.selector,
            Some(BTreeMap::from([(
                "batch.kubernetes.io/job-name".to_string(),
                "jump-cannon-alpha".to_string()
            )]))
        );
        let port = &spec.ports.expect("ports")[0];
        assert_eq!(port.name.as_deref(), Some("grpc"));
        assert_eq!(port.port, 50051);
        assert_eq!(port.target_port, Some(IntOrString::String("grpc".into())));
    }

    #[test]
    fn service_manifest_inherits_job_labels_without_kueue_ones() {
        let svc = service_manifest(&stamped_job(), "jump-cannon-alpha", "gpu-workloads", 50051);
        let labels = svc.metadata.labels.expect("labels");
        assert_eq!(
            labels.get("app.kubernetes.io/component").map(String::as_str),
            Some("graph-compute")
        );
        assert!(labels.keys().all(|k| !k.starts_with("kueue.x-k8s.io/")), "{labels:?}");
    }

    #[test]
    fn service_routes_like_detects_selector_and_port_drift() {
        let desired = service_manifest(&stamped_job(), "jump-cannon-alpha", "gpu-workloads", 50051);
        let mut same = desired.clone();
        // Server-populated fields do not matter.
        same.spec.as_mut().expect("spec").cluster_ip = Some("10.0.0.7".into());
        assert!(service_routes_like(&same, &desired));

        let mut other_selector = desired.clone();
        other_selector.spec.as_mut().expect("spec").selector =
            Some(BTreeMap::from([("some.other/label".to_string(), "x".to_string())]));
        assert!(!service_routes_like(&other_selector, &desired));

        let other_port = service_manifest(&stamped_job(), "jump-cannon-alpha", "gpu-workloads", 9);
        assert!(!service_routes_like(&other_port, &desired));

        let mut no_spec = desired.clone();
        no_spec.spec = None;
        assert!(!service_routes_like(&no_spec, &desired));
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
            Some("creating compute Job".into()),
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
        // A preemption after ready queues again: a fresh lifecycle task opens.
        transition(&mut shared, &progress, SessionState::Queued, Some("evicted".into()));
        assert!(shared.task.is_some());
        assert!(matches!(
            progress.since(0).events.last().map(|e| &e.event),
            Some(crate::progress::ProgressEvent::Start { .. })
        ));
        // A failure with an open task emits Fail.
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
