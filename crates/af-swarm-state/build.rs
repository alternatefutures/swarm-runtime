// build.rs — prost codegen for af-swarm-state proto definitions
// Issue: alternatefutures/admin#134

fn main() -> Result<(), Box<dyn std::error::Error>> {
    prost_build::Config::new()
        // Generate well-known types (google.protobuf.Timestamp)
        .compile_well_known_types()
        // Output to src/generated/ (gitignored)
        .out_dir("src/generated")
        .compile_protos(
            &[
                "proto/common.proto",
                "proto/encryption.proto",
                "proto/agent_state.proto",
                "proto/working_memory.proto",
                "proto/conversation.proto",
                "proto/tool_call_log.proto",
                "proto/checkpoint.proto",
            ],
            &["proto/"],
        )?;
    Ok(())
}
