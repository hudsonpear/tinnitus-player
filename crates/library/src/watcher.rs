//! Optional filesystem watching.
//!
//! A change never triggers a full rescan. The watcher reports the paths that
//! moved and the caller re-reads only those, which is what keeps a library of
//! tens of thousands of tracks usable while files are being copied into it.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::{Context as _, Result};
use notify::{RecursiveMode, event::EventKind};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

/// How long to wait for a burst of filesystem events to settle. Copying an album
/// in fires hundreds of events; this collapses them into one batch.
const DEBOUNCE: Duration = Duration::from_secs(2);

/// A batch of paths that changed under the watched folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// Files created or modified; re-read these.
    pub touched: Vec<PathBuf>,
    /// Files or directories that went away; mark these missing.
    pub gone: Vec<PathBuf>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty() && self.gone.is_empty()
    }
}

/// Watches folders and posts `Changes` on a channel. Dropping it stops watching.
pub struct Watcher {
    debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    changes: Receiver<Changes>,
}

impl Watcher {
    pub fn new() -> Result<Self> {
        let (sender, changes) = channel();
        let debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
            forward(&sender, result);
        })
        .context("cannot start the filesystem watcher")?;

        Ok(Self { debouncer, changes })
    }

    /// Starts watching a folder tree. Watching the same folder twice is harmless.
    pub fn watch(&mut self, path: &std::path::Path) -> Result<()> {
        self.debouncer
            .watch(path, RecursiveMode::Recursive)
            .with_context(|| format!("cannot watch {}", path.display()))
    }

    pub fn unwatch(&mut self, path: &std::path::Path) -> Result<()> {
        self.debouncer
            .unwatch(path)
            .with_context(|| format!("cannot stop watching {}", path.display()))
    }

    /// Non-blocking. Returns every batch that has arrived since the last call,
    /// flattened into one.
    pub fn drain(&self) -> Changes {
        let mut all = Changes {
            touched: vec![],
            gone: vec![],
        };
        while let Ok(batch) = self.changes.try_recv() {
            all.touched.extend(batch.touched);
            all.gone.extend(batch.gone);
        }
        all.touched.sort();
        all.touched.dedup();
        all.gone.sort();
        all.gone.dedup();
        all
    }

    /// Blocks until the next batch, or the timeout. Used by the background task
    /// that owns the watcher.
    pub fn wait(&self, timeout: Duration) -> Option<Changes> {
        self.changes.recv_timeout(timeout).ok()
    }
}

fn forward(sender: &Sender<Changes>, result: DebounceEventResult) {
    let events = match result {
        Ok(events) => events,
        Err(errors) => {
            for error in errors {
                log::warn!("watcher: {error}");
            }
            return;
        }
    };

    let mut changes = Changes {
        touched: vec![],
        gone: vec![],
    };
    for event in events {
        // A rename shows up as a remove plus a create; both halves are reported
        // and the scanner's move detection stitches them back together.
        let bucket = match event.kind {
            EventKind::Remove(_) => &mut changes.gone,
            EventKind::Create(_) | EventKind::Modify(_) => &mut changes.touched,
            _ => continue,
        };
        for path in &event.paths {
            if crate::metadata::is_audio_file(path) || path.is_dir() {
                bucket.push(path.clone());
            }
        }
    }

    if !changes.is_empty() {
        sender.send(changes).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_is_empty_when_nothing_happened() {
        let watcher = Watcher::new().unwrap();
        assert!(watcher.drain().is_empty());
    }

    #[test]
    fn forward_keeps_audio_and_drops_the_rest() {
        let (sender, receiver) = channel();
        let audio = PathBuf::from("a.flac");
        let text = PathBuf::from("notes.txt");

        forward(
            &sender,
            Ok(vec![
                debounced(EventKind::Create(notify::event::CreateKind::File), &audio),
                debounced(EventKind::Create(notify::event::CreateKind::File), &text),
            ]),
        );

        let batch = receiver.try_recv().unwrap();
        assert_eq!(batch.touched, vec![audio]);
        assert!(batch.gone.is_empty());
    }

    #[test]
    fn forward_separates_removals() {
        let (sender, receiver) = channel();
        let audio = PathBuf::from("gone.mp3");
        forward(
            &sender,
            Ok(vec![debounced(
                EventKind::Remove(notify::event::RemoveKind::File),
                &audio,
            )]),
        );

        let batch = receiver.try_recv().unwrap();
        assert_eq!(batch.gone, vec![audio]);
        assert!(batch.touched.is_empty());
    }

    fn debounced(kind: EventKind, path: &std::path::Path) -> notify_debouncer_full::DebouncedEvent {
        let mut event = notify::Event::new(kind);
        event.paths.push(path.to_path_buf());
        notify_debouncer_full::DebouncedEvent::new(event, std::time::Instant::now())
    }
}
