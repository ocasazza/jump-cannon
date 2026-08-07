//! Compute-worker broker.
//!
//! graph-api dials a `graph-compute` worker (default `http://[::1]:50051`,
//! override with `JUMP_CANNON_COMPUTE_URL`) and re-broadcasts each
//! `PositionDelta` it receives onto a `tokio::sync::broadcast` channel that
//! the WebSocket handler subscribes to.
//!
//! Boot semantics: the broadcast channel is created up front so the WS
//! endpoint never returns 503 for a transient worker outage. A background
//! reconnect task keeps the gRPC stream alive across worker restarts using
//! exponential backoff (1s → cap 30s, reset on a successful dial). This is
//! the SkyPilot-pod-restart story; see infra/sky/README.md.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tonic::transport::Channel;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::{
    GraphAttributes as ProtoGraphAttributes, HealthRequest, ListEnginesRequest, LoadGraphRequest,
    PositionDelta, SubscribeRequest,
};
use graph_compute::service::json_to_struct;
use graph_layouts::geometric::LensConfig;

#[derive(Clone)]
pub struct ComputeBroker {
    inner: Arc<Inner>,
}

/// Remote layout-engine selection forwarded to graph-compute on the
/// `Subscribe` request (ADR-002). `layout_id` is a registry key (empty ⇒ the
/// worker's startup default); `params` is the JSON-shaped engine settings
/// object (`None` ⇒ engine defaults), serialized on the wire as
/// `google.protobuf.Struct`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteLayout {
    pub layout_id: String,
    pub params: Option<serde_json::Value>,
    pub lens: Option<LensConfig>,
    pub attributes: Option<ProtoGraphAttributes>,
}

/// Authoritative singleton selection plus its monotonic control-plane token.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionState {
    pub layout: RemoteLayout,
    pub generation: u64,
}

/// Atomic snapshot of the broker's graph/selection mutation preconditions.
#[derive(Clone, Debug, PartialEq)]
pub struct BrokerControlState {
    pub graph_revision: u64,
    pub selection: SelectionState,
}

/// Result of an authoritative selection request. Identical retries are
/// successful without advancing the generation or restarting the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SelectionUpdate {
    pub generation: u64,
    pub changed: bool,
}

impl RemoteLayout {
    /// Build a selection from the env vars `main.rs` reads. Returns the
    /// default (empty) selection when unset, so existing single-engine
    /// deployments are unaffected.
    ///
    /// - `JUMP_CANNON_COMPUTE_LAYOUT_ID` — registry key.
    /// - `JUMP_CANNON_COMPUTE_LAYOUT_PARAMS` — a JSON object string.
    pub fn from_env() -> Self {
        let layout_id = std::env::var("JUMP_CANNON_COMPUTE_LAYOUT_ID").unwrap_or_default();
        let params = std::env::var("JUMP_CANNON_COMPUTE_LAYOUT_PARAMS")
            .ok()
            .and_then(|s| {
                serde_json::from_str::<serde_json::Value>(&s)
                    .map_err(|e| {
                        tracing::warn!(
                            "ignoring JUMP_CANNON_COMPUTE_LAYOUT_PARAMS (not valid JSON): {e}"
                        );
                        e
                    })
                    .ok()
            });
        Self {
            layout_id,
            params,
            lens: None,
            attributes: None,
        }
    }
}

