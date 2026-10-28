use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

mod gemdos;

use gemdos::Gemdos;

#[derive(Parser)]
#[command(
    name = "mkfs-gemdos",
    version,
    about = "Create GEMDOS (Atari TOS) FAT12 disk images"
)]
struct Cli {
    /// Populate from this directory tree (recursive)
    #[arg(short = 'd', long)]
    directory: Option<PathBuf>,

    /// Floppy size in KB: 360, 720, 1200, 1440, or 2880 (default: 720)
    #[arg(short = 's', long, default_value_t = 720)]
    size_kb: u32,

    /// Output filename
    filename: PathBuf,
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut g = Gemdos::new(cli.size_kb)?;
    if let Some(src) = &cli.directory {
        g.populate_root(src)?;
    }
    let stats = g.write_to(&cli.filename)?;

    println!(
        "Created GEMDOS image: {} ({}KB, {} clusters, FAT12, non-bootable)",
        cli.filename.display(),
        cli.size_kb,
        stats.total_clusters,
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mkfs-gemdos: {e:#}");
            ExitCode::FAILURE
        }
    }
}
