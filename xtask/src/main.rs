use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod demo;
mod load;
mod release;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Parser)]
#[command(about = "Local synthetic QA, reproducible load checks and release tools")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the loopback-only, synthetic demo ledger.
    Demo(demo::Options),
    /// Run a deterministic synthetic HTTP benchmark or mixed load.
    Load(load::Options),
    /// Package tracked source and the built frontend; never runtime state.
    Package { output: PathBuf },
    /// Extract a version's Chinese release notes.
    ReleaseNotes { tag: String, output: PathBuf },
    /// Check an isolated, network-disabled Docker image.
    SmokeImage { image: String, revision: String },
}

#[tokio::main]
async fn main() {
    let result = match Args::parse().command {
        Command::Demo(options) => demo::run(options).await,
        Command::Load(options) => load::run(options).await,
        Command::Package { output } => release::package(&output),
        Command::ReleaseNotes { tag, output } => release::notes(&tag, &output),
        Command::SmokeImage { image, revision } => release::smoke(&image, &revision).await,
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