struct Inner {
    /// `None` until the dial succeeds. After connect, holds the broadcast
    /// sender used to fan PositionDeltas out to WS clients.
    tx: tokio::sync::RwLock<Option<broadcast::Sender<PositionDelta>>>,
    /// Live status flag — flips `true` once a `Subscribe` stream is open
    /// and `false` when the inner loop breaks out (worker closed, error,
    /// dial failed). Exposed via `/compute/health` so the renderer can
    /// surface the back-half-of-the-chain liveness in the footer log.
    connected: std::sync::atomic::AtomicBool,
    /// Last-known URL the loop is dialing. Set on `connect()`; read by
    /// `/compute/health` and the one-shot `list_engines` dial.
    url: tokio::sync::RwLock<Option<String>>,
    /// The currently-selected remote layout (ADR-002), carrying `layout_id` +
    /// `params` + the resolved geometric `attributes`. Seeded by env via
    /// `connect_with`, replayed on every reconnect, and swapped by `reselect`.
    /// Exposed (its `layout_id`) as `active` on `/compute/engines`. Held behind
    /// a lock so the forwarder reads the live value on each resubscribe and
    /// `reselect` can update it from a handler without racing the loop.
    selection: tokio::sync::RwLock<SelectionState>,
    /// Graph revision graph-api most recently loaded into the worker. Forward
    /// subscriptions request it and reject frames for any other graph.
    expected_revision: std::sync::atomic::AtomicU64,
    /// Serializes graph loads/reseeds with authoritative selection changes.
    /// The worker repeats the same CAS under its graph-swap lock.
    control_plane: tokio::sync::Mutex<()>,
    /// Abort handle for the live forwarder task. `reselect` aborts the old
    /// task before spawning a new one so the previous Subscribe stream is
    /// torn down (no leak) and subsequent `/graph/layout/stream` frames
    /// come from the newly-selected engine.
    forwarder: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl ComputeBroker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                tx: tokio::sync::RwLock::new(None),
                connected: std::sync::atomic::AtomicBool::new(false),
                url: tokio::sync::RwLock::new(None),
                selection: tokio::sync::RwLock::new(SelectionState {
                    layout: RemoteLayout::default(),
                    generation: 0,
                }),
                expected_revision: std::sync::atomic::AtomicU64::new(0),
                control_plane: tokio::sync::Mutex::new(()),
                forwarder: tokio::sync::Mutex::new(None),
            }),
        }
    }

    pub async fn is_configured(&self) -> bool {
        self.inner.url.read().await.is_some()
    }

    pub async fn control_state(&self) -> BrokerControlState {
        let _control = self.inner.control_plane.lock().await;
        BrokerControlState {
            graph_revision: self.inner.expected_revision.load(Ordering::Acquire),
            selection: self.inner.selection.read().await.clone(),
        }
    }

    /// Snapshot of the broker and worker's effective runtime state. Worker
    /// health is bounded by a short timeout so polling cannot hang the API.
    pub async fn status(&self) -> BrokerStatus {
        let url = self.inner.url.read().await.clone().unwrap_or_default();
        let selection = self.inner.selection.read().await.clone();
        let mut status = BrokerStatus {
            connected: self
                .inner
                .connected
                .load(std::sync::atomic::Ordering::Relaxed),
            url: url.clone(),
            worker_ok: false,
            requested_layout_id: selection.layout.layout_id,
            effective_layout_id: String::new(),
            fallback_reason: None,
            worker_status_error: None,
            selection_generation: selection.generation,
            graph_revision: self.inner.expected_revision.load(Ordering::Acquire),
        };
        if url.is_empty() {
            status.worker_status_error = Some("compute worker not configured".into());
            return status;
        }

        let health = tokio::time::timeout(Duration::from_millis(750), async {
            let mut client = ComputeClient::connect(url)
                .await
                .map_err(anyhow::Error::from)?;
            client
                .health(HealthRequest {})
                .await
                .map_err(anyhow::Error::from)
        })
        .await;
        match health {
            Ok(Ok(response)) => {
                let health = response.into_inner();
                status.worker_ok = health.ok;
                status.requested_layout_id = health.requested_layout_id;
                status.effective_layout_id = health.effective_layout_id;
                status.fallback_reason =
                    (!health.fallback_reason.is_empty()).then_some(health.fallback_reason);
                status.graph_revision = health.graph_revision;
                status.selection_generation = health.selection_generation;
            }
            Ok(Err(error)) => status.worker_status_error = Some(error.to_string()),
            Err(_) => status.worker_status_error = Some("worker health timed out".into()),
        }
        status
    }

    /// Spawn a reconnecting forwarder task that dials the compute worker,
    /// streams `PositionDelta`s onto a broadcast channel, and redials with
    /// exponential backoff if the dial fails or the stream ends.
    pub async fn connect(&self, url: String) -> anyhow::Result<()> {
        self.connect_with(url, RemoteLayout::from_env()).await
    }

    /// Like [`connect`](Self::connect) but with an explicit remote-layout
    /// selection.
    pub async fn connect_with(&self, url: String, selection: RemoteLayout) -> anyhow::Result<()> {
        let _control = self.inner.control_plane.lock().await;
        let _ = Channel::from_shared(url.clone())
            .map_err(|e| anyhow::anyhow!("invalid compute url {url}: {e}"))?;

        let (tx, _rx) = broadcast::channel::<PositionDelta>(64);
        *self.inner.tx.write().await = Some(tx.clone());
        *self.inner.url.write().await = Some(url.clone());
        *self.inner.selection.write().await = SelectionState {
            layout: selection,
            generation: 1,
        };

        self.spawn_forwarder(url, tx).await;
        Ok(())
    }

    /// Hot-swap the worker's active graph (the `LoadGraph` RPC) — used by the
    /// `/compute/soup` self-assembly endpoint to push a server-synthesized
    /// particle soup into the worker. One-shot unary call on a fresh connection
    /// to the stored worker URL (the forwarder's streaming channel is for
    /// `Subscribe`). `csr` is the binary CSR DTO, `positions` optional
    /// interleaved-xyz f32 LE. Returns the worker's new node count.
    pub async fn load_graph(
        &self,
        csr: Vec<u8>,
        positions: Vec<u8>,
        graph_revision: u64,
        expected_worker_revision: u64,
        expected_selection_generation: u64,
        rebound_selection: Option<RemoteLayout>,
    ) -> anyhow::Result<u32> {
        let _control = self.inner.control_plane.lock().await;
        let url = self.inner.url.read().await.clone().ok_or_else(|| {
            anyhow::anyhow!("compute broker not connected (no worker URL configured)")
        })?;
        let previous_revision = self.inner.expected_revision.load(Ordering::Acquire);
        if previous_revision != expected_worker_revision {
            anyhow::bail!(
                "stale worker graph revision {expected_worker_revision}; current worker revision is {previous_revision}"
            );
        }
        let previous_selection = {
            let mut current = self.inner.selection.write().await;
            if current.generation != expected_selection_generation {
                anyhow::bail!(
                    "stale selection generation {expected_selection_generation}; current generation is {}",
                    current.generation
                );
            }
            let previous = current.layout.clone();
            if let Some(rebound) = rebound_selection {
                current.layout = rebound;
            }
            previous
        };
        // Fence the existing forwarder before the RPC: any old-revision frame
        // is dropped, and a reconnect cannot apply rebound attributes to the
        // worker's old graph because Subscribe carries this new revision.
        self.inner
            .expected_revision
            .store(graph_revision, Ordering::Release);
        let mut client = ComputeClient::connect(url.clone())
            .await
            .map_err(|e| anyhow::anyhow!("dial worker for LoadGraph: {e}"));
        let resp = match client.as_mut() {
            Ok(client) => client
                .load_graph(LoadGraphRequest {
                    csr,
                    positions,
                    graph_revision,
                    expected_selection_generation,
                })
                .await
                .map(|response| response.into_inner())
                .map_err(|e| anyhow::anyhow!("LoadGraph RPC: {e}")),
            Err(error) => Err(anyhow::anyhow!("{error}")),
        };
        let resp = match resp {
            Ok(resp) => resp,
            Err(error) => {
                self.inner
                    .expected_revision
                    .store(previous_revision, Ordering::Release);
                self.inner.selection.write().await.layout = previous_selection;
                return Err(error);
            }
        };
        if !resp.ok {
            self.inner
                .expected_revision
                .store(previous_revision, Ordering::Release);
            self.inner.selection.write().await.layout = previous_selection;
            anyhow::bail!(
                "worker rejected LoadGraph at selection generation {}: {}",
                resp.selection_generation,
                resp.error
            )
        }
        if resp.graph_revision != graph_revision {
            self.inner
                .expected_revision
                .store(previous_revision, Ordering::Release);
            self.inner.selection.write().await.layout = previous_selection;
            anyhow::bail!(
                "worker acknowledged graph revision {}, expected {}",
                resp.graph_revision,
                graph_revision
            )
        }
        if previous_revision == graph_revision
            && resp.selection_generation != expected_selection_generation
        {
            self.inner
                .expected_revision
                .store(previous_revision, Ordering::Release);
            self.inner.selection.write().await.layout = previous_selection;
            anyhow::bail!(
                "worker acknowledged selection generation {}, expected {}",
                resp.selection_generation,
                expected_selection_generation
            )
        }

        // LoadGraph clears the worker's active engine. Restarting Subscribe
        // both re-initializes it and binds the stream to the new revision.
        if let Some(tx) = self.inner.tx.read().await.as_ref().cloned() {
            self.spawn_forwarder(url, tx).await;
        }
        Ok(resp.n_nodes)
    }

    /// Switch the active remote layout engine (ADR-002, the `/compute/layout`
    /// PUT handler). Stores `selection`, aborts the live forwarder task, and
    /// spawns a fresh one that resubscribes with the new `layout_id`/`params`.
    /// Aborting the old task tears down its `Subscribe` stream so it does not
    /// leak and subsequent `/graph/layout/stream` frames come from the NEW
    /// engine. Reuses the existing broadcast channel + URL so WS clients
    /// already subscribed stay attached across the swap.
    ///
    /// Errors only if the broker was never `connect`ed (no URL stored) — a
    /// reselect against a disabled broker is a caller bug.
    pub async fn reselect(
        &self,
        selection: RemoteLayout,
        expected_graph_revision: Option<u64>,
        expected_generation: Option<u64>,
    ) -> anyhow::Result<SelectionUpdate> {
        let _control = self.inner.control_plane.lock().await;
        let url =
            self.inner.url.read().await.clone().ok_or_else(|| {
                anyhow::anyhow!("compute broker not connected (no URL configured)")
            })?;
        if let Some(expected) = expected_graph_revision {
            let current = self.inner.expected_revision.load(Ordering::Acquire);
            if expected != current {
                anyhow::bail!(
                    "stale graph revision {expected}; current worker revision is {current}"
                );
            }
        }

        let update = {
            let mut current = self.inner.selection.write().await;
            if current.layout == selection {
                return Ok(SelectionUpdate {
                    generation: current.generation,
                    changed: false,
                });
            }
            if let Some(expected) = expected_generation {
                if expected != current.generation {
                    anyhow::bail!(
                        "stale selection generation {expected}; current generation is {}",
                        current.generation
                    );
                }
            }
            let generation = current
                .generation
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("selection generation exhausted"))?;
            *current = SelectionState {
                layout: selection,
                generation,
            };
            SelectionUpdate {
                generation,
                changed: true,
            }
        };

        // Ensure a broadcast channel exists (it normally does after connect).
        // Reusing it keeps existing WS subscribers attached across the swap.
        let tx = {
            let guard = self.inner.tx.read().await;
            match guard.as_ref() {
                Some(tx) => tx.clone(),
                None => {
                    drop(guard);
                    let (tx, _rx) = broadcast::channel::<PositionDelta>(64);
                    *self.inner.tx.write().await = Some(tx.clone());
                    tx
                }
            }
        };

        self.spawn_forwarder(url, tx).await;
        Ok(update)
    }

    /// (Re)spawn the reconnecting forwarder task, aborting any previous one.
    /// The task reads the live `selection` from `Inner` on every (re)subscribe
    /// so a `reselect` that lands between reconnects is honoured on the next
    /// dial; the abort below guarantees an *immediate* swap rather than waiting
    /// for a worker-driven reconnect.
    async fn spawn_forwarder(&self, url: String, tx: broadcast::Sender<PositionDelta>) {
        let inner = self.inner.clone();
        let handle = tokio::spawn(async move {
            const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
            const BACKOFF_CAP: Duration = Duration::from_secs(30);
            let mut backoff = BACKOFF_INITIAL;

            loop {
                tracing::info!(url = %url, "compute broker dialing worker");
                let endpoint = match Channel::from_shared(url.clone()) {
                    Ok(e) => e,
                    Err(e) => {
                        // Should not happen — already validated in connect_with
                        // — but be defensive.
                        tracing::warn!("compute broker invalid url {url}: {e}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_CAP);
                        continue;
                    }
                };

                let channel = match endpoint.connect().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(url = %url, "compute broker dial failed: {e}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_CAP);
                        continue;
                    }
                };

                // Read the LIVE selection on each (re)subscribe so a worker
                // restart resumes the currently-selected engine (not the one
                // configured at boot, if a reselect happened since) — its
                // layout_id, params, and the resolved geometric attributes.
                let (req_layout_id, req_params, req_attributes, selection_generation) = {
                    let sel = inner.selection.read().await;
                    let params = sel.layout.params.clone().map(json_to_struct);
                    (
                        sel.layout.layout_id.clone(),
                        params,
                        sel.layout.attributes.clone(),
                        sel.generation,
                    )
                };

                let mut client = ComputeClient::new(channel);
                let graph_revision = inner.expected_revision.load(Ordering::Acquire);

                let stream = match client
                    .subscribe(SubscribeRequest {
                        graph_id: String::new(),
                        layout_id: req_layout_id,
                        params: req_params,
                        attributes: req_attributes,
                        graph_revision,
                        selection_generation,
                    })
                    .await
                {
                    Ok(s) => s.into_inner(),
                    Err(e) => {
                        tracing::warn!(url = %url, "compute broker subscribe failed: {e}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_CAP);
                        continue;
                    }
                };

                tracing::info!(url = %url, "compute broker connected; streaming frames");
                inner
                    .connected
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                backoff = BACKOFF_INITIAL;

                let mut stream = stream;
                loop {
                    // No need to poll for selection changes here: `reselect`
                    // aborts this task and spawns a fresh one, so a swap is
                    // applied immediately rather than detected on the next frame.
                    match tokio::time::timeout(Duration::from_millis(100), stream.message()).await {
                        Ok(Ok(Some(frame))) => {
                            let expected = inner.expected_revision.load(Ordering::Acquire);
                            if expected != 0 && frame.graph_revision != expected {
                                tracing::warn!(
                                    expected,
                                    got = frame.graph_revision,
                                    "dropping layout frame for the wrong graph revision"
                                );
                                continue;
                            }
                            let expected_generation = inner.selection.read().await.generation;
                            if frame.selection_generation != expected_generation {
                                tracing::warn!(
                                    expected = expected_generation,
                                    got = frame.selection_generation,
                                    "dropping layout frame for a stale selection"
                                );
                                continue;
                            }
                            let _ = tx.send(frame);
                        }
                        Ok(Ok(None)) => {
                            tracing::warn!("compute worker closed stream; reconnecting");
                            break;
                        }
                        Ok(Err(e)) => {
                            tracing::warn!("compute stream error: {e}; reconnecting");
                            break;
                        }
                        Err(_) => {
                            // Timeout: just loop and check selection
                            continue;
                        }
                    }
                }
                inner
                    .connected
                    .store(false, std::sync::atomic::Ordering::Relaxed);

                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_CAP);
            }
        });

        // Swap in the new handle, aborting (and dropping) the previous one so
        // its Subscribe stream is torn down — no leaked forwarder.
        let old = self.inner.forwarder.lock().await.replace(handle);
        if let Some(old) = old {
            old.abort();
        }
    }

    /// Snapshot of the engine registry for the `/compute/engines` endpoint
    /// (FROZEN CONTRACT). One-shot dials the stored compute URL, calls the
    /// `ListEngines` gRPC, and maps to the contract shape. `active` is the
    /// broker's currently-selected `layout_id` (empty ⇒ the worker default).
    ///
    /// Degrades gracefully (mirrors `/compute/health`): when the broker is
    /// disabled (no URL configured) or the dial/RPC fails, returns
    /// `{ connected: false, active: "", engines: [] }` rather than an error,
    /// so the renderer's picker shows a disabled hint instead of breaking.
    pub async fn list_engines(&self) -> EnginesView {
        let selection = self.inner.selection.read().await.clone();
        let active = selection.layout.layout_id;
        let selection_generation = selection.generation;
        let url = match self.inner.url.read().await.clone() {
            Some(u) => u,
            None => return EnginesView::disconnected(active, selection_generation),
        };

        let endpoint = match Channel::from_shared(url.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("compute broker invalid url {url} for ListEngines: {e}");
                return EnginesView::disconnected(active, selection_generation);
            }
        };
        let channel = match endpoint.connect().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(url = %url, "ListEngines dial failed: {e}");
                return EnginesView::disconnected(active, selection_generation);
            }
        };
        let mut client = ComputeClient::new(channel);
        let resp = match client.list_engines(ListEnginesRequest {}).await {
            Ok(r) => r.into_inner(),
            Err(e) => {
                tracing::warn!(url = %url, "ListEngines RPC failed: {e}");
                return EnginesView::disconnected(active, selection_generation);
            }
        };

        let engines = resp
            .engines
            .into_iter()
            .map(|d| EngineView {
                id: d.id,
                display_name: d.display_name,
                description: d.description,
                kind: d.kind,
            })
            .collect();
        EnginesView {
            connected: true,
            active,
            selection_generation,
            engines,
        }
    }

    /// Subscribe to the broadcast. Returns `None` if the broker hasn't
    /// connected to a worker yet.
    pub async fn subscribe(&self) -> Option<broadcast::Receiver<PositionDelta>> {
        self.inner.tx.read().await.as_ref().map(|tx| tx.subscribe())
    }

    /// The currently-selected remote layout (ADR-002). Mainly a test seam for
    /// asserting that `reselect` updated the stored selection.
    pub async fn selection(&self) -> RemoteLayout {
        self.inner.selection.read().await.layout.clone()
    }

    pub async fn selection_state(&self) -> SelectionState {
        self.inner.selection.read().await.clone()
    }
}

