use prost::Message;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let overlay = "../../proto/overlay";
    let walletrpc = "../../proto/canonical/walletrpc";
    println!("cargo:rerun-if-changed={overlay}");
    println!("cargo:rerun-if-changed={walletrpc}");

    // The overlay mirrors canonical service.proto, so canonical's copy must
    // never appear in the same compilation; walletrpc is on the include path
    // only for compact_formats.proto.
    let fds = protox::compile(
        [&format!("{overlay}/crosslink.proto")],
        [overlay, walletrpc],
    )?;

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(out_dir.join("descriptor.bin"), fds.encode_to_vec())?;

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(fds)?;
    Ok(())
}
