//! Album art: extraction, de-duplication, and an on-disk thumbnail cache.
//!
//! Images are content-addressed by the SHA-256 of their bytes, so the thousand
//! tracks of a box set that all embed the same cover decode and store it once.
//! Nothing here runs on the UI thread; the UI asks for a path and shows a
//! placeholder until it exists.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use image::imageops::FilterType;
use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

/// Thumbnail edge lengths, largest last. `Small` is a list row, `Medium` a grid
/// tile, `Large` the player bar and the album header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbSize {
    Small,
    Medium,
    Large,
}

impl ThumbSize {
    pub const ALL: [ThumbSize; 3] = [ThumbSize::Small, ThumbSize::Medium, ThumbSize::Large];

    pub fn pixels(self) -> u32 {
        match self {
            Self::Small => 96,
            Self::Medium => 320,
            Self::Large => 1000,
        }
    }

    /// The smallest cached size that still covers `edge` logical pixels, so a
    /// user who drags the album-grid slider up gets sharper art without us
    /// keeping a size for every slider position.
    pub fn covering(edge: u32) -> Self {
        Self::ALL
            .into_iter()
            .find(|size| size.pixels() >= edge)
            .unwrap_or(Self::Large)
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Small => "s",
            Self::Medium => "m",
            Self::Large => "l",
        }
    }
}

/// Filenames checked, in order, for art sitting next to the audio files.
const FOLDER_ART_NAMES: &[&str] = &[
    "cover",
    "folder",
    "front",
    "album",
    "albumart",
    "albumartsmall",
    "thumb",
];
const FOLDER_ART_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// Refuse to decode anything larger than this. Image dimensions come from an
/// untrusted file, and a decompression bomb is a real shape of malformed input.
const MAX_SOURCE_BYTES: usize = 24 * 1024 * 1024;
const MAX_SOURCE_PIXELS: u64 = 64_000_000;

pub struct ArtworkCache {
    root: PathBuf,
}

