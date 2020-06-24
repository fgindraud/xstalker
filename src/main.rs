use gumdrop::Options;
use std::path::PathBuf;

/// Single-thread async runtime
mod star;

/// Metadata for the current active window
#[derive(Debug)]
pub struct ActiveWindowMetadata {
    title: Option<String>,
    class: Option<String>,
}

#[derive(Debug, Options)]
struct DaemonOptions {
    help: bool,
    debug: bool,

    #[options(free, required, help = "statistics database file location")]
    file: PathBuf,

    #[options(help = "time granularity (minutes, must divide 24h)")]
    granularity: Option<u64>,

    #[options(default = "60", help = "statistics write frequency (seconds)")]
    write_frequency: u64,

    #[options(free, help = "classifier shell command")]
    classifier: Vec<String>,
}

fn main() -> Result<(), anyhow::Error> {
    let options = DaemonOptions::parse_args_default_or_exit();

    println!("{:?}", options);

    Ok(())
}
