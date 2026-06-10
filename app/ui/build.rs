fn main() {
    let proto = "../../crates/graph-api/proto/graph.proto";
    println!("cargo:rerun-if-changed={proto}");
    prost_build::compile_protos(&[proto], &["../../crates/graph-api/proto"])
        .expect("prost-build: failed to compile graph.proto (is protoc on PATH? use the nix devshell)");
}
