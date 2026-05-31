//! Live probe: subscribes to an external graph-compute and prints frames.
//! Usage: `cargo run --release -p graph-compute --example probe`
//! Reads URL from `GRAPH_COMPUTE_PROBE_URL` (default `http://[::1]:50051`).
use std::time::Duration;
use graph_compute::proto::{compute_client::ComputeClient, HealthRequest, SubscribeRequest};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("GRAPH_COMPUTE_PROBE_URL").unwrap_or_else(|_| "http://[::1]:50051".into());
    println!("dialing {url}");
    let chan = tonic::transport::Channel::from_shared(url)?
        .timeout(Duration::from_secs(2))
        .connect()
        .await?;
    let mut client = ComputeClient::new(chan);

    let h = client.health(HealthRequest{}).await?.into_inner();
    println!("HEALTH: frame={} n_nodes={}", h.frame, h.n_nodes);

    let mut s = client
        .subscribe(SubscribeRequest {
            graph_id: String::new(),
            layout_id: String::new(),
            params: None,
            attributes: None,
        })
        .await?
        .into_inner();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut count = 0;
    while count < 3 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, s.message()).await {
            Ok(Ok(Some(d))) => {
                println!("FRAME: {} n={} bytes={}", d.frame, d.n_nodes, d.positions.len());
                count += 1;
            }
            _ => break,
        }
    }
    println!("OK: received {count} frames");
    Ok(())
}
