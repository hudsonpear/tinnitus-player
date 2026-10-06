//! Plain data shared across the library layer. No SQL, no UI, no audio.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch. Every timestamp in the database is this.
pub type Millis = i64;

pub type TrackId = i64;
pub type AlbumId = i64;
pub type ArtistId = i64;
pub type PlaylistId = i64;

/// One row of `tracks`, joined with the names its foreign keys point at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<AlbumId>,
    pub artist_id: Option<ArtistId>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub composer: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<u32>,
    /// Seconds.
    pub duration: f64,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub codec: Option<String>,
    pub file_size: i64,
    pub replay_gain: ReplayGainTags,
    pub date_added: Millis,
    pub last_played: Option<Millis>,
    pub play_count: u32,
    /// 0-5.
    pub rating: u8,
    pub favorite: bool,
    pub artwork_id: Option<i64>,
    /// The file was not found on the last sweep. The row is kept so an unmounted
    /// drive does not silently erase a chunk of the library.
    pub missing: bool,
}

/// ReplayGain as read from the file's tags. Never computed by us at playback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplayGainTags {
    /// Decibels.
    pub track_gain: Option<f32>,
    /// Linear sample peak, 1.0 == full scale.
    pub track_peak: Option<f32>,
    pub album_gain: Option<f32>,
    pub album_peak: Option<f32>,
}

impl ReplayGainTags {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Everything a tag reader can pull out of one file, before it meets the database.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub composer: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<u32>,
    pub replay_gain: ReplayGainTags,
}

/// Technical properties, read from the stream rather than the tags.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioProperties {
    /// Seconds.
    pub duration: f64,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    pub album_artist: Option<String>,
    pub artist_id: Option<ArtistId>,
    pub year: Option<i32>,
    pub artwork_id: Option<i64>,
    pub track_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artist {
    pub id: ArtistId,
    pub name: String,
    pub album_count: u32,
    pub track_count: u32,
    /// Up to four covers from this artist's tracks, most common first, so the
    /// artist tile can be a mosaic rather than one square. An artist with a
    /// single cover has a single entry, and the tile draws it whole.
    pub artwork_ids: Vec<i64>,
}

impl Artist {
    /// The one cover to use where only one will fit — the sidebar, a search
    /// result. Keeps those callers the shape they already had.
    pub fn artwork(&self) -> Option<i64> {
        self.artwork_ids.first().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: i64,
    pub path: PathBuf,
    pub watch: bool,
    pub added_at: Millis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistKind {
    Custom,
    /// The single built-in playlist backed by `tracks.favorite`.
    Favorites,
    /// Reserved: rules exist in the schema from day one so smart playlists
    /// need no migration later.
    Smart,
}

impl PlaylistKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Favorites => "favorites",
            Self::Smart => "smart",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "favorites" => Self::Favorites,
            "smart" => Self::Smart,
            _ => Self::Custom,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistRow {
    pub id: PlaylistId,
    pub name: String,
    pub kind: PlaylistKind,
    /// JSON rules for smart playlists, `None` otherwise.
    pub rules: Option<String>,
    pub position: i64,
    pub track_count: u32,
    /// Up to four cover ids for the playlist's mosaic; empty in search results.
    pub artwork_ids: Vec<i64>,
    pub created_at: Millis,
    pub modified_at: Millis,
}

/// A volume and an equalizer curve remembered for one song.
///
/// The curve is carried as the JSON that was stored, not as a parsed settings
/// object: this crate knows nothing about the audio engine and there is no
/// reason for it to start. Whoever asked for it can deserialize it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackAudio {
    pub volume: Option<f32>,
    pub eq: Option<String>,
}

impl TrackAudio {
    /// True when there is nothing worth keeping a row for.
    pub fn is_empty(&self) -> bool {
        self.volume.is_none() && self.eq.is_none()
    }
}

/// How much of a track has to be heard before it counts as a play.
///
/// Below this it was a skip: it still goes in the history, and it still stamps
/// `last_played`, but it does not move `play_count`. The difference is the
/// whole distinction between "what you put on" and "what you listened to".
pub const PLAY_THRESHOLD: f32 = 0.5;

/// One listen. Written when a track stops, not when it starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listen {
    pub track_id: TrackId,
    pub played_at: Millis,
    pub listened_ms: i64,
    /// 0.0-1.0 of the track's duration.
    pub completion: f32,
}

/// What the library view is currently showing. Each variant maps to one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryView {
    AllSongs,
    Albums,
    Artists,
    Genres,
    Years,
    Folders,
    RecentlyAdded,
    RecentlyPlayed,
    MostPlayed,
    Favorites,
    Album(AlbumId),
    Artist(ArtistId),
    Genre(String),
    Year(i32),
    Folder(i64),
    Playlist(PlaylistId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    Title,
    Artist,
    Album,
    Duration,
    TrackNumber,
    Year,
    DateAdded,
    LastPlayed,
    PlayCount,
    Rating,
}

impl SortKey {
    /// The SQL expression to order by. Never interpolate user text here — this
    /// is a closed set precisely so the sort clause can be built by hand.
    pub(crate) fn column(self) -> &'static str {
        match self {
            Self::Title => "t.title COLLATE NOCASE",
            Self::Artist => "t.artist COLLATE NOCASE",
            Self::Album => "al.name COLLATE NOCASE",
            Self::Duration => "t.duration",
            Self::TrackNumber => "t.disc_number, t.track_number",
            Self::Year => "t.year",
            Self::DateAdded => "t.date_added",
            Self::LastPlayed => "t.last_played",
            Self::PlayCount => "t.play_count",
            Self::Rating => "t.rating",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Title,
            descending: false,
        }
    }
}

/// Progress of a running scan, pushed to the UI on a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum ScanEvent {
    Started {
        total: usize,
    },
    Progress {
        done: usize,
        total: usize,
        current: PathBuf,
    },
    /// The scan finished or was cancelled; `added`/`updated`/`removed` are counts.
    Finished {
        added: usize,
        updated: usize,
        removed: usize,
        cancelled: bool,
    },
    Failed(String),
}

/// Grouped results of a global search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub genres: Vec<String>,
    pub playlists: Vec<PlaylistRow>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
            && self.albums.is_empty()
            && self.artists.is_empty()
            && self.genres.is_empty()
            && self.playlists.is_empty()
    }
}

/// Milliseconds since the Unix epoch, right now.
pub fn now_ms() -> Millis {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as Millis)
        .unwrap_or(0)
}
