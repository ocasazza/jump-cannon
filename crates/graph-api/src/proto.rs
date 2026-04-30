//! Protobuf-generated message types. Single schema lives in proto/graph.proto;
//! prost-build emits Rust here at build time.
//
// Future: keep this file as-is — schema lives next to the .proto, not here.

include!(concat!(env!("OUT_DIR"), "/jumpcannon.graph.rs"));
