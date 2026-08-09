//! Session state derivation (Q3). The observed state is derived entirely from
//! cluster objects (RayCluster CR, Kueue Workload, head pod) plus broker
//! status — never persisted — so graph-api restarts and external deletions
//! self-heal.
//!
//! [`derive`] is a pure function over fabricated facts so the whole state
//! table is unit-testable without a cluster; the IO half lives in
//! `gpu_session::mod` (kube dynamic API reads) and only assembles the fact
//! structs below.

use std::time::Duration;

use super::{Desired, SessionState};

/// How long after RayCluster create we wait for Kueue's webhook to spawn the
/// owning Workload before concluding the webhook is absent/misconfigured (R1)
/// and deleting the CR (an ungated Ray would start unbilled).
pub const WORKLOAD_GRACE: Duration = Duration::from_secs(60);

/// Head-pod restart count at or above which the session is latched failed.
pub const RESTART_LIMIT: u32 = 5;

/// Facts about the session RayCluster CR. `None` in [`DeriveInput`] means the
/// GET returned NotFound.
#[derive(Clone, Debug)]
pub struct RayClusterFacts {
    pub uid: String,
    /// `metadata.deletionTimestamp` set (park in flight / external delete).
    pub terminating: bool,
    /// Age of the CR (now - creationTimestamp).
    pub age: Duration,
}

/// Facts about the Kueue Workload owned by the RayCluster CR (matched via
/// `ownerReferences` UID). `None` = no matching Workload in the namespace.
#[derive(Clone, Debug, Default)]
pub struct WorkloadFacts {
    /// `Admitted` condition is True.
    pub admitted: bool,
    /// `Evicted` condition is True.
    pub evicted: bool,
    /// Most recent condition message (surfaced as the `detail` line).
    pub message: Option<String>,
}

