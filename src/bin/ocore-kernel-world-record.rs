use clap::Parser;
use o_lang::kernel_world::VerifiedKernelWorld;
use o_lang::live_system::manifest::{VerifiedPackage, MAX_MANIFEST_BYTES};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "ocore-kernel-world-record",
    version,
    about = "Encode a verified kernel-world package as a native admission record"
)]
struct Cli {
    /// Strict ocore.package/v1 manifest.
    #[arg(long)]
    manifest: PathBuf,

    /// Complete package payload directory.
    #[arg(long)]
    payload: PathBuf,

    /// Destination for the deterministic normal-form record.
    #[arg(short, long)]
    output: PathBuf,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let metadata = fs::metadata(&cli.manifest)?;
    if metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(format!("package manifest exceeds {} bytes", MAX_MANIFEST_BYTES).into());
    }
    let manifest = fs::read_to_string(&cli.manifest)?;
    let package = VerifiedPackage::load(&manifest, &cli.payload)?;
    let world = VerifiedKernelWorld::from_package(&package)?;
    let record = world.encode_native_record()?;
    if let Some(parent) = cli.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&cli.output, record.bytes())?;
    println!("record-bytes: {}", record.bytes().len());
    println!("record: {}", cli.output.display());
    println!("record-sha256: {}", record.sha256_hex());
    println!("package-digest: {}", record.package_digest().as_hex());
    Ok(())
}
