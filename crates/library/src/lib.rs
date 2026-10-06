//! The music library: SQLite storage, tag reading, the scanner, artwork, and
//! filesystem watching.
//!
//! This crate knows nothing about GPUI and nothing about audio playback. It
//! deals in paths, rows, and plain data. Everything it does is safe to run on a
//! background thread, and nothing in it should ever be called from the UI
//! thread directly.

pub mod artwork;
pub mod db;
pub mod metadata;
pub mod models;
pub mod scanner;
pub mod smart;
pub mod watcher;

pub use artwork::{ArtworkCache, ThumbSize};
pub use db::{Db, ScannedTrack, Upsert};
pub use metadata::{TagEdit, is_audio_file};
pub use models::*;
pub use scanner::{Cancel, scan, sync_paths};
pub use watcher::{Changes, Watcher};

use std::path::{Path, PathBuf};

/// Sends a file to the Recycle Bin, rather than unlinking it.
///
/// Deleting somebody's music is the one thing this app does that they cannot
/// undo, and "it went to the bin" is the difference between a mis-click that
/// costs a moment and one that costs a file. The platform's own bin is used, so
/// restoring works from Explorer the way the user already expects.
///
/// A file that is not there is not an error: it is already in the state the
/// caller asked for.
pub fn delete_to_trash(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    // `trash::Error` is not `std::error::Error`, so it cannot be `?`'d into
    // `anyhow` on its own.
    trash::delete(path).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Where the library database and artwork cache live. Everything the app stores
/// is under one directory so a user can back it up or delete it in one go.
///
/// `TINNITUS_DATA_DIR` overrides it, which is what makes a portable install — and
/// a run against a throwaway library — possible.
pub fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("TINNITUS_DATA_DIR") {
        return PathBuf::from(path);
    }
    settle_data_dir(dirs_data_dir().join(APP_DIR), &legacy_dirs())
}

/// The directory name the store lives under. Capitalised on the platforms whose
/// users see it in a file manager, lower-case where the convention is otherwise.
#[cfg(any(windows, target_os = "macos"))]
const APP_DIR: &str = "Tinnitus";
#[cfg(all(unix, not(target_os = "macos")))]
const APP_DIR: &str = "tinnitus";

/// Where the store used to live: before Tinnitus had its name, a directory
/// called `mp3rust`. On Windows that sat in the local profile rather than the
/// roaming one.
fn legacy_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        local.into_iter().map(|root| root.join("mp3rust")).collect()
    }
    #[cfg(not(windows))]
    {
        vec![dirs_data_dir().join("mp3rust")]
    }
}

/// Settles on one directory: the current one if it is there, otherwise the first
/// older one that is, moved into place.
///
/// The move is one `rename` of the whole directory, so the database, the artwork
/// cache and the settings arrive together or not at all. A move that fails costs
/// nothing — the app keeps using the old directory where it stands, which is far
/// better than starting up with an empty library.
fn settle_data_dir(current: PathBuf, older: &[PathBuf]) -> PathBuf {
    if current.exists() {
        return current;
    }
    let Some(old) = older.iter().find(|path| path.exists()) else {
        return current;
    };
    if let Some(parent) = current.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(old, &current) {
        Ok(()) => {
            log::info!(
                "library: moved the store from {} to {}",
                old.display(),
                current.display()
            );
            current
        }
        Err(error) => {
            log::warn!(
                "library: cannot move {} to {} ({error}); using it where it is",
                old.display(),
                current.display()
            );
            old.clone()
        }
    }
}

pub fn database_path() -> PathBuf {
    data_dir().join("library.db")
}

pub fn artwork_dir() -> PathBuf {
    data_dir().join("artwork")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deleted_file_leaves_the_disk() {
        // It goes to the Recycle Bin rather than being unlinked, so this only
        // checks it is gone from where it was — where it went is the platform's
        // business, and asserting on the bin's contents would be asserting on
        // the user's own bin.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.mp3");
        std::fs::write(&path, b"not really audio").unwrap();

        delete_to_trash(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn an_older_store_is_moved_to_the_new_one_whole() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("mp3rust");
        std::fs::create_dir_all(old.join("artwork/ab")).unwrap();
        std::fs::write(old.join("library.db"), b"a library").unwrap();
        std::fs::write(old.join("artwork/ab/abcd_s.jpg"), b"a thumbnail").unwrap();

        let current = root.path().join("Tinnitus");
        assert_eq!(settle_data_dir(current.clone(), &[old.clone()]), current);
        assert!(!old.exists());
        assert_eq!(
            std::fs::read(current.join("artwork/ab/abcd_s.jpg")).unwrap(),
            b"a thumbnail"
        );
    }

    #[test]
    fn a_store_already_in_place_is_left_alone() {
        // Both directories existing means the move already happened once; the
        // current one is the live library and the old one is not touched.
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("Tinnitus");
        let old = root.path().join("mp3rust");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&old).unwrap();

        assert_eq!(settle_data_dir(current.clone(), &[old.clone()]), current);
        assert!(old.exists());
    }

    #[test]
    fn a_first_run_lands_in_the_new_directory() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("Tinnitus");
        assert_eq!(
            settle_data_dir(current.clone(), &[root.path().join("mp3rust")]),
            current
        );
    }

    #[test]
    fn deleting_a_file_that_is_already_gone_is_not_an_error() {
        // The confirmation can name files a rescan has since dropped, and a
        // failure there would be reported to the user as a problem they cannot
        // act on.
        let dir = tempfile::tempdir().unwrap();
        assert!(delete_to_trash(&dir.path().join("never-existed.mp3")).is_ok());
    }
}

/// `dirs` without the dependency: this is the one thing we need from it.
fn dirs_data_dir() -> PathBuf {
    // Roaming, which is the folder Windows users know as "AppData" and where
    // every other player they have puts its library.
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support"))
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(std::env::temp_dir)
    }
}
