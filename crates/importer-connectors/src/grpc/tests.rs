//! gRPC tests run against an in-process tonic server on a loopback socket.
//! The service is hand-rolled (raw gRPC frames, no codegen) and the client
//! descriptors are built programmatically, so the suite needs no build.rs and
//! no protoc.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::future::Future;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::Frame;
use http_body_util::StreamBody;
use parking_lot::Mutex;
use prost::Message;
use tonic::codegen::http;

use super::*;

#[derive(Clone, PartialEq, Message)]
struct HelloReply {
    #[prost(string, tag = "1")]
    message: String,
}

fn greeter_file() -> prost_types::FileDescriptorProto {
    let string_field = |name: &str, number: i32| prost_types::FieldDescriptorProto {
        name: Some(name.to_string()),
        number: Some(number),
        label: Some(prost_types::field_descriptor_proto::Label::Optional as i32),
        r#type: Some(prost_types::field_descriptor_proto::Type::String as i32),
        json_name: Some(name.to_string()),
        ..Default::default()
    };
    prost_types::FileDescriptorProto {
        name: Some("test/greeter.proto".to_string()),
        package: Some("test".to_string()),
        syntax: Some("proto3".to_string()),
        message_type: vec![
            prost_types::DescriptorProto {
                name: Some("HelloRequest".to_string()),
                field: vec![string_field("name", 1)],
                ..Default::default()
            },
            prost_types::DescriptorProto {
                name: Some("HelloReply".to_string()),
                field: vec![string_field("message", 1)],
                ..Default::default()
            },
        ],
        service: vec![prost_types::ServiceDescriptorProto {
            name: Some("Greeter".to_string()),
            method: vec![
                prost_types::MethodDescriptorProto {
                    name: Some("SayHello".to_string()),
                    input_type: Some(".test.HelloRequest".to_string()),
                    output_type: Some(".test.HelloReply".to_string()),
                    ..Default::default()
                },
                prost_types::MethodDescriptorProto {
                    name: Some("LotsOfReplies".to_string()),
                    input_type: Some(".test.HelloRequest".to_string()),
                    output_type: Some(".test.HelloReply".to_string()),
                    server_streaming: Some(true),
                    ..Default::default()
                },
                prost_types::MethodDescriptorProto {
                    name: Some("StreamIn".to_string()),
                    input_type: Some(".test.HelloRequest".to_string()),
                    output_type: Some(".test.HelloReply".to_string()),
                    client_streaming: Some(true),
                    ..Default::default()
                },
                prost_types::MethodDescriptorProto {
                    name: Some("Boom".to_string()),
                    input_type: Some(".test.HelloRequest".to_string()),
                    output_type: Some(".test.HelloReply".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn descriptor_set_bytes() -> Vec<u8> {
    prost_types::FileDescriptorSet {
        file: vec![greeter_file()],
    }
    .encode_to_vec()
}

fn data_frame(message: &HelloReply) -> Bytes {
    let payload = message.encode_to_vec();
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0u8);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Bytes::from(frame)
}

fn trailers_frame(status: &str) -> http::HeaderMap {
    let mut trailers = http::HeaderMap::new();
    trailers.insert("grpc-status", status.parse().unwrap());
    trailers
}

type CapturedCalls = Arc<Mutex<Vec<(String, Option<String>)>>>;

#[derive(Clone)]
struct TestGreeter {
    calls: CapturedCalls,
}

impl tonic::server::NamedService for TestGreeter {
    const NAME: &'static str = "test.Greeter";
}

type BoxFuture =
    Pin<Box<dyn Future<Output = Result<http::Response<tonic::body::Body>, Infallible>> + Send>>;

impl tonic::codegen::Service<http::Request<tonic::body::Body>> for TestGreeter {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        let path = request.uri().path().to_string();
        let authorization = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        self.calls.lock().push((path.clone(), authorization));
        Box::pin(async move {
            let mut frames: Vec<Result<Frame<Bytes>, Infallible>> = Vec::new();
            match path.as_str() {
                "/test.Greeter/SayHello" => {
                    frames.push(Ok(Frame::data(data_frame(&HelloReply {
                        message: "hello world".to_string(),
                    }))));
                }
                "/test.Greeter/LotsOfReplies" => {
                    for index in 0..3 {
                        frames.push(Ok(Frame::data(data_frame(&HelloReply {
                            message: format!("reply {index}"),
                        }))));
                    }
                }
                _ => {
                    frames.push(Ok(Frame::trailers(trailers_frame("12"))));
                    let body = tonic::body::Body::new(StreamBody::new(tokio_stream::iter(frames)));
                    return Ok(http::Response::builder()
                        .header("content-type", "application/grpc")
                        .body(body)
                        .unwrap());
                }
            }
            frames.push(Ok(Frame::trailers(trailers_frame("0"))));
            let body = tonic::body::Body::new(StreamBody::new(tokio_stream::iter(frames)));
            Ok(http::Response::builder()
                .header("content-type", "application/grpc")
                .body(body)
                .unwrap())
        })
    }
}

async fn spawn_server(with_reflection: bool) -> (String, CapturedCalls) {
    let calls: CapturedCalls = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let greeter = TestGreeter { calls: calls.clone() };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let descriptors = descriptor_set_bytes();
    tokio::spawn(async move {
        if with_reflection {
            let reflection = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(&descriptors)
                .build_v1()
                .unwrap();
            tonic::transport::Server::builder()
                .add_service(reflection)
                .add_service(greeter)
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        } else {
            tonic::transport::Server::builder()
                .add_service(greeter)
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        }
    });
    (format!("http://{addr}"), calls)
}

fn descriptor_file() -> (tempfile::TempDir, DescriptorSource) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("descriptors.pb");
    std::fs::write(&path, descriptor_set_bytes()).unwrap();
    (dir, DescriptorSource::File(path))
}

#[test]
fn capability_scope_is_endpoint_plus_method() {
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(GrpcConfig::new(
        "http://grpc.example.com:50051",
        descriptors,
        "test.Greeter/SayHello",
    ))
    .unwrap();
    for effect in [Effect::Read, Effect::Watch] {
        assert_eq!(
            connector.capabilities(effect),
            vec![Capability::new(
                effect,
                Transport::Grpc,
                "http://grpc.example.com:50051/test.Greeter/SayHello"
            )]
        );
    }
    assert!(connector.capabilities(Effect::Write).is_empty());
}

#[test]
fn rejects_invalid_config() {
    let (_dir, descriptors) = descriptor_file();
    for config in [
        GrpcConfig::new("http://x", descriptors.clone(), "NotFullyQualified"),
        GrpcConfig::new("http://x", descriptors.clone(), "/Method"),
        GrpcConfig::new("http://x", descriptors.clone(), "svc/Method").with_max_messages(0),
        GrpcConfig::new("http://x", descriptors.clone(), "svc/Method").with_max_response_bytes(0),
    ] {
        assert!(matches!(
            config.validate(),
            Err(ImportError::InvalidDescriptor { .. })
        ));
    }
}

#[test]
fn debug_redacts_metadata_token() {
    let (_dir, descriptors) = descriptor_file();
    let config = GrpcConfig::new("http://x", descriptors, "test.Greeter/SayHello")
        .with_metadata_token("grpc-secret");
    let debug = format!("{config:?}");
    assert!(!debug.contains("grpc-secret"), "token leaked: {debug}");
    assert!(debug.contains("<redacted>"));
}

#[tokio::test]
async fn unary_returns_one_json_record() {
    let (endpoint, _calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(GrpcConfig::new(
        &endpoint,
        descriptors,
        "test.Greeter/SayHello",
    ))
    .unwrap();
    let records = connector.read().await.unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        record.origin,
        format!("{endpoint}/test.Greeter/SayHello#0")
    );
    assert_eq!(record.content_type, CONTENT_TYPE);
    let value: serde_json::Value = serde_json::from_slice(&record.bytes).unwrap();
    assert_eq!(value, serde_json::json!({ "message": "hello world" }));
    assert_eq!(
        record.metadata.get(GRPC_METHOD_KEY).and_then(|v| v.as_str()),
        Some("test.Greeter/SayHello")
    );
    assert_eq!(
        record.metadata.get(GRPC_MESSAGE_INDEX_KEY),
        Some(&serde_json::Value::Number(0.into()))
    );
}

#[tokio::test]
async fn server_streaming_returns_one_record_per_message() {
    let (endpoint, _calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(GrpcConfig::new(
        &endpoint,
        descriptors,
        "test.Greeter/LotsOfReplies",
    ))
    .unwrap();
    let records = connector.read().await.unwrap();
    assert_eq!(records.len(), 3);
    for (index, record) in records.iter().enumerate() {
        let value: serde_json::Value = serde_json::from_slice(&record.bytes).unwrap();
        assert_eq!(value, serde_json::json!({ "message": format!("reply {index}") }));
        assert_eq!(
            record.metadata.get(GRPC_MESSAGE_INDEX_KEY),
            Some(&serde_json::Value::Number((index as u64).into()))
        );
    }
}

#[tokio::test]
async fn server_reflection_resolves_descriptors() {
    let (endpoint, _calls) = spawn_server(true).await;
    let connector = GrpcConnector::new(GrpcConfig::new(
        &endpoint,
        DescriptorSource::ServerReflection,
        "test.Greeter/SayHello",
    ))
    .unwrap();
    let records = connector.read().await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&records[0].bytes).unwrap();
    assert_eq!(value, serde_json::json!({ "message": "hello world" }));
}

#[tokio::test]
async fn bearer_token_reaches_server() {
    let (endpoint, calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(
        GrpcConfig::new(&endpoint, descriptors, "test.Greeter/SayHello")
            .with_metadata_token("grpc-secret"),
    )
    .unwrap();
    connector.read().await.unwrap();
    let calls = calls.lock();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "/test.Greeter/SayHello");
    assert_eq!(calls[0].1.as_deref(), Some("Bearer grpc-secret"));
}

#[tokio::test]
async fn unknown_method_errors() {
    let (endpoint, _calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(GrpcConfig::new(
        &endpoint,
        descriptors,
        "test.Greeter/Missing",
    ))
    .unwrap();
    let error = connector.read().await.unwrap_err();
    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("Missing"));
}

#[tokio::test]
async fn client_streaming_is_rejected() {
    let (endpoint, _calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(GrpcConfig::new(
        &endpoint,
        descriptors,
        "test.Greeter/StreamIn",
    ))
    .unwrap();
    let error = connector.read().await.unwrap_err();
    assert!(error.to_string().contains("client-streaming"));
}

#[tokio::test]
async fn message_bound_errors() {
    let (endpoint, _calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(
        GrpcConfig::new(&endpoint, descriptors, "test.Greeter/LotsOfReplies")
            .with_max_messages(2),
    )
    .unwrap();
    let error = connector.read().await.unwrap_err();
    assert!(error.to_string().contains("message bound"));
}

#[tokio::test]
async fn response_byte_bound_errors() {
    let (endpoint, _calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(
        GrpcConfig::new(&endpoint, descriptors, "test.Greeter/SayHello")
            .with_max_response_bytes(4),
    )
    .unwrap();
    let error = connector.read().await.unwrap_err();
    assert!(error.to_string().contains("byte bound"));
}

#[tokio::test]
async fn grpc_status_error_surfaces() {
    let (endpoint, _calls) = spawn_server(false).await;
    let (_dir, descriptors) = descriptor_file();
    let connector = GrpcConnector::new(GrpcConfig::new(
        &endpoint,
        descriptors,
        "test.Greeter/Boom",
    ))
    .unwrap();
    let error = connector.read().await.unwrap_err();
    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("call failed"));
}
