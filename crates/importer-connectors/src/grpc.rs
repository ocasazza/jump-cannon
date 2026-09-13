//! gRPC byte connector — dynamic invocation via prost-reflect.
//!
//! Runtime configuration supplies the endpoint, a descriptor source (an
//! encoded `FileDescriptorSet` file, or gRPC server reflection v1), and a
//! fully-qualified method (`package.Service/Method`). The connector sends an
//! empty request message and serializes each response message to canonical
//! protobuf JSON — one [`SourceRecord`] per message. Unary and
//! server-streaming methods only; client-streaming is rejected at read time.
//!
//! The optional metadata token is runtime configuration (never a package
//! field), sent as an `authorization: Bearer` header, and redacted from
//! `Debug`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use data_loader::{
    Capability, Effect, ImportError, ImportFuture, ImportProgress, SourceConnector, SourceRecord,
    Transport,
};
use prost_reflect::{DescriptorPool, DynamicMessage, MethodDescriptor};
use prost::Message as _;
use tonic::codec::{Codec, DecodeBuf, EncodeBuf};
use tonic::codegen::http;
use tonic::transport::{Channel, Endpoint};

/// Record metadata key carrying the fully-qualified method name.
pub const GRPC_METHOD_KEY: &str = "grpc.method";
/// Record metadata key carrying the zero-based response message index.
pub const GRPC_MESSAGE_INDEX_KEY: &str = "grpc.message_index";

/// Media type of every emitted record: canonical protobuf JSON.
pub const CONTENT_TYPE: &str = "application/json";

/// Default bound on response messages per read.
pub const DEFAULT_MAX_MESSAGES: usize = 1024;
/// Default bound on total serialized JSON bytes per read: 64 MiB.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// How the connector obtains the protobuf descriptors for the method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorSource {
    /// Path to an encoded `prost_types::FileDescriptorSet` (protoc
    /// `--descriptor_set_out`; include imports or the well-known types your
    /// service references).
    File(PathBuf),
    /// Query the server's reflection service (v1) for the method's file and
    /// its transitive dependencies.
    ServerReflection,
}

/// Runtime configuration for one gRPC source.
#[derive(Clone)]
pub struct GrpcConfig {
    pub endpoint: String,
    pub descriptors: DescriptorSource,
    /// Fully-qualified method: `package.Service/Method`.
    pub method: String,
    pub metadata_token: Option<String>,
    pub max_messages: usize,
    pub max_response_bytes: usize,
}

impl fmt::Debug for GrpcConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcConfig")
            .field("endpoint", &self.endpoint)
            .field("descriptors", &self.descriptors)
            .field("method", &self.method)
            .field("metadata_token", &self.metadata_token.as_ref().map(|_| "<redacted>"))
            .field("max_messages", &self.max_messages)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

impl GrpcConfig {
    pub fn new(endpoint: impl Into<String>, descriptors: DescriptorSource, method: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            descriptors,
            method: method.into(),
            metadata_token: None,
            max_messages: DEFAULT_MAX_MESSAGES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    pub fn with_metadata_token(mut self, token: impl Into<String>) -> Self {
        self.metadata_token = Some(token.into());
        self
    }

    pub fn with_max_messages(mut self, max_messages: usize) -> Self {
        self.max_messages = max_messages;
        self
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// `(service, method)` split of the fully-qualified method name.
    fn split_method(&self) -> Result<(&str, &str), ImportError> {
        self.method
            .rsplit_once('/')
            .filter(|(service, method)| !service.is_empty() && !method.is_empty())
            .ok_or_else(|| ImportError::InvalidDescriptor {
                message: format!(
                    "grpc: method must be fully qualified as package.Service/Method, got {:?}",
                    self.method
                ),
            })
    }

    pub fn validate(&self) -> Result<(), ImportError> {
        self.split_method()?;
        Endpoint::from_shared(self.endpoint.clone()).map_err(|error| {
            ImportError::InvalidDescriptor {
                message: format!("grpc: endpoint {:?} is invalid: {error}", self.endpoint),
            }
        })?;
        if self.max_messages == 0 {
            return Err(ImportError::InvalidDescriptor {
                message: "grpc: max_messages must be at least 1".to_string(),
            });
        }
        if self.max_response_bytes == 0 {
            return Err(ImportError::InvalidDescriptor {
                message: "grpc: max_response_bytes must be at least 1".to_string(),
            });
        }
        Ok(())
    }
}

/// Effectful source connector for one gRPC method. Emits one
/// [`SourceRecord`] per response message, serialized to canonical protobuf
/// JSON.
pub struct GrpcConnector {
    config: GrpcConfig,
}

impl fmt::Debug for GrpcConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcConnector")
            .field("config", &self.config)
            .finish()
    }
}

