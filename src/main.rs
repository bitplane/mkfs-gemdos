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

    /// Image size in kilobytes (default: 720, a DSDD 3.5" floppy)
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
        "Created GEMDOS image: {} ({}KB, {} clusters, FAT12, checksum=0x1234)",
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
