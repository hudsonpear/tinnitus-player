//! Tag reading and writing, via lofty.
//!
//! Reading never modifies the file. Writing happens only from the metadata
//! editor, after the user confirms, and goes through a temporary copy so a
//! failure part-way cannot leave a half-written file behind.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, ItemKey, TagExt};
use lofty::probe::Probe;
use lofty::tag::Tag;
use lofty::tag::items::Timestamp;

use crate::models::{AudioProperties, ReplayGainTags, TrackTags};

/// File extensions we will try to decode.
///
/// This list matches the decoder features enabled for rodio in the workspace
/// manifest, and must keep matching them: an extension listed here that the
/// decoder cannot open is a track that scans into the library and then refuses
/// to play. WavPack, Monkey's Audio and Musepack are deliberately absent for
/// that reason — adding one means enabling its decoder first.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "opus", "m4a", "m4b", "mp4", "aac", "aiff", "aif", "aifc",
];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.as_str()))
}

/// Everything one file yields in a single open: tags, stream properties, codec
/// name, and the embedded cover if there is one.
pub struct Probed {
    pub tags: TrackTags,
    pub properties: AudioProperties,
    pub codec: Option<String>,
    pub picture: Option<Picture>,
}

pub struct Picture {
    pub data: Vec<u8>,
    pub mime: Option<String>,
}

/// Largest embedded picture we will pull into memory. A malformed or hostile tag
/// claiming a 500 MB cover is refused rather than allocated.
const MAX_PICTURE_BYTES: usize = 24 * 1024 * 1024;

/// Like `probe`, but a file whose tags cannot be parsed still yields a `Probed`.
///
/// Real libraries contain files with a damaged frame or a mangled tag that
/// decoders play perfectly well. Refusing to index those makes them invisible in
/// the app, which is worse than showing them with a filename for a title and no
/// duration until they play. The scanner uses this; the metadata editor uses
/// `probe`, because writing tags into a file we could not read is not something
/// to do quietly.
pub fn probe_lenient(path: &Path) -> Probed {
    match probe(path) {
        Ok(probed) => probed,
        Err(error) => {
            log::debug!(
                "metadata: {} has unreadable tags, indexing it anyway: {error:#}",
                path.display()
            );
            Probed {
                tags: TrackTags::default(),
                properties: AudioProperties::default(),
                codec: path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| extension.to_ascii_lowercase()),
                picture: None,
            }
        }
    }
}

pub fn probe(path: &Path) -> Result<Probed> {
    let tagged = Probe::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?
        .read()
        .with_context(|| format!("cannot read {}", path.display()))?;

    let file_properties = tagged.properties();
    let properties = AudioProperties {
        duration: file_properties.duration().as_secs_f64(),
        bitrate: file_properties
            .audio_bitrate()
            .or_else(|| file_properties.overall_bitrate()),
        sample_rate: file_properties.sample_rate(),
        channels: file_properties.channels().map(u16::from),
    };
    let codec = Some(format!("{:?}", tagged.file_type()).to_ascii_lowercase());

    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(Probed {
            tags: TrackTags::default(),
            properties,
            codec,
            picture: None,
        });
    };

    let tags = TrackTags {
        title: text(tag.title()),
        artist: text(tag.artist()),
        album: text(tag.album()),
        album_artist: string(tag, ItemKey::AlbumArtist),
        genre: text(tag.genre()),
        year: tag
            .date()
            .map(|date| i32::from(date.year))
            .filter(|year| *year > 0),
        track_number: tag.track().filter(|value| *value > 0),
        disc_number: tag.disk().filter(|value| *value > 0),
        composer: string(tag, ItemKey::Composer),
        comment: text(tag.comment()),
        bpm: string(tag, ItemKey::Bpm).and_then(|value| value.trim().parse().ok()),
        replay_gain: ReplayGainTags {
            track_gain: decibels(string(tag, ItemKey::ReplayGainTrackGain)),
            track_peak: peak(string(tag, ItemKey::ReplayGainTrackPeak)),
            album_gain: decibels(string(tag, ItemKey::ReplayGainAlbumGain)),
            album_peak: peak(string(tag, ItemKey::ReplayGainAlbumPeak)),
        },
    };

    Ok(Probed {
        tags,
        properties,
        codec,
        picture: cover(tag),
    })
}

/// The picture to use as the cover, out of however many the tag holds.
///
/// Tags in the wild carry more than one, and the first is not necessarily the
/// cover: taggers leave empty `APIC` frames behind that declare a MIME type and
/// no bytes, and a file can hold an artist shot or a thumbnail ahead of the front
/// cover. An empty frame is not a picture — taking it means the real cover is
/// never looked at, and the scanner reports art it cannot read on a file whose
/// art is fine.
fn cover(tag: &Tag) -> Option<Picture> {
    let usable = || {
        tag.pictures()
            .iter()
            .filter(|picture| (1..=MAX_PICTURE_BYTES).contains(&picture.data().len()))
    };
    usable()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| usable().next())
        .map(|picture| Picture {
            data: picture.data().to_vec(),
            mime: picture.mime_type().map(ToString::to_string),
        })
}

