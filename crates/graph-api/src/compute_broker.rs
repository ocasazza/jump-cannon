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

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tonic::transport::Channel;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::{
    GraphAttributes as ProtoGraphAttributes, ListEnginesRequest, PositionDelta, SubscribeRequest,
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
    selection: tokio::sync::RwLock<RemoteLayout>,
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
                selection: tokio::sync::RwLock::new(RemoteLayout::default()),
                forwarder: tokio::sync::Mutex::new(None),
            }),
        }
    }

    /// Snapshot of the broker's reachability to the compute worker.
    /// Cheap (one atomic load + one read-lock async hop for the url).
    pub async fn status(&self) -> BrokerStatus {
        let url = self.inner.url.read().await.clone().unwrap_or_default();
        BrokerStatus {
            connected: self
                .inner
                .connected
                .load(std::sync::atomic::Ordering::Relaxed),
            url,
        }
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
        let _ = Channel::from_shared(url.clone())
            .map_err(|e| anyhow::anyhow!("invalid compute url {url}: {e}"))?;

        let (tx, _rx) = broadcast::channel::<PositionDelta>(64);
        *self.inner.tx.write().await = Some(tx.clone());
        *self.inner.url.write().await = Some(url.clone());
        *self.inner.selection.write().await = selection;

        self.spawn_forwarder(url, tx).await;
        Ok(())
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
    pub async fn reselect(&self, selection: RemoteLayout) -> anyhow::Result<()> {
        let url = self
            .inner
            .url
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("compute broker not connected (no URL configured)"))?;

        // Publish the new selection BEFORE restarting the loop so the freshly
        // spawned forwarder picks it up on its first subscribe.
        *self.inner.selection.write().await = selection;

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
        Ok(())
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
                let (req_layout_id, req_params, req_attributes) = {
                    let sel = inner.selection.read().await;
                    let params = sel.params.clone().map(json_to_struct);
                    (sel.layout_id.clone(), params, sel.attributes.clone())
                };

                let mut client = ComputeClient::new(channel);

                let stream = match client
                    .subscribe(SubscribeRequest {
                        graph_id: String::new(),
                        layout_id: req_layout_id,
                        params: req_params,
                        attributes: req_attributes,
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
                inner.connected.store(true, std::sync::atomic::Ordering::Relaxed);
                backoff = BACKOFF_INITIAL;

                let mut stream = stream;
                loop {
                    // No need to poll for selection changes here: `reselect`
                    // aborts this task and spawns a fresh one, so a swap is
                    // applied immediately rather than detected on the next frame.
                    match tokio::time::timeout(Duration::from_millis(100), stream.message()).await {
                        Ok(Ok(Some(frame))) => {
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
                inner.connected.store(false, std::sync::atomic::Ordering::Relaxed);

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
        let url = match self.inner.url.read().await.clone() {
            Some(u) => u,
            None => return EnginesView::disconnected(),
        };
        let active = self.inner.selection.read().await.layout_id.clone();

        let endpoint = match Channel::from_shared(url.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("compute broker invalid url {url} for ListEngines: {e}");
                return EnginesView::disconnected();
            }
        };
        let channel = match endpoint.connect().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(url = %url, "ListEngines dial failed: {e}");
                return EnginesView::disconnected();
            }
        };
        let mut client = ComputeClient::new(channel);
        let resp = match client.list_engines(ListEnginesRequest {}).await {
            Ok(r) => r.into_inner(),
            Err(e) => {
                tracing::warn!(url = %url, "ListEngines RPC failed: {e}");
                return EnginesView::disconnected();
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
    pub engines: Vec<EngineView>,
}

impl EnginesView {
    /// The degraded form returned when the broker is disabled or the worker
    /// is unreachable. Per the contract this is still HTTP 200.
    pub fn disconnected() -> Self {
        Self {
            connected: false,
            active: String::new(),
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