/// Facts about the Ray head pod (labels `ray.io/cluster=<name>`,
/// `ray.io/node-type=head`).
#[derive(Clone, Debug)]
pub struct HeadPodFacts {
    pub name: String,
    pub uid: String,
    /// `status.phase == "Running"`.
    pub running: bool,
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
    pub raycluster: Option<RayClusterFacts>,
    pub workload: Option<WorkloadFacts>,
    pub head_pod: Option<HeadPodFacts>,
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
    /// R1 watchdog: the CR exists past the grace window with no Workload —
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

/// Derive the session state from one tick's facts. See the Q3 state table in
/// the design spec; every row is covered by a unit test below.
pub fn derive(input: &DeriveInput) -> Derived {
    let Some(rc) = &input.raycluster else {
        // CR absent.
        return match input.desired {
            Desired::Parked => Derived::simple(SessionState::Parked, None),
            // Create in flight, or the old CR's deletion (incl. finalizers)
            // has not completed and we are between NotFound and create.
            Desired::Running => {
                Derived::simple(SessionState::Dispatching, "creating RayCluster".to_string())
            }
        };
    };

    if rc.terminating {
        return Derived::simple(
            SessionState::Parking,
            "RayCluster deleting; waiting for finalizers".to_string(),
        );
    }

    // A latched failure holds while the CR still exists (the controller may
    // be keeping it for the crashloop log-grace window).
    if let Some(reason) = &input.latched_failure {
        return Derived::simple(SessionState::Failed, reason.clone());
    }

    // Crash-looping head pod fails the session regardless of admission phase.
    if let Some(pod) = &input.head_pod {
        if pod.crash_loop_back_off || pod.restart_count >= input.restart_limit {
            return Derived::failed(format!(
                "head pod {} is crash-looping (restarts: {})",
                pod.name, pod.restart_count
            ));
        }
    }

    match &input.workload {
        Some(w) if w.evicted => Derived::failed(format!(
            "Kueue evicted the session workload: {}",
            w.message.as_deref().unwrap_or("no condition message")
        )),
        Some(w) if w.admitted => match &input.head_pod {
            Some(pod) if pod.running => {
                if input.broker.connected && input.broker.worker_ok {
                    Derived::simple(SessionState::Ready, None)
                } else {
                    Derived::simple(
                        SessionState::HeadStarting,
                        "head pod running; waiting for the compute worker".to_string(),
                    )
                }
            }
            _ => Derived::simple(
                SessionState::Admitted,
                "admitted; waiting for the head pod".to_string(),
            ),
        },
        Some(w) => {
            // Workload exists but is not (yet) admitted.
            if rc.age > input.admission_timeout {
                Derived::failed(format!(
                    "admission timeout after {}s: {}",
                    input.admission_timeout.as_secs(),
                    w.message
                        .as_deref()
                        .unwrap_or("no condition message from Kueue")
                ))
            } else {
                Derived::simple(
                    SessionState::Queued,
                    w.message
                        .clone()
                        .unwrap_or_else(|| "waiting for admission".to_string()),
                )
            }
        }
        None => {
            // R1 watchdog: no Workload owned by the CR within the grace
            // window means the Kueue webhook is absent/misconfigured.
            if rc.age > input.workload_grace {
                let mut d = Derived::failed(format!(
                    "no Kueue Workload appeared within {}s of RayCluster create \
                     (is the kueue webhook installed?); deleting the cluster",
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

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(600);

    fn base() -> DeriveInput {
        DeriveInput {
            desired: Desired::Parked,
            raycluster: None,
            workload: None,
            head_pod: None,
            broker: BrokerFacts::default(),
            latched_failure: None,
            admission_timeout: TIMEOUT,
            workload_grace: WORKLOAD_GRACE,
            restart_limit: RESTART_LIMIT,
        }
    }

    fn rc(age_secs: u64) -> Option<RayClusterFacts> {
        Some(RayClusterFacts {
            uid: "uid-1".into(),
            terminating: false,
            age: Duration::from_secs(age_secs),
        })
    }

    fn workload(admitted: bool) -> Option<WorkloadFacts> {
        Some(WorkloadFacts {
            admitted,
            evicted: false,
            message: None,
        })
    }

    fn head_pod(running: bool) -> Option<HeadPodFacts> {
        Some(HeadPodFacts {
            name: "jump-cannon-compute-head-abc".into(),
            uid: "pod-uid-1".into(),
            running,
            crash_loop_back_off: false,
            restart_count: 0,
        })
    }

    #[test]
    fn parked_when_cr_absent_and_desired_parked() {
        let d = derive(&base());
        assert_eq!(d.state, SessionState::Parked);
        assert!(d.new_failure.is_none());
    }

    #[test]
    fn dispatching_when_desired_running_and_cr_absent() {
        let mut i = base();
        i.desired = Desired::Running;
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Dispatching);
    }

    #[test]
    fn parking_when_cr_terminating() {
        let mut i = base();
        i.raycluster = rc(10).map(|mut r| {
            r.terminating = true;
            r
        });
        assert_eq!(derive(&i).state, SessionState::Parking);
        // Desired=Running during parking still reads as parking (the state
        // machine absorbs AlreadyExists until the old CR is NotFound).
        i.desired = Desired::Running;
        assert_eq!(derive(&i).state, SessionState::Parking);
    }

    #[test]
    fn queued_within_workload_grace() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(30);
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Queued);
        assert!(!d.watchdog_delete);
    }

    #[test]
    fn watchdog_fires_after_workload_grace() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(WORKLOAD_GRACE.as_secs() + 1);
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.watchdog_delete, "controller must delete the ungated CR");
        assert!(d.new_failure.expect("latched").contains("Workload"));
    }

    #[test]
    fn queued_with_detail_while_unadmitted() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(30);
        i.workload = Some(WorkloadFacts {
            admitted: false,
            evicted: false,
            message: Some("0/1 nvidia.com/gpu available".into()),
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Queued);
        assert_eq!(d.detail.as_deref(), Some("0/1 nvidia.com/gpu available"));
    }

    #[test]
    fn admission_timeout_fails_with_message() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(TIMEOUT.as_secs() + 1);
        i.workload = Some(WorkloadFacts {
            admitted: false,
            evicted: false,
            message: Some("quota exhausted".into()),
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(
            d.new_failure
                .expect("latched")
                .contains("quota exhausted")
        );
        assert!(!d.watchdog_delete);
    }

    #[test]
    fn eviction_fails() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(120);
        i.workload = Some(WorkloadFacts {
            admitted: true,
            evicted: true,
            message: Some("max exec time reached".into()),
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.new_failure.expect("latched").contains("evicted"));
    }

    #[test]
    fn admitted_until_head_pod_runs() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(120);
        i.workload = workload(true);
        assert_eq!(derive(&i).state, SessionState::Admitted);
        i.head_pod = head_pod(false);
        assert_eq!(derive(&i).state, SessionState::Admitted);
    }

    #[test]
    fn head_starting_until_broker_healthy() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(120);
        i.workload = workload(true);
        i.head_pod = head_pod(true);
        assert_eq!(derive(&i).state, SessionState::HeadStarting);
        i.broker.connected = true;
        assert_eq!(derive(&i).state, SessionState::HeadStarting);
    }

    #[test]
    fn ready_when_broker_connected_and_worker_ok() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(120);
        i.workload = workload(true);
        i.head_pod = head_pod(true);
        i.broker = BrokerFacts {
            connected: true,
            worker_ok: true,
        };
        assert_eq!(derive(&i).state, SessionState::Ready);
    }

    #[test]
    fn crashloop_fails_regardless_of_phase() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(120);
        i.workload = workload(true);
        i.head_pod = head_pod(false).map(|mut p| {
            p.crash_loop_back_off = true;
            p
        });
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Failed);
        assert!(d.new_failure.expect("latched").contains("crash-looping"));
    }

    #[test]
    fn restart_limit_fails() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(120);
        i.workload = workload(true);
        i.head_pod = head_pod(true).map(|mut p| {
            p.restart_count = RESTART_LIMIT;
            p
        });
        assert_eq!(derive(&i).state, SessionState::Failed);
    }

    #[test]
    fn latched_failure_holds_while_cr_exists() {
        let mut i = base();
        i.desired = Desired::Running;
        i.raycluster = rc(120);
        i.workload = workload(true);
        i.head_pod = head_pod(true);
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
        // Janitor/max-exec-time delete: CR NotFound + desired Parked → parked;
        // the controller keeps the latched reason for `failure_reason`.
        let mut i = base();
        i.latched_failure = Some("Kueue evicted the session workload".into());
        let d = derive(&i);
        assert_eq!(d.state, SessionState::Parked);
    }
}
