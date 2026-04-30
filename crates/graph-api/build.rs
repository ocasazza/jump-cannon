fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/graph.proto");
    let mut config = prost_build::Config::new();
    config.compile_protos(&["proto/graph.proto"], &["proto/"])?;
    Ok(())
}