impl GrpcConnector {
    pub fn new(config: GrpcConfig) -> Result<Self, ImportError> {
        config.validate()?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &GrpcConfig {
        &self.config
    }

    /// Capability scope: `endpoint/package.Service/Method`.
    fn scope(&self) -> String {
        format!(
            "{}/{}",
            self.config.endpoint.trim_end_matches('/'),
            self.config.method
        )
    }
}

impl SourceConnector for GrpcConnector {
    fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        match effect {
            Effect::Read | Effect::Watch => {
                vec![Capability::new(effect, Transport::Grpc, self.scope())]
            }
            _ => Vec::new(),
        }
    }

    fn read<'a>(
        &'a self,
        _progress: &'a dyn ImportProgress,
    ) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
        Box::pin(async move {
            let scope = self.scope();
            let fail = |message: String| ImportError::SourceRead {
                origin: scope.clone(),
                message,
            };

            let (service_name, method_name) = self.config.split_method()?;
            let channel = Endpoint::from_shared(self.config.endpoint.clone())
                .map_err(|error| fail(format!("invalid endpoint: {error}")))?
                .connect()
                .await
                .map_err(|error| fail(format!("connect failed: {error}")))?;
            let pool = load_descriptors(&channel, &self.config.descriptors, service_name).await?;
            let service = pool
                .get_service_by_name(service_name)
                .ok_or_else(|| fail(format!("service {service_name} not in descriptor pool")))?;
            let method = service
                .methods()
                .find(|method| method.name() == method_name)
                .ok_or_else(|| {
                    fail(format!(
                        "method {method_name} not found on service {service_name}"
                    ))
                })?;
            if method.is_client_streaming() {
                return Err(fail(format!(
                    "method {} is client-streaming; only unary and server-streaming are supported",
                    self.config.method
                )));
            }

            let mut request = tonic::Request::new(DynamicMessage::new(method.input()));
            if let Some(token) = &self.config.metadata_token {
                let value = format!("Bearer {token}").parse().map_err(|error| {
                    ImportError::InvalidDescriptor {
                        message: format!("grpc: metadata token is not a valid header value: {error}"),
                    }
                })?;
                request.metadata_mut().insert("authorization", value);
            }

            let path = http::uri::PathAndQuery::from_maybe_shared(format!(
                "/{service_name}/{method_name}"
            ))
            .map_err(|error| fail(format!("invalid method path: {error}")))?;
            let codec = DynamicCodec { method };
            let mut grpc = tonic::client::Grpc::new(channel);
            grpc.ready().await.map_err(|error| {
                fail(format!("channel not ready: {error}"))
            })?;

            let mut records = Vec::new();
            let mut total_bytes = 0usize;
            let mut push = |index: u64,
                            message: DynamicMessage,
                            records: &mut Vec<SourceRecord>|
             -> Result<(), ImportError> {
                if records.len() >= self.config.max_messages {
                    return Err(fail(format!(
                        "response exceeds the {} message bound",
                        self.config.max_messages
                    )));
                }
                let bytes = serde_json::to_vec(&message).map_err(|error| {
                    fail(format!("response message {index} is not JSON-serializable: {error}"))
                })?;
                total_bytes += bytes.len();
                if total_bytes > self.config.max_response_bytes {
                    return Err(fail(format!(
                        "response exceeds the {} byte bound",
                        self.config.max_response_bytes
                    )));
                }
                let mut metadata = BTreeMap::new();
                metadata.insert(
                    GRPC_METHOD_KEY.to_string(),
                    serde_json::Value::String(self.config.method.clone()),
                );
                metadata.insert(
                    GRPC_MESSAGE_INDEX_KEY.to_string(),
                    serde_json::Value::Number(index.into()),
                );
                records.push(SourceRecord {
                    origin: format!("{scope}#{index}"),
                    content_type: CONTENT_TYPE.to_string(),
                    bytes,
                    metadata,
                });
                Ok(())
            };

            if codec.method.is_server_streaming() {
                let response = grpc
                    .server_streaming(request, path, codec)
                    .await
                    .map_err(|status| fail(format!("call failed: {}", status.message())))?;
                let mut stream = response.into_inner();
                let mut index = 0u64;
                while let Some(message) = stream
                    .message()
                    .await
                    .map_err(|status| fail(format!("stream failed: {}", status.message())))?
                {
                    push(index, message, &mut records)?;
                    index += 1;
                }
            } else {
                let response = grpc
                    .unary(request, path, codec)
                    .await
                    .map_err(|status| fail(format!("call failed: {}", status.message())))?;
                push(0, response.into_inner(), &mut records)?;
            }
            Ok(records)
        })
    }
}

/// Codec for dynamically-described messages. Encoding mirrors tonic's
/// prost codec: the message bytes, with framing handled by tonic.
struct DynamicCodec {
    method: MethodDescriptor,
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder {
            output: self.method.output(),
        }
    }
}

struct DynamicEncoder;

impl tonic::codec::Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn encode(
        &mut self,
        item: Self::Item,
        dst: &mut EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        item.encode(dst)
            .map_err(|error| tonic::Status::internal(format!("dynamic encode failed: {error}")))
    }
}

struct DynamicDecoder {
    output: prost_reflect::MessageDescriptor,
}

