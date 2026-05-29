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
use arc_swap::ArcSwap;
use tokio::sync::broadcast;
use tonic::transport::Channel;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::{GraphAttributes as ProtoGraphAttributes, PositionDelta, SubscribeRequest};
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
    /// `/compute/health`.
    url: tokio::sync::RwLock<Option<String>>,
    /// Current layout selection.
    selection: ArcSwap<RemoteLayout>,
}

impl ComputeBroker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                tx: tokio::sync::RwLock::new(None),
                connected: std::sync::atomic::AtomicBool::new(false),
                url: tokio::sync::RwLock::new(None),
                selection: ArcSwap::new(Arc::new(RemoteLayout::default())),
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

    /// Update the layout selection.
    pub fn set_selection(&self, selection: RemoteLayout) {
        self.inner.selection.store(Arc::new(selection));
    }

    pub fn selection(&self) -> Arc<RemoteLayout> {
        self.inner.selection.load_full()
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

        self.set_selection(selection);

        let (tx, _rx) = broadcast::channel::<PositionDelta>(64);
        *self.inner.tx.write().await = Some(tx.clone());
        *self.inner.url.write().await = Some(url.clone());

        let inner = self.inner.clone();
        tokio::spawn(async move {
            const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
            const BACKOFF_CAP: Duration = Duration::from_secs(30);
            let mut backoff = BACKOFF_INITIAL;

            loop {
                tracing::info!(url = %url, "compute broker dialing worker");
                let endpoint = match Channel::from_shared(url.clone()) {
                    Ok(e) => e,
                    Err(_) => {
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

                let mut client = ComputeClient::new(channel);
                
                // Get current selection
                let selection = inner.selection.load();
                let req_layout_id = selection.layout_id.clone();
                let req_params = selection.params.as_ref().map(|v| json_to_struct(v.clone()));
                let req_attributes = selection.attributes.clone();

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
                    // Check if selection changed
                    if **inner.selection.load() != **selection {
                        tracing::info!("selection changed, re-subscribing");
                        break;
                    }

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

        Ok(())
    }

    /// Subscribe to the broadcast. Returns `None` if the broker hasn't
    /// connected to a worker yet.
    pub async fn subscribe(&self) -> Option<broadcast::Receiver<PositionDelta>> {
        self.inner.tx.read().await.as_ref().map(|tx| tx.subscribe())
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

impl Default for ComputeBroker {
    fn default() -> Self {
        Self::new()
    }
}