/// Snapshot returned by [`ComputeBroker::status`] — fed to the
/// `/compute/health` HTTP endpoint so the renderer can show the
/// back-half-of-the-chain liveness in the footer log.
#[derive(Clone, Debug, serde::Serialize)]
pub struct BrokerStatus {
    pub connected: bool,
    /// May be empty if `connect()` was never called.
    pub url: String,
    pub worker_ok: bool,
    pub requested_layout_id: String,
    pub effective_layout_id: String,
    pub fallback_reason: Option<String>,
    pub worker_status_error: Option<String>,
    pub selection_generation: u64,
    pub graph_revision: u64,
}

/// JSON body for `GET /compute/engines` (FROZEN CONTRACT). Serializes to:
/// `{ "connected": bool, "active": "<layout_id>", "engines": [ … ] }`.
/// `disconnected()` is the graceful degraded form (broker disabled or the
/// dial/RPC failed) — HTTP 200, not an error.
#[derive(Clone, Debug, serde::Serialize)]
pub struct EnginesView {
    /// Broker connected to a worker (a successful ListEngines round-trip).
    pub connected: bool,
    /// Currently-selected remote engine id (`""` if none / worker default).
    pub active: String,
    pub selection_generation: u64,
    pub engines: Vec<EngineView>,
}

impl EnginesView {
    /// The degraded form returned when the broker is disabled or the worker
    /// is unreachable. Per the contract this is still HTTP 200.
    pub fn disconnected(active: String, selection_generation: u64) -> Self {
        Self {
            connected: false,
            active,
            selection_generation,
            engines: Vec::new(),
        }
    }
}

/// One selectable engine in the `/compute/engines` payload (FROZEN CONTRACT).
/// Mirrors the gRPC `EngineDescriptor`; `kind` is `"Physics"` | `"Static"`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct EngineView {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub kind: String,
}

impl Default for ComputeBroker {
    fn default() -> Self {
        Self::new()
    }
}
