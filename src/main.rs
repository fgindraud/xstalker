use anyhow::{Context, Error};
use gumdrop::Options;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use time::OffsetDateTime;

/// Tracking of the active window state.
mod active_window;

/// Interact with external process to classify timeslots
mod classifier;

mod utils;

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

/// Span of positive Duration anchored to use local time.
#[derive(Debug)]
pub struct TimeSpan {
    start: OffsetDateTime,
    end: OffsetDateTime,
    /// Real duration, using a monotonous clock.
    duration: Duration,
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

fn main() -> Result<(), Error> {
    let options = DaemonOptions::parse_args_default_or_exit();
    dbg!(&options);

    let (mut classifier_in, classifier_out) = classifier::spawn(&options.classifier)?;
    let mut watcher = active_window::ActiveWindowWatcher::new()?;

    star::block_on(async move {
        let watch_window = star::spawn(async move {
            let mut span_start_monotonic = Instant::now();
            let mut span_start_user = OffsetDateTime::now_local();
            let mut span_metadata = watcher.cached_metadata();
            loop {
                let new_metadata = watcher.active_window_change().await?;
                let span_end_monotonic = Instant::now();
                let span_end_user = OffsetDateTime::now_local();

                let elapsed_span = TimeSpan {
                    start: span_start_user,
                    end: span_end_user,
                    duration: span_end_monotonic - span_start_monotonic,
                };
                classifier_in.classify(&span_metadata, elapsed_span)?;

                span_start_monotonic = span_end_monotonic;
                span_start_user = span_end_user;
                span_metadata = new_metadata;
            }
        });
        let update_db = star::spawn(async move {
            // TODO
            Ok::<(), Error>(())
        });

        utils::TryJoin::new(watch_window, update_db).await
    })
    .with_context(|| "async runtime error")?
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