/// What the metadata editor asks to change. `None` means "leave alone", which is
/// what makes batch editing across a mixed selection work: only the fields the
/// user actually touched are written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagEdit {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<Option<i32>>,
    pub track_number: Option<Option<u32>>,
    pub disc_number: Option<Option<u32>>,
    pub composer: Option<String>,
    pub comment: Option<String>,
}

impl TagEdit {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Applies an edit to one file.
///
/// The file is copied to a sibling temporary file, tagged there, and only then
/// renamed over the original. A crash or a full disk leaves the original intact.
pub fn write_tags(path: &Path, edit: &TagEdit) -> Result<()> {
    if edit.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(path.is_file(), "{} is not a file", path.display());

    let staging = staging_path(path);
    std::fs::copy(path, &staging)
        .with_context(|| format!("cannot stage a copy of {}", path.display()))?;

    let result = (|| -> Result<()> {
        // The staging file is called `*.tinnitus-tmp`, and a probe picks its parser
        // from the extension unless told to look inside. Without this every
        // write failed with "no format could be determined" — on a copy, so the
        // original was left untouched and the edit simply vanished.
        let mut tagged = Probe::open(&staging)?.guess_file_type()?.read()?;
        if tagged.primary_tag().is_none() && tagged.first_tag().is_none() {
            let kind = tagged.primary_tag_type();
            tagged.insert_tag(Tag::new(kind));
        }
        let primary = tagged.primary_tag_mut().is_some();
        let tag = match primary {
            true => tagged.primary_tag_mut(),
            false => tagged.first_tag_mut(),
        }
        .with_context(|| format!("{} cannot hold tags", path.display()))?;

        apply(tag, edit);
        tag.save_to_path(&staging, WriteOptions::default())
            .with_context(|| format!("cannot write tags for {}", path.display()))?;
        Ok(())
    })();

    if let Err(error) = result {
        std::fs::remove_file(&staging).ok();
        return Err(error);
    }

    std::fs::rename(&staging, path).with_context(|| {
        format!(
            "cannot replace {} with the tagged copy at {}",
            path.display(),
            staging.display()
        )
    })?;
    Ok(())
}

fn apply(tag: &mut Tag, edit: &TagEdit) {
    set(tag, ItemKey::TrackTitle, edit.title.as_deref());
    set(tag, ItemKey::TrackArtist, edit.artist.as_deref());
    set(tag, ItemKey::AlbumTitle, edit.album.as_deref());
    set(tag, ItemKey::AlbumArtist, edit.album_artist.as_deref());
    set(tag, ItemKey::Genre, edit.genre.as_deref());
    set(tag, ItemKey::Composer, edit.composer.as_deref());
    set(tag, ItemKey::Comment, edit.comment.as_deref());

    if let Some(track) = edit.track_number {
        match track {
            Some(value) => tag.set_track(value),
            None => tag.remove_track(),
        }
    }
    if let Some(disc) = edit.disc_number {
        match disc {
            Some(value) => tag.set_disk(value),
            None => tag.remove_disk(),
        }
    }
    if let Some(year) = edit.year {
        match year.filter(|year| *year > 0) {
            Some(value) => tag.set_date(Timestamp {
                year: value as u16,
                month: None,
                day: None,
                hour: None,
                minute: None,
                second: None,
            }),
            None => tag.remove_date(),
        }
    }
}

/// A sibling of the original, so the rename at the end stays on one filesystem
/// and is therefore atomic.
fn staging_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tinnitus-tmp");
    path.with_file_name(name)
}

fn set(tag: &mut Tag, key: ItemKey, value: Option<&str>) {
    let Some(value) = value else { return };
    let value = value.trim();
    if value.is_empty() {
        tag.remove_key(key);
    } else {
        tag.insert_text(key, value.to_owned());
    }
}

