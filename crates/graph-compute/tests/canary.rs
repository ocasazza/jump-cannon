//! Live-cluster canaries. Probe a running graph-compute (local podman or
//! SkyPilot pod) and assert end-to-end health.
//!
//! Tests are gated on `GRAPH_COMPUTE_CANARY_URL` — without it they're
//! skipped, so they're safe to leave in `cargo test --workspace`. Run
//! against a live cluster via:
//!
//!     just dev-up
//!     GRAPH_COMPUTE_CANARY_URL=http://[::1]:50051 cargo test \
//!         -p graph-compute --test canary -- --nocapture
//!
//! Sweep parameters via env:
//!     GRAPH_COMPUTE_EXPECTED_NODES   (default: 1024)
//!     GRAPH_COMPUTE_MIN_FRAMES       (default: 5)
//!     GRAPH_COMPUTE_WINDOW_MS        (default: 1500)
//!     GRAPH_COMPUTE_MAX_HEALTH_MS    (default: 250)

use std::time::{Duration, Instant};

use graph_compute::proto::{compute_client::ComputeClient, HealthRequest, SubscribeRequest};

fn canary_url() -> Option<String> {
    std::env::var("GRAPH_COMPUTE_CANARY_URL").ok()
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

async fn connect(url: &str) -> ComputeClient<tonic::transport::Channel> {
    let chan = tonic::transport::Channel::from_shared(url.to_string())
        .expect("valid URL")
        .timeout(Duration::from_secs(2))
        .connect()
        .await
        .expect("dial graph-compute");
    ComputeClient::new(chan)
}

#[tokio::test]
async fn canary_health_responds_quickly() {
    let Some(url) = canary_url() else { return };
    let max_ms = env_u64("GRAPH_COMPUTE_MAX_HEALTH_MS", 250);
    let mut client = connect(&url).await;
    let start = Instant::now();
    let h = client.health(HealthRequest {}).await.expect("health rpc").into_inner();
    let elapsed = start.elapsed();
    println!("HEALTH: frame={} n_nodes={} elapsed_ms={}", h.frame, h.n_nodes, elapsed.as_millis());
    assert!(
        elapsed.as_millis() <= max_ms as u128,
        "health rpc took {}ms (>{}ms)",
        elapsed.as_millis(),
        max_ms
    );
}

#[tokio::test]
async fn canary_node_count_matches() {
    let Some(url) = canary_url() else { return };
    let expected = env_u32("GRAPH_COMPUTE_EXPECTED_NODES", 1024);
    let mut client = connect(&url).await;
    let h = client.health(HealthRequest {}).await.expect("health").into_inner();
    assert_eq!(
        h.n_nodes, expected,
        "node count mismatch: server reports {} expected {}",
        h.n_nodes, expected
    );
}

#[tokio::test]
async fn canary_subscribe_streams_frames() {
    let Some(url) = canary_url() else { return };
    let expected_nodes = env_u32("GRAPH_COMPUTE_EXPECTED_NODES", 1024);
    let min_frames = env_u32("GRAPH_COMPUTE_MIN_FRAMES", 5);
    let window = Duration::from_millis(env_u64("GRAPH_COMPUTE_WINDOW_MS", 1500));

    let mut client = connect(&url).await;
    let mut s = client
        .subscribe(SubscribeRequest { graph_id: String::new(), ..Default::default() })
        .await
        .expect("subscribe")
        .into_inner();

    let deadline = Instant::now() + window;
    let mut frames = Vec::with_capacity(min_frames as usize);
    while frames.len() < min_frames as usize {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, s.message()).await {
            Ok(Ok(Some(d))) => frames.push(d),
            _ => break,
        }
    }

    assert!(
        frames.len() >= min_frames as usize,
        "received only {}/{} frames within {}ms",
        frames.len(),
        min_frames,
        window.as_millis()
    );

    // Frame numbers must be strictly increasing.
    for w in frames.windows(2) {
        assert!(
            w[1].frame > w[0].frame,
            "frame numbers not strictly increasing: {} -> {}",
            w[0].frame,
            w[1].frame
        );
    }

    // Each delta must carry n_nodes * 3 * 4 bytes.
    let expect_bytes = (expected_nodes as usize) * 3 * 4;
    for d in &frames {
        assert_eq!(d.n_nodes, expected_nodes, "delta n_nodes={}, expected {}", d.n_nodes, expected_nodes);
        assert_eq!(
            d.positions.len(),
            expect_bytes,
            "delta payload {} bytes (expected {})",
            d.positions.len(),
            expect_bytes
        );
    }
}

#[tokio::test]
async fn canary_positions_are_changing() {
    // Catches a "stuck integrator" regression where the sim is alive but
    // not actually advancing positions (e.g. dt=0, halted state, or a wgpu
    // dispatch that bound the wrong positions buffer).
    let Some(url) = canary_url() else { return };

    let mut client = connect(&url).await;
    let mut s = client
        .subscribe(SubscribeRequest { graph_id: String::new(), ..Default::default() })
        .await
        .expect("subscribe")
        .into_inner();

    let first = tokio::time::timeout(Duration::from_secs(2), s.message())
        .await
        .expect("first frame timeout")
        .expect("first frame")
        .expect("frame body")
        .positions;

    // Skip forward several frames.
    for _ in 0..5 {
        let _ = tokio::time::timeout(Duration::from_millis(500), s.message()).await;
    }
    let later = tokio::time::timeout(Duration::from_secs(2), s.message())
        .await
        .expect("later frame timeout")
        .expect("later frame")
        .expect("frame body")
        .positions;

    assert_eq!(first.len(), later.len(), "frame size changed mid-stream");

    // Treat as f32 LE buffers and compute L2 distance. Spring-only or FA2
    // will move at least *something* over five frames at 30Hz.
    let l2: f64 = first
        .chunks_exact(4)
        .zip(later.chunks_exact(4))
        .map(|(a, b)| {
            let av = f32::from_le_bytes(a.try_into().unwrap()) as f64;
            let bv = f32::from_le_bytes(b.try_into().unwrap()) as f64;
            (av - bv).powi(2)
        })
        .sum::<f64>()
        .sqrt();

    println!("L2 distance over ~5 frames: {l2:.6}");
    assert!(l2 > 0.0, "positions unchanged across 5 frames — sim appears stuck");
}
