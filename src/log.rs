//! This application's own log file.
//!
//! # Why a file
//!
//! A release build on Windows has no console — see `main`'s
//! `windows_subsystem` — so everything this application said on stderr was
//! said to nobody: why a Source would not open, why a recording stopped,
//! which encoder it fell back to. The file is where that goes now, and
//! stderr still gets a copy for whoever started it from a terminal.
//!
//! # Beside media-pp's, not in it
//!
//! Both are in [`crate::paths::logs_dir`], on the same terms — a file a day,
//! a week of them kept — but they are two files. `media-pp`'s logger is the
//! library's own by design: it takes no records from the application that
//! embeds it, and writes none into that application's logger. So a report
//! of what went wrong is the two read side by side, which is what the
//! timestamps on both are for.
//!
//! # What it takes
//!
//! This crate's records, at `info` unless `OBS_RS_LOG` says otherwise — the
//! same switch `OBS_RS_MEDIA_PP_LOG` is for the library. A dependency that
//! speaks `tracing` is let through at `warn` and above only: winit and wgpu
//! have a great deal to say at `info` about a window that is working.

use std::fmt;
use std::path::Path;

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// How many days of files are kept — the same as `media-pp`'s, so the two
/// go back equally far.
const DAYS_KEPT: usize = 7;

/// Keeps the log writing. Dropping it flushes what is queued and stops the
/// thread that writes, so it is held for the whole of `main`.
pub struct LogGuard {
    _worker: WorkerGuard,
}

/// Starts writing this application's log into `directory`, or says on stderr
/// why it could not — which is then the only place anything is said.
pub fn start(directory: &Path) -> Option<LogGuard> {
    let level = level_from(std::env::var("OBS_RS_LOG").ok().as_deref());
    let appender = match RollingFileAppender::builder()
        .filename_prefix("obs-rs")
        .filename_suffix("log")
        .rotation(Rotation::DAILY)
        .max_log_files(DAYS_KEPT)
        .build(directory)
    {
        Ok(appender) => appender,
        Err(error) => {
            eprintln!("obs-rs logging is off: {error}");
            return None;
        }
    };
    // Off the calling thread: an engine loop or a UI pass must never wait on
    // a disk. A line that cannot be queued is dropped rather than waited for.
    let (writer, worker) = tracing_appender::non_blocking(appender);

    let targets = Targets::new()
        .with_default(LevelFilter::WARN)
        .with_target(env!("CARGO_CRATE_NAME"), level);
    let file = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_timer(LocalClock)
        .with_thread_names(true)
        .with_filter(targets.clone());
    let terminal = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_timer(LocalClock)
        .with_filter(targets);
    if let Err(error) = tracing_subscriber::registry()
        .with(file)
        .with(terminal)
        .try_init()
    {
        eprintln!("obs-rs logging is off: {error}");
        return None;
    }
    Some(LogGuard { _worker: worker })
}

/// What `OBS_RS_LOG` asks for. Anything it does not recognise is `info`,
/// which is what a normal run wants — and the same default the library's
/// own switch has.
fn level_from(setting: Option<&str>) -> LevelFilter {
    match setting.map(str::to_ascii_lowercase).as_deref() {
        Some("off") => LevelFilter::OFF,
        Some("error") => LevelFilter::ERROR,
        Some("warn") => LevelFilter::WARN,
        Some("debug") => LevelFilter::DEBUG,
        Some("trace") => LevelFilter::TRACE,
        _ => LevelFilter::INFO,
    }
}

/// Stamps each record with the user's own wall clock, as the file names of
/// recordings are — see [`crate::clock`] for why that is not simply
/// `OffsetDateTime::now_local`, which answers nothing in a process with a
/// second thread and would leave every line in UTC.
struct LocalClock;

impl FormatTime for LocalClock {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        let now = crate::clock::now_local();
        write!(
            writer,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The switch reads like the library's: the five levels by name, `off`,
    /// and anything else taken as the default rather than as an error — a
    /// typo in an environment variable must not cost a run its log.
    #[test]
    fn the_level_is_read_by_name_and_defaults_to_info() {
        assert_eq!(level_from(Some("trace")), LevelFilter::TRACE);
        assert_eq!(level_from(Some("Debug")), LevelFilter::DEBUG);
        assert_eq!(level_from(Some("off")), LevelFilter::OFF);
        assert_eq!(level_from(Some("loud")), LevelFilter::INFO);
        assert_eq!(level_from(None), LevelFilter::INFO);
    }
}