fn text(value: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn string(tag: &Tag, key: ItemKey) -> Option<String> {
    tag.get_string(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// ReplayGain gains are written as e.g. `-7.28 dB`.
fn decibels(value: Option<String>) -> Option<f32> {
    let value = value?;
    let cleaned = value
        .trim()
        .trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace());
    cleaned.trim().parse().ok()
}

/// Peaks are a bare linear float; anything outside a sane range is ignored
/// rather than trusted into a clipping calculation.
fn peak(value: Option<String>) -> Option<f32> {
    value?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|peak| peak.is_finite() && *peak > 0.0 && *peak < 64.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_audio_by_extension() {
        assert!(is_audio_file(Path::new(r"d:\m\a.MP3")));
        assert!(is_audio_file(Path::new(r"d:\m\a.flac")));
        assert!(!is_audio_file(Path::new(r"d:\m\cover.jpg")));
        assert!(!is_audio_file(Path::new(r"d:\m\notes")));
    }

    #[test]
    fn parses_replaygain_text() {
        assert_eq!(decibels(Some("-7.28 dB".into())), Some(-7.28));
        assert_eq!(decibels(Some("+3.5dB".into())), Some(3.5));
        assert_eq!(decibels(Some("nonsense".into())), None);
        assert_eq!(peak(Some("0.988".into())), Some(0.988));
        // A peak of zero would make the clip guard divide by nothing.
        assert_eq!(peak(Some("0".into())), None);
        assert_eq!(peak(Some("1e9".into())), None);
    }

    #[test]
    fn an_empty_picture_frame_never_wins_over_a_real_cover() {
        use lofty::picture::{MimeType, Picture as TagPicture};
        use lofty::tag::TagType;

        let mut tag = Tag::new(TagType::Id3v2);
        // What a real file out of a sloppy tagger looks like: two stub frames
        // that declare a MIME type and carry no image, then the actual cover.
        for pic_type in [PictureType::Other, PictureType::CoverFront] {
            tag.push_picture(
                TagPicture::unchecked(Vec::new())
                    .pic_type(pic_type)
                    .mime_type(MimeType::Jpeg)
                    .build(),
            );
        }
        tag.push_picture(
            TagPicture::unchecked(b"\x89PNG\r\n\x1a\n and then some".to_vec())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Png)
                .build(),
        );

        let chosen = cover(&tag).expect("the cover with bytes in it");
        assert_eq!(chosen.data.first(), Some(&0x89));
        assert_eq!(chosen.mime.as_deref(), Some("image/png"));

        // A tag whose every picture is empty has no cover at all, so the scanner
        // falls back to folder art instead of reporting art it cannot read.
        let mut empty = Tag::new(TagType::Id3v2);
        empty.push_picture(
            TagPicture::unchecked(Vec::new())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Jpeg)
                .build(),
        );
        assert!(cover(&empty).is_none());
    }

    #[test]
    fn a_front_cover_is_preferred_to_whatever_comes_first() {
        use lofty::picture::{MimeType, Picture as TagPicture};
        use lofty::tag::TagType;

        let mut tag = Tag::new(TagType::Id3v2);
        tag.push_picture(
            TagPicture::unchecked(b"an artist shot".to_vec())
                .pic_type(PictureType::Artist)
                .mime_type(MimeType::Jpeg)
                .build(),
        );
        tag.push_picture(
            TagPicture::unchecked(b"the front cover".to_vec())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Jpeg)
                .build(),
        );

        assert_eq!(cover(&tag).unwrap().data, b"the front cover");
    }

    #[test]
    fn staging_file_is_a_sibling() {
        let original = Path::new(r"d:\music\album\01 song.flac");
        let staged = staging_path(original);
        assert_eq!(staged.parent(), original.parent());
        assert!(
            staged
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".tinnitus-tmp")
        );
    }

    /// Writes a tag to a copy of a real file and reads it back, which is the
    /// only way to know the whole path works: staging, tagging, and the rename
    /// over the original. Ignored by default — it needs a file of the runner's.
    ///
    /// `TINNITUS_TAG_FILE="D:\\song.mp3" cargo test -p library --lib round_trips -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn round_trips_a_tag_through_a_real_file() {
        let Some(source) = std::env::var_os("TINNITUS_TAG_FILE") else {
            panic!("set TINNITUS_TAG_FILE to the file to copy and tag");
        };
        let source = PathBuf::from(source);
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join(source.file_name().unwrap());
        std::fs::copy(&source, &copy).unwrap();

        let edit = TagEdit {
            title: Some("A Title Written By The Test".to_owned()),
            year: Some(Some(1999)),
            ..Default::default()
        };
        write_tags(&copy, &edit).expect("write failed");

        let read = probe(&copy).expect("cannot read back");
        println!(
            "title back: {:?}, year back: {:?}",
            read.tags.title, read.tags.year
        );
        assert_eq!(
            read.tags.title.as_deref(),
            Some("A Title Written By The Test")
        );
        assert_eq!(read.tags.year, Some(1999));
    }

    #[test]
    fn an_empty_edit_writes_nothing() {
        // No file needed: an empty edit must return before it ever opens one.
        assert!(write_tags(Path::new(r"d:\does\not\exist.mp3"), &TagEdit::default()).is_ok());
    }
}
