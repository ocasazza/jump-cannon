//! Session state derivation (Q3). The observed state is derived entirely from
//! cluster objects (session Job, its Kueue Workload, the Job's pod) plus
//! broker status — never persisted — so graph-api restarts and external
//! deletions self-heal.
//!
//! [`derive`] is a pure function over fabricated facts so the whole state
//! table is unit-testable without a cluster. The `*_facts` builders below are
//! the only code that reads Kubernetes objects; they are pure too, and the IO
//! half in `gpu_session::mod` only fetches objects and hands them over.

use std::time::Duration;

use anyhow::anyhow;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::jiff::Timestamp;
use kube::api::DynamicObject;
use serde_json::Value;

use super::{Desired, SessionState};

/// How long after Job create we wait for Kueue to spawn the owning Workload
/// before concluding Kueue is absent/misconfigured (R1) and deleting the Job
/// (a Job Kueue never sees would sit suspended forever).
pub const WORKLOAD_GRACE: Duration = Duration::from_secs(60);

/// Compute-pod restart count at or above which the session is latched failed.
pub const RESTART_LIMIT: u32 = 5;

/// Facts about the session Job. `None` in [`DeriveInput`] means the GET
/// returned NotFound.
#[derive(Clone, Debug)]
pub struct JobFacts {
    pub uid: String,
    /// `metadata.deletionTimestamp` set (park in flight / external delete).
    pub terminating: bool,
    /// Age of the Job (now - creationTimestamp).
    pub age: Duration,
    /// Message of a `Failed` condition on the Job (backoff limit, deadline).
    pub failed: Option<String>,
}

/// Facts about the Kueue Workload owned by the Job (matched via
/// `ownerReferences` UID). `None` in [`DeriveInput`] = no matching Workload in
/// the namespace.
#[derive(Clone, Debug, Default)]
pub struct WorkloadFacts {
    /// `Admitted` condition is True (quota reserved, admission checks passed).
    pub admitted: bool,
    /// `Evicted` condition is True: Kueue is stopping the run (preemption,
    /// deactivation, ...). Kueue flips it back to False on re-admission.
    pub evicted: bool,
    /// Reason on the `Evicted` condition (e.g. `Preempted`).
    pub evicted_reason: Option<String>,
    /// Kueue will never re-admit the Workload: `spec.active` is false
    /// (max-exec-time breach, manual deactivation) or the `Evicted` reason
    /// starts with `Deactivated`.
    pub deactivated: bool,
    /// `Finished` condition is True: the Job completed or failed.
    pub finished: bool,
    /// Time without quota: since `QuotaReserved` turned False, or since the
    /// Workload was created. Zero while quota is held.
    pub waiting: Duration,
    /// Most relevant condition message (Finished, else Evicted, else the
    /// pending reason), surfaced as the `detail` line.
    pub message: Option<String>,
}

/// Facts about the session's compute pod (the Job's pod,
/// `batch.kubernetes.io/job-name=<name>`).
#[derive(Clone, Debug)]
pub struct ComputePodFacts {
    pub name: String,
    pub uid: String,
    /// `status.phase == "Running"`.
    pub running: bool,
    /// The `Ready` pod condition is True (the readiness probe passes and the
    /// Service publishes the pod as an endpoint).
    pub ready: bool,
    /// `metadata.deletionTimestamp` set (Job suspended / deleted).
    pub terminating: bool,
    /// Any container waiting with reason `CrashLoopBackOff`.
    pub crash_loop_back_off: bool,
    /// Sum of container restart counts.
    pub restart_count: u32,
}

/// Broker liveness, from `ComputeBroker::status()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrokerFacts {
    pub connected: bool,
    pub worker_ok: bool,
}

/// Everything [`derive`] needs for one reconcile tick.
#[derive(Clone, Debug)]
pub struct DeriveInput {
    pub desired: Desired,
    pub job: Option<JobFacts>,
    pub workload: Option<WorkloadFacts>,
    pub compute_pod: Option<ComputePodFacts>,
    pub broker: BrokerFacts,
    /// A failure latched earlier in this episode (stays until a new dispatch
    /// clears it). Surfaced as `failure_reason` even once parked.
    pub latched_failure: Option<String>,
    pub admission_timeout: Duration,
    pub workload_grace: Duration,
    pub restart_limit: u32,
}

