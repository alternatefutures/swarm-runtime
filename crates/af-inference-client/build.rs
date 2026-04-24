// build.rs — prost + tonic code generation for af-inference-client
// Issue: alternatefutures/admin#132
//
// Generates Rust types from proto/inference.proto and proto/common.proto.
// Output goes to OUT_DIR (picked up by include! in src/generated.rs).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)   // client-only crate; server impl lives in the cluster (Python/Ray)
        .build_client(true)
        // Use owned types for prost (see rust-swarm-runtime.md §Tech_Stack — prost + owned types)
        .bytes(["."])
        .compile_protos(
            &["proto/inference.proto", "proto/common.proto"],
            &["proto/"],
        )?;
    Ok(())
}
