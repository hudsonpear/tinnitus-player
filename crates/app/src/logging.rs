//! Structured logging to a rolling file, plus stderr in debug builds.
//!
//! Quiet by default: normal playback writes nothing. Everything that goes wrong
//! writes once, with the context needed to act on it.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

/// The log is truncated when it passes this, so it cannot grow without bound on
/// a machine that is never restarted.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub fn path() -> PathBuf {
    library::data_dir().join("tinnitus.log")
}

pub fn init() {
    let level = match cfg!(debug_assertions) {
        true => log::LevelFilter::Debug,
        false => log::LevelFilter::Info,
    };

    let file = open();
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(level)
        // GPUI and its dependencies are chatty at debug level and none of it is
        // ours to act on.
        .filter_module("gpui", log::LevelFilter::Warn)
        .filter_module("blade_graphics", log::LevelFilter::Warn)
        .filter_module("naga", log::LevelFilter::Warn)
        // lofty narrates every tag it parses, and a scan parses thousands of
        // files. Its warnings are per-file quirks of the tags themselves
        // ("using bitrate to estimate duration", "replaced frame with ID COMM")
        // — the scanner already reports the files it could not read, so only
        // hard errors out of the parser earn a line.
        .filter_module("lofty", log::LevelFilter::Error)
        .filter_module("symphonia", log::LevelFilter::Warn)
        .filter_module("notify", log::LevelFilter::Info)
        .filter_module("notify_debouncer_full", log::LevelFilter::Info)
        .parse_default_env();

    if let Some(file) = file {
        let file = Mutex::new(file);
        builder.format(move |buffer, record| {
            let line = format!(
                "{:<5} {} {}\n",
                record.level(),
                record.target(),
                record.args()
            );
            if let Ok(mut file) = file.lock() {
                file.write_all(line.as_bytes()).ok();
            }
            // Debug builds also print, so `cargo run` shows what is happening.
            match cfg!(debug_assertions) {
                true => write!(buffer, "{line}"),
                false => Ok(()),
            }
        });
    }

    if builder.try_init().is_err() {
        // Another init already ran (a test harness, say). Not worth failing over.
    }
    catch_panics();
}

/// Writes panics to the log before the process goes.
///
/// Without this a panic reaches stderr and nowhere else, and a windowed build
/// has no stderr — so the app appears to vanish and the log's last line is
/// whatever happened to be written before it. A panic raised inside a Win32
/// window procedure is worse still: unwinding out of an `extern "system"` call
/// aborts, so not even a `Drop` runs to leave a trace.
fn catch_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let where_ = info
            .location()
            .map(|at| format!("{}:{}", at.file(), at.line()))
            .unwrap_or_else(|| "an unknown place".to_owned());
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_owned());
        log::error!(
            "tinnitus panicked at {where_}: {what}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        previous(info);
    }));
}

fn open() -> Option<std::fs::File> {
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) > MAX_BYTES {
        std::fs::remove_file(&path).ok();
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_lives_beside_the_library() {
        assert_eq!(path().parent(), Some(library::data_dir().as_path()));
        assert_eq!(
            path().file_name().and_then(|name| name.to_str()),
            Some("tinnitus.log")
        );
    }
}
