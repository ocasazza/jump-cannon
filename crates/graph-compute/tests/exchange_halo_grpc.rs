//! In-process round-trip test for the distributed `Compute::ExchangeHalo` RPC
//! (docs/compute-architecture.md §4, step c: "exchange boundary positions").
//!
//! This exercises the REAL gRPC path — the boundary `HaloDelta`s cross the tonic
//! codec both ways — WITHOUT binding a TCP port. We use tonic's standard
//! in-memory pattern: a `tokio::io::duplex` pipe whose server end is fed to
//! `Server::serve_with_incoming`, and whose client end is handed to
//! `Endpoint::connect_with_connector` via a `tower::service_fn` connector (the
//! duplex stream wrapped in `hyper_util::rt::TokioIo` to satisfy hyper's
//! Read/Write runtime traits). No sockets, so it runs under the sandbox.
//!
//! The scenario is a two-worker boundary exchange on `path(6)` split into two
//! partitions (owned {0,1,2} | {3,4,5}, boundary node 2 | 3). The server worker
//! owns partition 1; when partition 0 streams in its boundary delta for a frame,
//! the server replies with partition 1's boundary delta for that frame, and we
//! assert the right node id + positions round-trip.

use std::future::ready;
use std::sync::Arc;

use graph_compute::partition::{partition_csr, HaloDelta as HostHaloDelta};
use graph_compute::proto::compute_client::ComputeClient;
use graph_compute::proto::compute_server::ComputeServer;
use graph_compute::proto::HaloDelta;
use graph_compute::service::{ComputeService, HaloProvider};
use graph_compute::sim::{CsrGraph, SimState};
use hyper_util::rt::TokioIo;
use tonic::transport::{Endpoint, Server, Uri};

/// A trivial [`HaloProvider`] standing in for a real distributed worker: it
/// always replies with the same fixed boundary delta (partition 1's boundary
/// positions for the requested frame), keyed to whatever frame the peer asked
/// about. A production worker would pull live owned positions; this fixed reply
/// is enough to prove the gRPC bytes round-trip end to end.
struct FixedHaloProvider {
    owner_id: u32,
    node_ids: Vec<u32>,
    positions: Vec<f32>,
}

impl HaloProvider for FixedHaloProvider {
    fn outgoing_for(&self, frame: u64, _inbound: &HostHaloDelta) -> Vec<HostHaloDelta> {
        vec![HostHaloDelta {
            frame,
            owner_id: self.owner_id,
            node_ids: self.node_ids.clone(),
            positions: self.positions.clone(),
            attributes: None,
        }]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_halo_round_trips_boundary_positions() {
    // Two-worker partition of path(6): part 0 owns {0,1,2}, part 1 owns {3,4,5}.
    let parts = partition_csr(&CsrGraph::path(6), None, 2);
    assert_eq!(parts.len(), 2);
    let p1 = &parts[1];
    assert!(
        !p1.boundary_global_ids().is_empty(),
        "partition 1 must have a boundary node at the cut"
    );

    // The server worker (partition 1) owes the peer its boundary node's position.
    // Give that boundary node a recognizable position so we can assert it survived
    // the wire round-trip.
    let boundary_id = p1.boundary_global_ids()[0];
    let provider = Arc::new(FixedHaloProvider {
        owner_id: p1.partition_id,
        node_ids: vec![boundary_id],
        positions: vec![3.5, -1.25, 0.75],
    });

    // Build the service with the halo provider installed (without it the RPC is
    // `unimplemented`). The SimState graph is irrelevant to ExchangeHalo here.
    let state = SimState::new(CsrGraph::path(6));
    let svc = ComputeService::new(state).with_halo_provider(provider);

    // In-memory transport: a duplex pipe. Server gets one end as its single
    // incoming connection; the client connects through the other end.
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);

    // Serve over the single in-memory connection (no TCP bind).
    let incoming = tokio_stream::once(Ok::<_, std::io::Error>(server_io));
    tokio::spawn(async move {
        Server::builder()
            .add_service(ComputeServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    // Connect the client through the duplex client end via a service_fn
    // connector. The Uri is a dummy — the connector ignores it and always
    // returns our in-memory pipe.
    let mut client_io = Some(client_io);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = client_io.take().expect("connector invoked more than once");
            ready(Ok::<_, std::io::Error>(TokioIo::new(io)))
        }))
        .await
        .expect("connect over in-memory duplex");

    let mut client = ComputeClient::new(channel);

    // Partition 0 streams its boundary delta for frame 7; the server replies with
    // partition 1's boundary delta for the same frame.
    let p0 = &parts[0];
    let p0_boundary = p0.boundary_global_ids()[0];
    let (out_node_ids, out_positions) = HostHaloDelta {
        frame: 7,
        owner_id: p0.partition_id,
        node_ids: vec![p0_boundary],
        positions: vec![9.0, 9.0, 9.0],
        attributes: None,
    }
    .encode_bytes();
    let request = HaloDelta {
        frame: 7,
        owner_id: p0.partition_id,
        node_ids: out_node_ids,
        positions: out_positions,
        attributes: None,
    };

    let req_stream = tokio_stream::once(request);
    let mut resp = client
        .exchange_halo(req_stream)
        .await
        .expect("open ExchangeHalo stream")
        .into_inner();

    let reply = tokio::time::timeout(std::time::Duration::from_secs(2), resp.message())
        .await
        .expect("ExchangeHalo reply timed out")
        .expect("stream error")
        .expect("server closed without a reply");

    // Decode the reply and assert it is partition 1's boundary delta, intact.
    let decoded = HostHaloDelta::decode_bytes(
        reply.frame,
        reply.owner_id,
        &reply.node_ids,
        &reply.positions,
        reply.attributes,
    )
    .expect("decode reply HaloDelta");
    assert_eq!(decoded.frame, 7, "reply must echo the request frame");
    assert_eq!(decoded.owner_id, p1.partition_id);
    assert_eq!(decoded.node_ids, vec![boundary_id]);
    assert_eq!(decoded.positions, vec![3.5, -1.25, 0.75]);
}

/// Without a `HaloProvider` installed (the single-process default), the RPC must
/// report `unimplemented` rather than silently succeeding — the single-process
/// path uses `LocalTransport`, not the network. Same in-memory transport, no TCP.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_halo_unimplemented_without_provider() {
    let state = SimState::new(CsrGraph::path(6));
    let svc = ComputeService::new(state); // no provider

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let incoming = tokio_stream::once(Ok::<_, std::io::Error>(server_io));
    tokio::spawn(async move {
        Server::builder()
            .add_service(ComputeServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    let mut client_io = Some(client_io);
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let io = client_io.take().expect("connector invoked more than once");
            ready(Ok::<_, std::io::Error>(TokioIo::new(io)))
        }))
        .await
        .expect("connect over in-memory duplex");

    let mut client = ComputeClient::new(channel);
    let req_stream = tokio_stream::once(HaloDelta::default());
    // The `unimplemented` status surfaces when the server rejects the RPC. tonic
    // may deliver it either at stream-open or on the first `message()` poll
    // depending on timing, so accept it in either place.
    let err = match client.exchange_halo(req_stream).await {
        Err(status) => status,
        Ok(resp) => resp
            .into_inner()
            .message()
            .await
            .expect_err("expected an unimplemented status"),
    };
    assert_eq!(err.code(), tonic::Code::Unimplemented);
}