impl ArtworkCache {
    /// `root` is a directory we own; it is created if missing.
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)
            .with_context(|| format!("cannot create the artwork cache at {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where a given image and size lives once cached. The name is derived from
    /// the content hash, never from anything in the audio file's tags.
    pub fn thumb_path(&self, hash: &str, size: ThumbSize) -> PathBuf {
        // One level of fan-out keeps directories small on a big library.
        let shard = &hash[..2.min(hash.len())];
        self.root
            .join(shard)
            .join(format!("{hash}_{}.jpg", size.suffix()))
    }

    /// Stores an image, returning the `artwork` row id. Re-storing bytes we have
    /// already seen is a cheap hash and a lookup: no decode, no write.
    pub fn store(
        &self,
        conn: &Connection,
        data: &[u8],
        mime: Option<&str>,
        source: &str,
    ) -> Result<i64> {
        anyhow::ensure!(
            data.len() <= MAX_SOURCE_BYTES,
            "cover art is {} bytes, larger than the {MAX_SOURCE_BYTES} byte limit",
            data.len()
        );
        let hash = hex(&Sha256::digest(data));

        if let Some(id) = self.lookup(conn, &hash)? {
            return Ok(id);
        }

        let reader = image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .context("cannot recognise the cover image format")?;
        let (width, height) = reader
            .into_dimensions()
            .context("cannot read the cover image size")?;
        anyhow::ensure!(
            u64::from(width) * u64::from(height) <= MAX_SOURCE_PIXELS,
            "cover art is {width}x{height}, too large to decode"
        );

        let image = image::load_from_memory(data).context("cannot decode the cover image")?;
        for size in ThumbSize::ALL {
            let path = self.thumb_path(&hash, size);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let edge = size.pixels();
            // Never upscale: a 300px cover stays 300px in the "large" slot.
            let thumb = match width.max(height) > edge {
                true => image.resize(edge, edge, FilterType::Lanczos3),
                false => image.clone(),
            };
            thumb
                .into_rgb8()
                .save_with_format(&path, image::ImageFormat::Jpeg)
                .with_context(|| format!("cannot write {}", path.display()))?;
        }

        conn.execute(
            "INSERT OR IGNORE INTO artwork(hash, mime, width, height, source)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![hash, mime, width, height, source],
        )?;
        self.lookup(conn, &hash)?
            .context("artwork row vanished right after it was written")
    }

    fn lookup(&self, conn: &Connection, hash: &str) -> Result<Option<i64>> {
        conn.query_row("SELECT id FROM artwork WHERE hash = ?1", [hash], |row| {
            row.get(0)
        })
        .optional()
        .context("cannot look up artwork")
    }

    /// The cached file for an artwork id, or `None` when the row exists but the
    /// file was cleared out from under us — the caller shows the placeholder.
    pub fn path_for(&self, conn: &Connection, id: i64, size: ThumbSize) -> Result<Option<PathBuf>> {
        let hash: Option<String> = conn
            .query_row("SELECT hash FROM artwork WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(hash) = hash else { return Ok(None) };
        let path = self.thumb_path(&hash, size);
        Ok(path.exists().then_some(path))
    }

    /// Deletes cached files for artwork rows that no longer exist. Run after
    /// `Db::prune_orphans`, never during a scan.
    pub fn sweep(&self, conn: &Connection) -> Result<usize> {
        let mut statement = conn.prepare("SELECT hash FROM artwork")?;
        let live: std::collections::HashSet<String> = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;

        let mut removed = 0;
        for shard in std::fs::read_dir(&self.root)?.flatten() {
            if !shard.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            for entry in std::fs::read_dir(shard.path())?.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some(hash) = name.split('_').next() else {
                    continue;
                };
                if !live.contains(hash) {
                    std::fs::remove_file(entry.path()).ok();
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

/// Looks for `cover.jpg` and friends beside an audio file. Only used when the
/// file itself carries no embedded picture.
pub fn folder_art(audio_path: &Path) -> Option<PathBuf> {
    let directory = audio_path.parent()?;
    let entries: Vec<_> = std::fs::read_dir(directory).ok()?.flatten().collect();

    for wanted in FOLDER_ART_NAMES {
        for entry in &entries {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
                continue;
            };
            if stem.eq_ignore_ascii_case(wanted)
                && FOLDER_ART_EXTENSIONS
                    .iter()
                    .any(|allowed| extension.eq_ignore_ascii_case(allowed))
            {
                return Some(path);
            }
        }
    }
    None
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn identical_bytes_are_stored_once() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtworkCache::new(dir.path().to_path_buf()).unwrap();
        let db = Db::in_memory().unwrap();
        let data = png(64, 64);

        let first = cache
            .store(db.conn(), &data, Some("image/png"), "embedded")
            .unwrap();
        let second = cache
            .store(db.conn(), &data, Some("image/png"), "embedded")
            .unwrap();
        assert_eq!(first, second);

        let rows: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM artwork", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn every_size_lands_on_disk_and_never_upscales() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtworkCache::new(dir.path().to_path_buf()).unwrap();
        let db = Db::in_memory().unwrap();
        // Smaller than every thumbnail size, so none of them may grow it.
        let id = cache
            .store(db.conn(), &png(48, 48), Some("image/png"), "embedded")
            .unwrap();

        for size in ThumbSize::ALL {
            let path = cache
                .path_for(db.conn(), id, size)
                .unwrap()
                .expect("cached file");
            let (width, height) = image::ImageReader::open(&path)
                .unwrap()
                .into_dimensions()
                .unwrap();
            assert_eq!((width, height), (48, 48), "{size:?} upscaled the source");
        }
    }

    #[test]
    fn large_art_is_scaled_down_to_each_size() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtworkCache::new(dir.path().to_path_buf()).unwrap();
        let db = Db::in_memory().unwrap();
        let id = cache
            .store(db.conn(), &png(1400, 1400), Some("image/png"), "embedded")
            .unwrap();

        let small = cache
            .path_for(db.conn(), id, ThumbSize::Small)
            .unwrap()
            .unwrap();
        let (width, _) = image::ImageReader::open(small)
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(width, ThumbSize::Small.pixels());
    }

    #[test]
    fn rejects_nonsense_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtworkCache::new(dir.path().to_path_buf()).unwrap();
        let db = Db::in_memory().unwrap();
        assert!(
            cache
                .store(db.conn(), b"not an image", None, "embedded")
                .is_err()
        );
    }

    #[test]
    fn covering_picks_the_smallest_size_that_fits() {
        assert_eq!(ThumbSize::covering(40), ThumbSize::Small);
        assert_eq!(ThumbSize::covering(96), ThumbSize::Small);
        assert_eq!(ThumbSize::covering(97), ThumbSize::Medium);
        assert_eq!(ThumbSize::covering(5000), ThumbSize::Large);
    }

    #[test]
    fn sweep_removes_files_with_no_row() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtworkCache::new(dir.path().to_path_buf()).unwrap();
        let db = Db::in_memory().unwrap();
        let id = cache
            .store(db.conn(), &png(64, 64), Some("image/png"), "embedded")
            .unwrap();

        db.conn()
            .execute("DELETE FROM artwork WHERE id = ?1", [id])
            .unwrap();
        assert_eq!(cache.sweep(db.conn()).unwrap(), ThumbSize::ALL.len());
    }
}
