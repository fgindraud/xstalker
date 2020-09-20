use anyhow::{Context, Error};
use gumdrop::Options;
use std::convert::TryFrom;
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

    let (mut classifier_in, mut classifier_out) = classifier::spawn(&options.classifier)?;
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
                if !ignore_span(&elapsed_span) {
                    classifier_in.classify(&span_metadata, elapsed_span)?
                }

                span_start_monotonic = span_end_monotonic;
                span_start_user = span_end_user;
                span_metadata = new_metadata;
            }
        });
        let update_db = star::spawn(async move {
            loop {
                let (classification, time_span) = classifier_out.classified().await?;

                // TODO
                dbg!(classification);
            }
        });

        utils::TryJoin::new(watch_window, update_db).await
    })
    .with_context(|| "async runtime error")?
}

/// Filter time spans that make no sense.
/// The goal is to eliminate crazy time spans resulting from timezone changes, suspend to disk mode, etc.
/// Currently filters out negative time spans, and those with large difference between user and monotonic durations.
fn ignore_span(span: &TimeSpan) -> bool {
    if span.duration == Duration::new(0, 0) {
        return true;
    }

    // Negative duration is a sure sign of timezone change
    if span.end <= span.start {
        return true;
    }

    // Check that monotonic and user time match (with some error allowed).
    let user_time_duration = match Duration::try_from(span.end - span.start) {
        Ok(d) => d,
        Err(_) => return true, // Conversion fail is a sign of crazy value
    };
    let relative_error = |a, b| 2. * (a - b) / (a + b);
    let error = relative_error(
        user_time_duration.as_secs_f32(),
        span.duration.as_secs_f32(),
    );
    error.abs() > 0.1
}

// Concurrently
//
// loop {
//   wait_xcb_event
//   read_new_state, get_time
//   send to classifier
// }
// loop {
//   recv classification
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
