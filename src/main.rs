use gumdrop::Options;
use std::path::PathBuf;

mod utils;

/// Tracking of the active window state.
mod active_window;

/// Metadata for the current active window.
#[derive(Debug, PartialEq, Eq)]
pub struct ActiveWindowMetadata {
    /// Window id in X.
    id: u32,
    /// Title bar text. Absent for some windows (like root).
    title: Option<String>,
    /// Usually the program name. Absent for some windows (like root).
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
    dbg!(options);

    star::block_on(async {
        let mut watcher = active_window::ActiveWindowWatcher::new()?;
        loop {
            let metadata = watcher.active_window_change().await?;
            dbg!(metadata);
        }
        Ok::<(), anyhow::Error>(())
    })??;

    Ok(())
}

// Concurrently
//
// loop {
//   wait_xcb_event
//   read_new_state, get_time
//   enqueue(state, time_slice, duration), splitting if duration covers multiple time slices
//   send to classifier(async)
// }
// loop {
//   recv classification
//   dequeue(state, time_slice, duration)
//   update_db (class, time_slice, duration)
// }
// loop {
//   wait(write_frequency)
//   store_db {
//     rewrite last line
//     new last line
//     rewrite whole DB to add column and new data
//   }
// }
