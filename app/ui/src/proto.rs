//! Protobuf-generated message types. The single schema lives in
//! crates/graph-api/proto/graph.proto (shared with the server); prost-build
//! emits Rust here at build time.

#![allow(dead_code)] // MetaSummary/FieldBucket aren't consumed by a panel yet

include!(concat!(env!("OUT_DIR"), "/jumpcannon.graph.rs"));