impl tonic::codec::Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        DynamicMessage::decode(self.output.clone(), src)
            .map(Some)
            .map_err(|error| tonic::Status::internal(format!("dynamic decode failed: {error}")))
    }
}

async fn load_descriptors(
    channel: &Channel,
    source: &DescriptorSource,
    service_name: &str,
) -> Result<DescriptorPool, ImportError> {
    match source {
        DescriptorSource::File(path) => {
            let bytes = std::fs::read(path).map_err(|error| ImportError::SourceRead {
                origin: path.display().to_string(),
                message: format!("descriptor set unreadable: {error}"),
            })?;
            let set = prost_types::FileDescriptorSet::decode(bytes.as_slice()).map_err(
                |error| ImportError::SourceRead {
                    origin: path.display().to_string(),
                    message: format!("descriptor set is not a FileDescriptorSet: {error}"),
                },
            )?;
            pool_from_set(set).map_err(|message| ImportError::SourceRead {
                origin: path.display().to_string(),
                message,
            })
        }
        DescriptorSource::ServerReflection => {
            let set = fetch_reflection_set(channel, service_name).await?;
            pool_from_set(set).map_err(|message| ImportError::SourceRead {
                origin: format!("{service_name} via server reflection"),
                message,
            })
        }
    }
}

/// Build a pool from a descriptor set, tolerating arbitrary file ordering by
/// adding files in dependency order.
fn pool_from_set(
    set: prost_types::FileDescriptorSet,
) -> Result<DescriptorPool, String> {
    let mut files = set.file;
    let names: std::collections::HashMap<String, usize> = files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| file.name.clone().map(|name| (name, index)))
        .collect();
    // Stable topological order: dependencies before dependents.
    let mut ordered: Vec<prost_types::FileDescriptorProto> = Vec::with_capacity(files.len());
    let mut placed = vec![false; files.len()];
    let mut visiting = vec![false; files.len()];
    fn visit(
        index: usize,
        files: &[prost_types::FileDescriptorProto],
        names: &std::collections::HashMap<String, usize>,
        placed: &mut [bool],
        visiting: &mut [bool],
        ordered: &mut Vec<prost_types::FileDescriptorProto>,
    ) -> Result<(), String> {
        if placed[index] {
            return Ok(());
        }
        if visiting[index] {
            return Err(format!(
                "cyclic file dependency at {:?}",
                files[index].name.as_deref().unwrap_or("<unnamed>")
            ));
        }
        visiting[index] = true;
        for dependency in &files[index].dependency {
            if let Some(&dependency_index) = names.get(dependency) {
                visit(dependency_index, files, names, placed, visiting, ordered)?;
            }
        }
        visiting[index] = false;
        placed[index] = true;
        ordered.push(files[index].clone());
        Ok(())
    }
    for index in 0..files.len() {
        visit(index, &files, &names, &mut placed, &mut visiting, &mut ordered)?;
    }
    files = ordered;
    let set = prost_types::FileDescriptorSet { file: files };
    DescriptorPool::decode(set.encode_to_vec().as_slice())
        .map_err(|error| format!("descriptor pool rejected the set: {error}"))
}

/// Fetch the file containing `service_name` and its transitive dependencies
/// via gRPC server reflection v1.
async fn fetch_reflection_set(
    channel: &Channel,
    service_name: &str,
) -> Result<prost_types::FileDescriptorSet, ImportError> {
    use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
    use tonic_reflection::pb::v1::server_reflection_response::MessageResponse;
    use tonic_reflection::pb::v1::{server_reflection_client::ServerReflectionClient, ServerReflectionRequest};

    let origin = format!("reflection for {service_name}");
    let fail = |message: String| ImportError::SourceRead {
        origin: origin.clone(),
        message,
    };
    let mut client = ServerReflectionClient::new(channel.clone());
    let request = ServerReflectionRequest {
        host: String::new(),
        message_request: Some(MessageRequest::FileContainingSymbol(
            service_name.to_string(),
        )),
    };
    let mut stream = client
        .server_reflection_info(tokio_stream::once(request))
        .await
        .map_err(|status| fail(format!("reflection request failed: {}", status.message())))?
        .into_inner();
    let mut files = Vec::new();
    while let Some(response) = stream
        .message()
        .await
        .map_err(|status| fail(format!("reflection stream failed: {}", status.message())))?
    {
        match response.message_response {
            Some(MessageResponse::FileDescriptorResponse(descriptor)) => {
                for bytes in descriptor.file_descriptor_proto {
                    files.push(
                        prost_types::FileDescriptorProto::decode(bytes.as_slice()).map_err(
                            |error| fail(format!("reflection returned an undecodable file: {error}")),
                        )?,
                    );
                }
            }
            Some(MessageResponse::ErrorResponse(error)) => {
                return Err(fail(format!(
                    "reflection error {}: {}",
                    error.error_code,
                    error.error_message
                )));
            }
            _ => {}
        }
    }
    if files.is_empty() {
        return Err(fail("reflection returned no files".to_string()));
    }
    Ok(prost_types::FileDescriptorSet { file: files })
}

#[cfg(test)]
mod tests;
