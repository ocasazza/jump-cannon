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

    /// Spawn a reconnecting forwarder task that dials the compute worker,
    /// streams `PositionDelta`s onto a broadcast channel, and redials with
    /// exponential backoff if the dial fails or the stream ends.
    ///
    /// Returns immediately — startup is never blocked on the worker being
    /// reachable. The first successful dial validates `url`; an *invalid*
    /// url (parse failure) is reported synchronously as an error.
    ///
    /// TODO(auth): wrap `ComputeClient` in a tonic interceptor that injects
    /// a bearer token from `JUMP_CANNON_COMPUTE_TOKEN`.
    pub async fn connect(&self, url: String) -> anyhow::Result<()> {
        // Validate the URL eagerly so a typo fails the boot sequence loudly.
        // The actual TCP dial happens inside the spawned reconnect loop.
        let _ = Channel::from_shared(url.clone())
            .map_err(|e| anyhow::anyhow!("invalid compute url {url}: {e}"))?;

        // Create the broadcast channel up front. WS clients can subscribe
        // before the first dial succeeds; they'll just sit on an empty
        // receiver until frames arrive (or after a worker restart).
        let (tx, _rx) = broadcast::channel::<PositionDelta>(64);
        *self.inner.tx.write().await = Some(tx.clone());

        // Reconnecting forwarder. Each iteration: dial, subscribe, pump
        // frames until the stream ends or errors, then back off and retry.
        tokio::spawn(async move {
            const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
            const BACKOFF_CAP: Duration = Duration::from_secs(30);
            let mut backoff = BACKOFF_INITIAL;

            loop {
                tracing::info!(url = %url, "compute broker dialing worker");
                let endpoint = match Channel::from_shared(url.clone()) {
                    Ok(e) => e,
                    Err(e) => {
                        // Should not happen — already validated above — but
                        // be defensive so a future env-var refresh path is
                        // safe.
                        tracing::warn!("compute broker invalid url {url}: {e}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_CAP);
                        continue;
                    }
                };

                let channel = match endpoint.connect().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            url = %url,
                            backoff_secs = backoff.as_secs(),
                            "compute broker dial failed: {e}",
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_CAP);
                        continue;
                    }
                };

                let mut client = ComputeClient::new(channel);
                let stream = match client
                    .subscribe(SubscribeRequest {
                        graph_id: String::new(),
                    })
                    .await
                {
                    Ok(s) => s.into_inner(),
                    Err(e) => {
                        tracing::warn!(
                            url = %url,
                            backoff_secs = backoff.as_secs(),
                            "compute broker subscribe failed: {e}",
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_CAP);
                        continue;
                    }
                };

                tracing::info!(url = %url, "compute broker connected; streaming frames");
                backoff = BACKOFF_INITIAL;

                let mut stream = stream;
                loop {
                    match stream.message().await {
                        Ok(Some(frame)) => {
                            let _ = tx.send(frame);
                        }
                        Ok(None) => {
                            tracing::warn!("compute worker closed stream; reconnecting");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!("compute stream error: {e}; reconnecting");
                            break;
                        }
                    }
                }

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

impl Default for ComputeBroker {
    fn default() -> Self {
        Self::new()
    }
}