/// Pure output of one derivation.
#[derive(Clone, Debug)]
pub struct Derived {
    pub state: SessionState,
    /// Human-readable one-liner for the session console (`detail` field).
    pub detail: Option<String>,
    /// A NEWLY detected failure the controller must latch (progress Fail
    /// event + auto-park handling). Never set when `latched_failure` already
    /// fired — latching is the controller's job.
    pub new_failure: Option<String>,
    /// R1 watchdog: the Job exists past the grace window with no Workload —
    /// the controller must DELETE it immediately.
    pub watchdog_delete: bool,
}

impl Derived {
    fn simple(state: SessionState, detail: impl Into<Option<String>>) -> Self {
        Self {
            state,
            detail: detail.into(),
            new_failure: None,
            watchdog_delete: false,
        }
    }

    fn failed(reason: String) -> Self {
        Self {
            state: SessionState::Failed,
            detail: Some(reason.clone()),
            new_failure: Some(reason),
            watchdog_delete: false,
        }
    }
}

/// `detail` line while the Workload waits for (re-)admission.
fn queued_detail(w: &WorkloadFacts) -> String {
    let message = w.message.as_deref();
    if w.evicted {
        let reason = w.evicted_reason.as_deref().unwrap_or("evicted");
        return match message {
            Some(m) => format!("evicted by Kueue ({reason}); waiting for re-admission: {m}"),
            None => format!("evicted by Kueue ({reason}); waiting for re-admission"),
        };
    }
    message.unwrap_or("waiting for admission").to_string()
}

