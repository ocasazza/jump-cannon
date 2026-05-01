fn main() {
    println!("cargo:rerun-if-changed=../graph-api/proto/graph.proto");
    let mut config = prost_build::Config::new();
    config
        .compile_protos(
            &["../graph-api/proto/graph.proto"],
            &["../graph-api/proto/"],
        )
        .expect("compile graph.proto");
}
