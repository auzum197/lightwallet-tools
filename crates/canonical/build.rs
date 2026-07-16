use prost::Message;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let walletrpc = "../../proto/canonical/walletrpc";
    println!("cargo:rerun-if-changed={walletrpc}");

    let fds = protox::compile([&format!("{walletrpc}/service.proto")], [walletrpc])?;

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(out_dir.join("descriptor.bin"), fds.encode_to_vec())?;

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(fds)?;
    Ok(())
}