/// Derive the session state from one tick's facts. See the Q3 state table in
/// the design spec; every row is covered by a unit test below.
pub fn derive(input: &DeriveInput) -> Derived {
    let Some(job) = &input.job else {
        // Job absent.
        return match input.desired {
            Desired::Parked => Derived::simple(SessionState::Parked, None),
            // Create in flight, or the old Job's deletion has not completed
            // and we are between NotFound and create.
            Desired::Running => {
                Derived::simple(SessionState::Dispatching, "creating compute Job".to_string())
            }
        };
    };

    if job.terminating {
        return Derived::simple(
            SessionState::Parking,
            "Job deleting; waiting for finalizers".to_string(),
        );
    }

    // A latched failure holds while the Job still exists (the controller may
    // be keeping it for the crashloop log-grace window).
    if let Some(reason) = &input.latched_failure {
        return Derived::simple(SessionState::Failed, reason.clone());
    }

    // A Job-level Failed condition (backoffLimit exhausted, deadline) fails
    // the session regardless of admission phase.
    if let Some(reason) = &job.failed {
        return Derived::failed(format!("compute Job failed: {reason}"));
    }

    // A pod already being torn down (Job suspended or deleted) says nothing
    // about the run in progress.
    let pod = input.compute_pod.as_ref().filter(|p| !p.terminating);

    // Crash-looping compute pod fails the session regardless of admission phase.
    if let Some(pod) = pod {
        if pod.crash_loop_back_off || pod.restart_count >= input.restart_limit {
            return Derived::failed(format!(
                "compute pod {} is crash-looping (restarts: {})",
                pod.name, pod.restart_count
            ));
        }
    }

    match &input.workload {
        Some(w) if w.finished => Derived::failed(format!(
            "Kueue finished the session workload: {}",
            w.message.as_deref().unwrap_or("the compute Job ended")
        )),
        Some(w) if w.deactivated => Derived::failed(format!(
            "Kueue deactivated the session workload: {}",
            w.message.as_deref().unwrap_or("no condition message")
        )),
        Some(w) if w.admitted && !w.evicted => match pod {
            Some(pod) if pod.running => {
                if input.broker.connected && input.broker.worker_ok {
                    Derived::simple(SessionState::Ready, None)
                } else if pod.ready {
                    Derived::simple(
                        SessionState::Starting,
                        "compute pod ready; waiting for the worker".to_string(),
                    )
                } else {
                    Derived::simple(
                        SessionState::Starting,
                        "compute pod running; waiting for it to become ready".to_string(),
                    )
                }
            }
            _ => Derived::simple(
                SessionState::Admitted,
                "admitted; waiting for the compute pod".to_string(),
            ),
        },
        Some(w) => {
            // No quota yet, or Kueue evicted the run (preemption) and will
            // re-admit it: the Job is suspended and the session queues again.
            if w.waiting > input.admission_timeout {
                Derived::failed(format!(
                    "admission timeout after {}s: {}",
                    input.admission_timeout.as_secs(),
                    w.message
                        .as_deref()
                        .unwrap_or("no condition message from Kueue")
                ))
            } else {
                Derived::simple(SessionState::Queued, queued_detail(w))
            }
        }
        None => {
            // R1 watchdog: no Workload owned by the Job within the grace
            // window means Kueue is absent/misconfigured.
            if job.age > input.workload_grace {
                let mut d = Derived::failed(format!(
                    "no Kueue Workload appeared within {}s of Job create \
                     (is Kueue installed and the queue label valid?); deleting the Job",
                    input.workload_grace.as_secs()
                ));
                d.watchdog_delete = true;
                d
            } else {
                Derived::simple(
                    SessionState::Queued,
                    "waiting for Kueue to create a Workload".to_string(),
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fact builders: Kubernetes objects -> the pure structs above.
// ---------------------------------------------------------------------------

fn elapsed(now: Timestamp, then: Timestamp) -> Duration {
    Duration::try_from(now.duration_since(then)).unwrap_or_default()
}

fn conditions(data: &Value) -> &[Value] {
    data.pointer("/status/conditions")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

fn condition<'a>(conditions: &'a [Value], ty: &str) -> Option<&'a Value> {
    conditions
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some(ty))
}

fn is_true(condition: Option<&Value>) -> bool {
    condition
        .and_then(|c| c.get("status"))
        .and_then(Value::as_str)
        == Some("True")
}

fn field(condition: Option<&Value>, key: &str) -> Option<String> {
    condition
        .and_then(|c| c.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Facts from the session Job object. `now` is injected for testability.
pub fn job_facts(job: &DynamicObject, now: Timestamp) -> anyhow::Result<JobFacts> {
    let uid = job
        .metadata
        .uid
        .clone()
        .ok_or_else(|| anyhow!("Job has no uid"))?;
    let failed = condition(conditions(&job.data), "Failed")
        .filter(|c| is_true(Some(c)))
        .map(|c| {
            field(Some(c), "message")
                .or_else(|| field(Some(c), "reason"))
                .unwrap_or_else(|| "Job failed".to_string())
        });
    Ok(JobFacts {
        uid,
        terminating: job.metadata.deletion_timestamp.is_some(),
        age: job
            .metadata
            .creation_timestamp
            .as_ref()
            .map(|t| elapsed(now, t.0))
            .unwrap_or_default(),
        failed,
    })
}

/// The Kueue Workload owned by the Job with UID `owner_uid`, if any.
pub fn owned_workload<'a>(
    workloads: &'a [DynamicObject],
    owner_uid: &str,
) -> Option<&'a DynamicObject> {
    workloads.iter().find(|wl| {
        wl.metadata
            .owner_references
            .as_ref()
            .is_some_and(|refs| refs.iter().any(|r| r.uid == owner_uid))
    })
}

/// Admission facts from a Workload's `spec.active` and `status.conditions`
/// (`QuotaReserved`, `Admitted`, `Evicted`, `Finished`). `now` is injected
/// for testability.
pub fn workload_facts(wl: &DynamicObject, now: Timestamp) -> WorkloadFacts {
    let conds = conditions(&wl.data);
    let quota = condition(conds, "QuotaReserved");
    let admitted = condition(conds, "Admitted");
    let evicted = condition(conds, "Evicted");
    let finished = condition(conds, "Finished");

    let evicted_now = is_true(evicted);
    let evicted_reason = if evicted_now {
        field(evicted, "reason")
    } else {
        None
    };
    let spec_inactive = wl.data.pointer("/spec/active").and_then(Value::as_bool) == Some(false);
    let deactivated = spec_inactive
        || evicted_reason
            .as_deref()
            .is_some_and(|r| r.starts_with("Deactivated"));

    // Time since the Workload last turned not-admitted (quota lost, or admission
    // checks still pending), else since it was created.
    let waiting = if is_true(admitted) {
        Duration::ZERO
    } else {
        [admitted, quota]
            .into_iter()
            .find_map(|c| field(c, "lastTransitionTime"))
            .and_then(|t| t.parse::<Timestamp>().ok())
            .or_else(|| wl.metadata.creation_timestamp.as_ref().map(|t| t.0))
            .map(|since| elapsed(now, since))
            .unwrap_or_default()
    };

    let finished_now = is_true(finished);
    let message = if finished_now {
        field(finished, "message")
    } else if evicted_now {
        field(evicted, "message")
    } else if is_true(quota) {
        // Quota held but not yet admitted: the admission-check wait.
        field(admitted, "message")
    } else {
        field(quota, "message").or_else(|| field(admitted, "message"))
    };

    WorkloadFacts {
        admitted: is_true(admitted),
        evicted: evicted_now,
        evicted_reason,
        deactivated,
        finished: finished_now,
        waiting,
        message,
    }
}

/// Extract compute-pod facts from a typed Pod.
pub fn compute_pod_facts(pod: &Pod) -> ComputePodFacts {
    let status = pod.status.as_ref();
    let running = status
        .and_then(|s| s.phase.as_deref())
        .is_some_and(|p| p == "Running");
    let ready = status
        .and_then(|s| s.conditions.as_ref())
        .into_iter()
        .flatten()
        .any(|c| c.type_ == "Ready" && c.status == "True");
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
    ComputePodFacts {
        name: pod.metadata.name.clone().unwrap_or_default(),
        uid: pod.metadata.uid.clone().unwrap_or_default(),
        running,
        ready,
        terminating: pod.metadata.deletion_timestamp.is_some(),
        crash_loop_back_off,
        restart_count,
    }
}

/// The pod that represents the run in progress: a live pod over a
/// terminating or finished one, newest first. Several pods coexist briefly
/// when the Job is re-admitted while the previous pod is still terminating.
pub fn pick_current_pod(pods: &[Pod]) -> Option<&Pod> {
    pods.iter().max_by_key(|p| {
        let terminating = p.metadata.deletion_timestamp.is_some();
        let finished = matches!(
            p.status.as_ref().and_then(|s| s.phase.as_deref()),
            Some("Succeeded" | "Failed")
        );
        let created = p.metadata.creation_timestamp.as_ref().map(|t| t.0);
        (!terminating, !finished, created)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{
        ContainerState, ContainerStateWaiting, ContainerStatus, PodCondition, PodStatus,
    };
    use serde_json::json;

    const TIMEOUT: Duration = Duration::from_secs(600);

    fn base() -> DeriveInput {
        DeriveInput {
            desired: Desired::Parked,
            job: None,
            workload: None,
            compute_pod: None,
            broker: BrokerFacts::default(),
            latched_failure: None,
            admission_timeout: TIMEOUT,
            workload_grace: WORKLOAD_GRACE,
            restart_limit: RESTART_LIMIT,
        }
    }

    fn job(age_secs: u64) -> Option<JobFacts> {
        Some(JobFacts {
            uid: "uid-1".into(),
            terminating: false,
            age: Duration::from_secs(age_secs),
            failed: None,
        })
    }

    fn workload(admitted: bool) -> Option<WorkloadFacts> {
        Some(WorkloadFacts {
            admitted,
            ..Default::default()
        })
    }

    fn compute_pod(running: bool) -> Option<ComputePodFacts> {
        Some(ComputePodFacts {
            name: "jump-cannon-compute-abcde".into(),
            uid: "pod-uid-1".into(),
            running,
            ready: running,
            terminating: false,
            crash_loop_back_off: false,
            restart_count: 0,
        })
    }

    fn running_input() -> DeriveInput {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(120);
        i.workload = workload(true);
        i
    }

    #[test]
    fn parked_when_job_absent_and_desired_parked() {
        let d = derive(&base());
        assert_eq!(d.state, SessionState::Parked);
        assert!(d.new_failure.is_none());
    }

    #[test]
    fn dispatching_when_desired_running_and_job_absent() {
        let mut i = base();
        i.desired = Desired::Running;
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Dispatching);
    }

    #[test]
    fn parking_when_job_terminating() {
        let mut i = base();
        i.job = job(10).map(|mut j| {
            j.terminating = true;
            j
        });
        assert_eq!(derive(&i).state, SessionState::Parking);
        // Desired=Running during parking still reads as parking (the state
        // machine absorbs AlreadyExists until the old Job is NotFound).
        i.desired = Desired::Running;
        assert_eq!(derive(&i).state, SessionState::Parking);
    }

    #[test]
    fn queued_within_workload_grace() {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(30);
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Queued);
        assert!(!d.watchdog_delete);
    }

    #[test]
    fn watchdog_fires_after_workload_grace() {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(WORKLOAD_GRACE.as_secs() + 1);
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.watchdog_delete, "controller must delete the orphaned Job");
        assert!(d.new_failure.expect("latched").contains("Workload"));
    }

    #[test]
    fn job_failed_condition_fails() {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(30).map(|mut j| {
            j.failed = Some("Job has reached the specified backoff limit".into());
            j
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.new_failure.expect("latched").contains("backoff"));
    }

    #[test]
    fn queued_with_detail_while_unadmitted() {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(30);
        i.workload = Some(WorkloadFacts {
            waiting: Duration::from_secs(30),
            message: Some("0/1 nvidia.com/gpu available".into()),
            ..Default::default()
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Queued);
        assert_eq!(d.detail.as_deref(), Some("0/1 nvidia.com/gpu available"));
    }

    #[test]
    fn admission_timeout_fails_with_message() {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(TIMEOUT.as_secs() + 1);
        i.workload = Some(WorkloadFacts {
            waiting: TIMEOUT + Duration::from_secs(1),
            message: Some("quota exhausted".into()),
            ..Default::default()
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.new_failure.expect("latched").contains("quota exhausted"));
        assert!(!d.watchdog_delete);
    }

    #[test]
    fn admission_timeout_counts_time_without_quota_not_job_age() {
        // An hours-old session that was just preempted has waited only
        // seconds for re-admission: it must queue, not time out.
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(3 * 3600);
        i.workload = Some(WorkloadFacts {
            evicted: true,
            evicted_reason: Some("Preempted".into()),
            waiting: Duration::from_secs(20),
            ..Default::default()
        });
        assert_eq!(derive(&i).state, SessionState::Queued);
    }

    #[test]
    fn preemption_requeues_instead_of_failing() {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(3600);
        i.compute_pod = compute_pod(true);
        i.broker = BrokerFacts {
            connected: true,
            worker_ok: true,
        };
        i.workload = Some(WorkloadFacts {
            evicted: true,
            evicted_reason: Some("Preempted".into()),
            waiting: Duration::from_secs(5),
            message: Some("Preempted to accommodate a higher-priority workload".into()),
            ..Default::default()
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Queued);
        assert!(d.new_failure.is_none(), "preemption is not a failure");
        let detail = d.detail.expect("detail");
        assert!(detail.contains("Preempted"), "{detail}");
        assert!(detail.contains("higher-priority"), "{detail}");
    }

    #[test]
    fn eviction_with_quota_still_held_reads_queued() {
        // Kueue marks Evicted before the Job stops and releases quota; the
        // session must not keep reporting ready/admitted through that window.
        let mut i = running_input();
        i.compute_pod = compute_pod(true);
        i.workload = Some(WorkloadFacts {
            admitted: true,
            evicted: true,
            evicted_reason: Some("Preempted".into()),
            ..Default::default()
        });
        assert_eq!(derive(&i).state, SessionState::Queued);
    }

    #[test]
    fn deactivation_fails() {
        let mut i = base();
        i.desired = Desired::Running;
        i.job = job(120);
        i.workload = Some(WorkloadFacts {
            evicted: true,
            evicted_reason: Some("DeactivatedDueToMaximumExecutionTimeExceeded".into()),
            deactivated: true,
            message: Some("exceeded the maximum execution time".into()),
            ..Default::default()
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        let reason = d.new_failure.expect("latched");
        assert!(reason.contains("deactivated"), "{reason}");
        assert!(reason.contains("maximum execution time"), "{reason}");
    }

    #[test]
    fn finished_workload_fails() {
        let mut i = running_input();
        i.compute_pod = compute_pod(true);
        i.workload = Some(WorkloadFacts {
            admitted: true,
            finished: true,
            message: Some("Job finished successfully".into()),
            ..Default::default()
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.new_failure.expect("latched").contains("finished"));
    }

    #[test]
    fn admitted_until_compute_pod_runs() {
        let mut i = running_input();
        assert_eq!(derive(&i).state, SessionState::Admitted);
        i.compute_pod = compute_pod(false);
        assert_eq!(derive(&i).state, SessionState::Admitted);
    }

    #[test]
    fn terminating_pod_does_not_count_as_running() {
        let mut i = running_input();
        i.compute_pod = compute_pod(true).map(|mut p| {
            p.terminating = true;
            p
        });
        assert_eq!(derive(&i).state, SessionState::Admitted);
    }

    #[test]
    fn terminating_pod_crash_loop_is_ignored() {
        let mut i = running_input();
        i.compute_pod = compute_pod(false).map(|mut p| {
            p.terminating = true;
            p.crash_loop_back_off = true;
            p.restart_count = RESTART_LIMIT;
            p
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Admitted);
        assert!(d.new_failure.is_none());
    }

    #[test]
    fn starting_until_broker_healthy() {
        let mut i = running_input();
        i.compute_pod = compute_pod(true);
        assert_eq!(derive(&i).state, SessionState::Starting);
        i.broker.connected = true;
        assert_eq!(derive(&i).state, SessionState::Starting);
    }

    #[test]
    fn starting_detail_tracks_pod_readiness() {
        let mut i = running_input();
        i.compute_pod = compute_pod(true).map(|mut p| {
            p.ready = false;
            p
        });
        let detail = derive(&i).detail.expect("detail");
        assert!(detail.contains("become ready"), "{detail}");
        i.compute_pod = compute_pod(true);
        let detail = derive(&i).detail.expect("detail");
        assert!(detail.contains("waiting for the worker"), "{detail}");
    }

    #[test]
    fn ready_when_broker_connected_and_worker_ok() {
        let mut i = running_input();
        i.compute_pod = compute_pod(true);
        i.broker = BrokerFacts {
            connected: true,
            worker_ok: true,
        };
        assert_eq!(derive(&i).state, SessionState::Ready);
    }

    #[test]
    fn crashloop_fails_regardless_of_phase() {
        let mut i = running_input();
        i.compute_pod = compute_pod(false).map(|mut p| {
            p.crash_loop_back_off = true;
            p
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.new_failure.expect("latched").contains("crash-looping"));
    }

    #[test]
    fn restart_limit_fails() {
        let mut i = running_input();
        i.compute_pod = compute_pod(true).map(|mut p| {
            p.restart_count = RESTART_LIMIT;
            p
        });
        assert_eq!(derive(&i).state, SessionState::Failed);
    }

    #[test]
    fn latched_failure_holds_while_job_exists() {
        let mut i = running_input();
        i.compute_pod = compute_pod(true);
        i.broker = BrokerFacts {
            connected: true,
            worker_ok: true,
        };
        i.latched_failure = Some("earlier failure".into());
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.new_failure.is_none(), "no double-latch");
        assert_eq!(d.detail.as_deref(), Some("earlier failure"));
    }

    #[test]
    fn parked_after_external_delete_surfaces_latched_reason() {
        // Janitor/max-exec-time delete: Job NotFound + desired Parked → parked;
        // the controller keeps the latched reason for `failure_reason`.
        let mut i = base();
        i.latched_failure = Some("Kueue deactivated the session workload".into());
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Parked);
    }

    // -- fact builders ------------------------------------------------------

    fn now() -> Timestamp {
        "2026-01-01T01:00:00Z".parse().expect("timestamp")
    }

    fn dynamic(value: Value) -> DynamicObject {
        serde_json::from_value(value).expect("dynamic object")
    }

    fn workload_obj(spec: Value, conditions: Value) -> DynamicObject {
        dynamic(json!({
            "apiVersion": "kueue.x-k8s.io/v1beta2",
            "kind": "Workload",
            "metadata": {
                "name": "job-jump-cannon-compute-1a2b3",
                "namespace": "gpu-workloads",
                "creationTimestamp": "2026-01-01T00:50:00Z",
                "ownerReferences": [{
                    "apiVersion": "batch/v1",
                    "kind": "Job",
                    "name": "jump-cannon-compute",
                    "uid": "job-uid-1"
                }]
            },
            "spec": spec,
            "status": { "conditions": conditions }
        }))
    }

    #[test]
    fn workload_facts_pending_measures_wait_from_quota_loss() {
        let wl = workload_obj(
            json!({ "active": true }),
            json!([{
                "type": "QuotaReserved",
                "status": "False",
                "reason": "Pending",
                "message": "couldn't assign flavors: insufficient quota for nvidia.com/gpu",
                "lastTransitionTime": "2026-01-01T00:55:00Z"
            }]),
        );
        let f = workload_facts(&wl, now());
        assert!(!f.admitted && !f.evicted && !f.deactivated && !f.finished);
        assert_eq!(f.waiting, Duration::from_secs(300));
        assert_eq!(
            f.message.as_deref(),
            Some("couldn't assign flavors: insufficient quota for nvidia.com/gpu")
        );
    }

    #[test]
    fn workload_facts_without_conditions_waits_since_creation() {
        let wl = workload_obj(json!({}), json!([]));
        let f = workload_facts(&wl, now());
        assert!(!f.admitted);
        assert_eq!(f.waiting, Duration::from_secs(600));
        assert!(f.message.is_none());
    }

    #[test]
    fn workload_facts_admitted_has_no_wait() {
        let wl = workload_obj(
            json!({ "active": true }),
            json!([
                { "type": "QuotaReserved", "status": "True",
                  "lastTransitionTime": "2026-01-01T00:51:00Z" },
                { "type": "Admitted", "status": "True",
                  "lastTransitionTime": "2026-01-01T00:51:00Z" }
            ]),
        );
        let f = workload_facts(&wl, now());
        assert!(f.admitted);
        assert_eq!(f.waiting, Duration::ZERO);
    }

    #[test]
    fn workload_facts_quota_held_but_unadmitted_waits_on_admission_checks() {
        let wl = workload_obj(
            json!({ "active": true }),
            json!([
                { "type": "QuotaReserved", "status": "True",
                  "message": "Quota reserved in ClusterQueue gpu",
                  "lastTransitionTime": "2026-01-01T00:51:00Z" },
                { "type": "Admitted", "status": "False", "reason": "UnsatisfiedChecks",
                  "message": "The workload has not all checks ready",
                  "lastTransitionTime": "2026-01-01T00:52:00Z" }
            ]),
        );
        let f = workload_facts(&wl, now());
        assert!(!f.admitted);
        assert_eq!(f.waiting, Duration::from_secs(480));
        assert_eq!(f.message.as_deref(), Some("The workload has not all checks ready"));
    }

    #[test]
    fn workload_facts_preemption_is_evicted_but_not_deactivated() {
        let wl = workload_obj(
            json!({ "active": true }),
            json!([
                { "type": "QuotaReserved", "status": "False", "reason": "Pending",
                  "message": "waiting for quota",
                  "lastTransitionTime": "2026-01-01T00:58:00Z" },
                { "type": "Admitted", "status": "False" },
                { "type": "Evicted", "status": "True", "reason": "Preempted",
                  "message": "Preempted to accommodate a workload" }
            ]),
        );
        let f = workload_facts(&wl, now());
        assert!(f.evicted);
        assert_eq!(f.evicted_reason.as_deref(), Some("Preempted"));
        assert!(!f.deactivated);
        assert_eq!(f.waiting, Duration::from_secs(120));
        // Eviction text outranks the pending text.
        assert_eq!(f.message.as_deref(), Some("Preempted to accommodate a workload"));
    }

    #[test]
    fn workload_facts_inactive_spec_is_deactivated() {
        let wl = workload_obj(
            json!({ "active": false }),
            json!([{ "type": "Evicted", "status": "True",
                     "reason": "DeactivatedDueToMaximumExecutionTimeExceeded",
                     "message": "exceeded the maximum execution time" }]),
        );
        let f = workload_facts(&wl, now());
        assert!(f.evicted && f.deactivated);
    }

    #[test]
    fn workload_facts_deactivated_reason_alone_is_deactivated() {
        let wl = workload_obj(
            json!({}),
            json!([{ "type": "Evicted", "status": "True", "reason": "Deactivated" }]),
        );
        assert!(workload_facts(&wl, now()).deactivated);
    }

    #[test]
    fn workload_facts_cleared_eviction_is_not_evicted() {
        // After re-admission Kueue sets Evicted=False ("Previously: ...").
        let wl = workload_obj(
            json!({ "active": true }),
            json!([
                { "type": "QuotaReserved", "status": "True" },
                { "type": "Admitted", "status": "True" },
                { "type": "Evicted", "status": "False", "reason": "QuotaReserved",
                  "message": "Previously: Preempted to accommodate a workload" }
            ]),
        );
        let f = workload_facts(&wl, now());
        assert!(f.admitted && !f.evicted);
        assert!(f.evicted_reason.is_none());
    }

    #[test]
    fn workload_facts_finished_message_wins() {
        let wl = workload_obj(
            json!({ "active": true }),
            json!([
                { "type": "Admitted", "status": "True", "message": "admitted" },
                { "type": "Finished", "status": "True", "reason": "Failed",
                  "message": "Job failed" }
            ]),
        );
        let f = workload_facts(&wl, now());
        assert!(f.finished);
        assert_eq!(f.message.as_deref(), Some("Job failed"));
    }

    #[test]
    fn owned_workload_matches_owner_uid() {
        let owned = workload_obj(json!({}), json!([]));
        let mut other = workload_obj(json!({}), json!([]));
        other.metadata.owner_references = None;
        let items = vec![other, owned];
        assert!(owned_workload(&items, "job-uid-1").is_some());
        assert!(owned_workload(&items, "someone-else").is_none());
    }

    fn job_obj(extra: Value) -> DynamicObject {
        let mut v = json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {
                "name": "jump-cannon-compute",
                "namespace": "gpu-workloads",
                "uid": "job-uid-1",
                "creationTimestamp": "2026-01-01T00:59:00Z"
            }
        });
        if let (Some(obj), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
            for (k, val) in extra {
                obj.insert(k.clone(), val.clone());
            }
        }
        dynamic(v)
    }

    #[test]
    fn job_facts_reads_age_and_failed_condition() {
        let f = job_facts(&job_obj(json!({})), now()).expect("facts");
        assert_eq!(f.uid, "job-uid-1");
        assert_eq!(f.age, Duration::from_secs(60));
        assert!(!f.terminating);
        assert!(f.failed.is_none());

        let failed = job_obj(json!({ "status": { "conditions": [
            { "type": "Failed", "status": "True", "reason": "BackoffLimitExceeded",
              "message": "Job has reached the specified backoff limit" }
        ]}}));
        let f = job_facts(&failed, now()).expect("facts");
        assert_eq!(f.failed.as_deref(), Some("Job has reached the specified backoff limit"));

        // A Failed condition that is not True is not a failure.
        let not_failed = job_obj(json!({ "status": { "conditions": [
            { "type": "Failed", "status": "False" }
        ]}}));
        assert!(job_facts(&not_failed, now()).expect("facts").failed.is_none());
    }

    #[test]
    fn job_facts_terminating_and_missing_uid() {
        let mut obj = job_obj(json!({}));
        obj.metadata.deletion_timestamp = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(now()));
        assert!(job_facts(&obj, now()).expect("facts").terminating);

        let mut no_uid = job_obj(json!({}));
        no_uid.metadata.uid = None;
        assert!(job_facts(&no_uid, now()).is_err());
    }

    fn pod(name: &str, phase: &str, created: &str) -> Pod {
        Pod {
            metadata: kube::api::ObjectMeta {
                name: Some(name.into()),
                uid: Some(format!("uid-{name}")),
                creation_timestamp: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    created.parse().expect("timestamp"),
                )),
                ..Default::default()
            },
            status: Some(PodStatus {
                phase: Some(phase.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn with_container(mut pod: Pod, waiting: Option<&str>, restarts: i32, ready: bool) -> Pod {
        let status = pod.status.get_or_insert_with(Default::default);
        status.container_statuses = Some(vec![ContainerStatus {
            name: "graph-compute".into(),
            ready,
            restart_count: restarts,
            image: "img".into(),
            image_id: String::new(),
            state: Some(ContainerState {
                waiting: waiting.map(|r| ContainerStateWaiting {
                    reason: Some(r.into()),
                    message: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }]);
        status.conditions = Some(vec![PodCondition {
            type_: "Ready".into(),
            status: if ready { "True" } else { "False" }.into(),
            ..Default::default()
        }]);
        pod
    }

    #[test]
    fn compute_pod_facts_running_ready() {
        let p = with_container(pod("compute-a", "Running", "2026-01-01T00:00:00Z"), None, 0, true);
        let f = compute_pod_facts(&p);
        assert!(f.running && f.ready && !f.terminating);
        assert!(!f.crash_loop_back_off);
        assert_eq!(f.restart_count, 0);
        assert_eq!(f.name, "compute-a");
        assert_eq!(f.uid, "uid-compute-a");
    }

    #[test]
    fn compute_pod_facts_running_not_ready() {
        let p = with_container(pod("compute-a", "Running", "2026-01-01T00:00:00Z"), None, 0, false);
        let f = compute_pod_facts(&p);
        assert!(f.running);
        assert!(!f.ready);
    }

    #[test]
    fn compute_pod_facts_crashloop() {
        let p = with_container(
            pod("compute-a", "Pending", "2026-01-01T00:00:00Z"),
            Some("CrashLoopBackOff"),
            7,
            false,
        );
        let f = compute_pod_facts(&p);
        assert!(!f.running);
        assert!(f.crash_loop_back_off);
        assert_eq!(f.restart_count, 7);
    }

    #[test]
    fn compute_pod_facts_terminating() {
        let mut p = pod("compute-a", "Running", "2026-01-01T00:00:00Z");
        p.metadata.deletion_timestamp = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(now()));
        assert!(compute_pod_facts(&p).terminating);
    }

    #[test]
    fn pick_current_pod_prefers_live_then_newest() {
        let old_running = pod("old", "Running", "2026-01-01T00:00:00Z");
        let new_pending = pod("new", "Pending", "2026-01-01T00:30:00Z");
        let mut terminating_newest = pod("dying", "Running", "2026-01-01T00:45:00Z");
        terminating_newest.metadata.deletion_timestamp =
            Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(now()));
        let failed_newest = pod("failed", "Failed", "2026-01-01T00:50:00Z");

        let pods = vec![
            old_running.clone(),
            terminating_newest.clone(),
            failed_newest.clone(),
            new_pending.clone(),
        ];
        assert_eq!(
            pick_current_pod(&pods).and_then(|p| p.metadata.name.as_deref()),
            Some("new")
        );
        // Only dead pods left: still returns one so the tailer can read it.
        let dead = vec![terminating_newest, failed_newest];
        assert_eq!(
            pick_current_pod(&dead).and_then(|p| p.metadata.name.as_deref()),
            Some("failed")
        );
        assert!(pick_current_pod(&[]).is_none());
    }
}
