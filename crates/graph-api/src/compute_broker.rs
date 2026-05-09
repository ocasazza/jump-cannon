//! Compute-worker broker.
//!
//! graph-api dials a `graph-compute` worker (default `http://[::1]:50051`,
//! override with `JUMP_CANNON_COMPUTE_URL`) and re-broadcasts each
//! `PositionDelta` it receives onto a `tokio::sync::broadcast` channel that
//! the WebSocket handler subscribes to.
//!
//! Boot semantics: the dial is best-effort. If the worker isn't reachable the
//! broker stays in a `Disconnected` state and the `/graph/layout/stream`
//! endpoint returns 503. A future revision should reconnect with backoff.

use std::sync::Arc;
use tokio::sync::broadcast;
use tonic::transport::Channel;

use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::{PositionDelta, SubscribeRequest};

#[derive(Clone)]
pub struct ComputeBroker {
    inner: Arc<Inner>,
}

struct Inner {
    /// `None` until the dial succeeds. After connect, holds the broadcast
    /// sender used to fan PositionDeltas out to WS clients.
    tx: tokio::sync::RwLock<Option<broadcast::Sender<PositionDelta>>>,
}

impl ComputeBroker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                tx: tokio::sync::RwLock::new(None),
            }),
        }
    }

    /// Try to dial the compute worker. On success, spawn a forwarder task
    /// that pumps the gRPC stream into a broadcast channel. The caller logs
    /// the error path; the broker stays disconnected on failure.
    pub async fn connect(&self, url: String) -> anyhow::Result<()> {
        let endpoint = Channel::from_shared(url.clone())
            .map_err(|e| anyhow::anyhow!("invalid compute url {url}: {e}"))?;
        let channel = endpoint.connect().await?;
        let mut client = ComputeClient::new(channel);

        let stream = client
            .subscribe(SubscribeRequest {
                graph_id: String::new(),
            })
            .await?
            .into_inner();

        let (tx, _rx) = broadcast::channel::<PositionDelta>(64);
        *self.inner.tx.write().await = Some(tx.clone());

        // Forwarder task: pull frames from the worker, fan out to WS clients.
        // Lives as long as the gRPC stream stays open. If the worker dies,
        // we drop the broadcast::Sender and `/graph/layout/stream` clients
        // see Closed and disconnect.
        tokio::spawn(async move {
            let mut stream = stream;
            loop {
                match stream.message().await {
                    Ok(Some(frame)) => {
                        let _ = tx.send(frame);
                    }
                    Ok(None) => {
                        tracing::warn!("compute worker closed stream");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("compute stream error: {e}");
                        break;
                    }
                }
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

impl Default for ComputeBroker {
    fn default() -> Self {
        Self::new()
    }
}
