//! Import and export of M3U, M3U8, PLS and XSPF playlists.
//!
//! Playlist files are untrusted input. Every path that comes out of one is
//! resolved against the playlist's own directory and then checked: no UNC
//! redirect, no device path, no scheme we do not understand. Nothing here ever
//! launches anything — a playlist entry is a path, not a command.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub title: Option<String>,
    /// Seconds, when the file recorded one.
    pub duration: Option<f64>,
}

impl Entry {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            title: None,
            duration: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `.m3u` — historically the local code page, so it is read leniently.
    M3u,
    /// `.m3u8` — the same format, UTF-8 by definition. What we write.
    M3u8,
    Pls,
    Xspf,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "m3u" => Some(Self::M3u),
            "m3u8" => Some(Self::M3u8),
            "pls" => Some(Self::Pls),
            "xspf" => Some(Self::Xspf),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::M3u => "m3u",
            Self::M3u8 => "m3u8",
            Self::Pls => "pls",
            Self::Xspf => "xspf",
        }
    }
}

/// Reads a playlist file from disk, choosing the parser by extension.
pub fn read(path: &Path) -> Result<Vec<Entry>> {
    let format = Format::from_path(path)
        .with_context(|| format!("{} is not a playlist we understand", path.display()))?;
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    // Lossy on purpose: a legacy .m3u in some code page should still yield its
    // ASCII paths rather than failing the whole import.
    let text = String::from_utf8_lossy(&bytes);
    let base = path.parent().unwrap_or(Path::new(""));
    Ok(parse(&text, format, base))
}

/// Writes a playlist file. Always UTF-8; `.m3u` is written as `.m3u8` content,
/// which every player that reads `.m3u` also accepts.
pub fn write(path: &Path, entries: &[Entry], relative_to: Option<&Path>) -> Result<()> {
    let format = Format::from_path(path)
        .with_context(|| format!("{} is not a playlist we understand", path.display()))?;
    let text = render(entries, format, relative_to);
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

pub fn parse(text: &str, format: Format, base: &Path) -> Vec<Entry> {
    match format {
        Format::M3u | Format::M3u8 => parse_m3u(text, base),
        Format::Pls => parse_pls(text, base),
        Format::Xspf => parse_xspf(text, base),
    }
}

pub fn render(entries: &[Entry], format: Format, relative_to: Option<&Path>) -> String {
    match format {
        Format::M3u | Format::M3u8 => render_m3u(entries, relative_to),
        Format::Pls => render_pls(entries, relative_to),
        Format::Xspf => render_xspf(entries, relative_to),
    }
}

// -- M3U ------------------------------------------------------------------

fn parse_m3u(text: &str, base: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut pending: Option<(Option<f64>, Option<String>)> = None;

    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // `#EXTINF:213,Artist - Title`
            let (seconds, title) = match rest.split_once(',') {
                Some((seconds, title)) => (seconds.trim(), title.trim()),
                None => (rest.trim(), ""),
            };
            pending = Some((
                seconds.parse::<f64>().ok().filter(|value| *value > 0.0),
                (!title.is_empty()).then(|| title.to_owned()),
            ));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        let Some(path) = resolve(line, base) else {
            pending = None;
            continue;
        };
        let (duration, title) = pending.take().unwrap_or((None, None));
        entries.push(Entry {
            path,
            title,
            duration,
        });
    }
    entries
}

fn render_m3u(entries: &[Entry], relative_to: Option<&Path>) -> String {
    let mut out = String::from("#EXTM3U\n");
    for entry in entries {
        let seconds = entry.duration.unwrap_or(0.0).round() as i64;
        let title = entry.title.clone().unwrap_or_else(|| stem(&entry.path));
        out.push_str(&format!("#EXTINF:{seconds},{title}\n"));
        out.push_str(&display(&entry.path, relative_to));
        out.push('\n');
    }
    out
}

// -- PLS ------------------------------------------------------------------

fn parse_pls(text: &str, base: &Path) -> Vec<Entry> {
    // PLS is `File1=`, `Title1=`, `Length1=`; the numbers are the join key and
    // are not required to be contiguous or in order.
    // file, title, length, keyed by the index in the file.
    type Slot = (Option<String>, Option<String>, Option<f64>);
    let mut slots: std::collections::BTreeMap<u32, Slot> = Default::default();

    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let lower = key.trim().to_ascii_lowercase();

        let (field, index) = if let Some(index) = lower.strip_prefix("file") {
            ("file", index)
        } else if let Some(index) = lower.strip_prefix("title") {
            ("title", index)
        } else if let Some(index) = lower.strip_prefix("length") {
            ("length", index)
        } else {
            continue;
        };
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };

        let slot = slots.entry(index).or_default();
        match field {
            "file" => slot.0 = Some(value.to_owned()),
            "title" => slot.1 = (!value.is_empty()).then(|| value.to_owned()),
            _ => slot.2 = value.parse::<f64>().ok().filter(|value| *value > 0.0),
        }
    }

    slots
        .into_values()
        .filter_map(|(file, title, duration)| {
            let path = resolve(&file?, base)?;
            Some(Entry {
                path,
                title,
                duration,
            })
        })
        .collect()
}

fn render_pls(entries: &[Entry], relative_to: Option<&Path>) -> String {
    let mut out = String::from("[playlist]\n");
    for (index, entry) in entries.iter().enumerate() {
        let number = index + 1;
        out.push_str(&format!(
            "File{number}={}\n",
            display(&entry.path, relative_to)
        ));
        out.push_str(&format!(
            "Title{number}={}\n",
            entry.title.clone().unwrap_or_else(|| stem(&entry.path))
        ));
        out.push_str(&format!(
            "Length{number}={}\n",
            entry.duration.map(|d| d.round() as i64).unwrap_or(-1)
        ));
    }
    out.push_str(&format!("NumberOfEntries={}\nVersion=2\n", entries.len()));
    out
}

// -- XSPF -----------------------------------------------------------------

fn parse_xspf(text: &str, base: &Path) -> Vec<Entry> {
    let document = match roxmltree::Document::parse(text) {
        Ok(document) => document,
        Err(error) => {
            log::warn!("playlist: cannot parse XSPF: {error}");
            return vec![];
        }
    };

    document
        .descendants()
        .filter(|node| node.has_tag_name("track"))
        .filter_map(|track| {
            let child = |name: &str| {
                track
                    .children()
                    .find(|node| node.has_tag_name(name))
                    .and_then(|node| node.text())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
            };
            let path = resolve(child("location")?, base)?;
            Some(Entry {
                path,
                title: child("title").map(str::to_owned),
                // XSPF durations are milliseconds.
                duration: child("duration")
                    .and_then(|value| value.parse::<f64>().ok())
                    .map(|ms| ms / 1000.0)
                    .filter(|seconds| *seconds > 0.0),
            })
        })
        .collect()
}

fn render_xspf(entries: &[Entry], relative_to: Option<&Path>) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <playlist version=\"1\" xmlns=\"http://xspf.org/ns/0/\">\n  <trackList>\n",
    );
    for entry in entries {
        out.push_str("    <track>\n");
        out.push_str(&format!(
            "      <location>{}</location>\n",
            escape_xml(&file_uri(&entry.path, relative_to))
        ));
        if let Some(title) = &entry.title {
            out.push_str(&format!("      <title>{}</title>\n", escape_xml(title)));
        }
        if let Some(duration) = entry.duration {
            out.push_str(&format!(
                "      <duration>{}</duration>\n",
                (duration * 1000.0).round() as i64
            ));
        }
        out.push_str("    </track>\n");
    }
    out.push_str("  </trackList>\n</playlist>\n");
    out
}

// -- path handling --------------------------------------------------------

/// Turns one playlist line into a path we are willing to open.
///
/// Returns `None` for anything that is not a local file: remote streams are not
/// supported yet, and a scheme we do not recognise is refused rather than
/// guessed at.
fn resolve(raw: &str, base: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let candidate = if let Some(rest) = strip_file_uri(raw) {
        PathBuf::from(rest)
    } else if raw.contains("://") {
        log::debug!("playlist: skipping remote entry {raw}");
        return None;
    } else {
        PathBuf::from(raw.replace('/', std::path::MAIN_SEPARATOR_STR))
    };

    let joined = match candidate.is_absolute() {
        true => candidate,
        false => base.join(candidate),
    };

    // `..` is allowed to walk out of the playlist's folder — a playlist next to
    // an album legitimately points at `../other album/01.flac`. What is refused
    // is anything that is not a plain local path: a UNC share, a device path.
    normalize(&joined)
}

/// Collapses `.` and `..` textually. Not `canonicalize`: the file may not exist
/// yet (importing a playlist for a drive that is not mounted should still
/// produce rows), and canonicalize would hit the disk for every line of a
/// ten-thousand-entry playlist.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                // Reject UNC and device paths: a playlist must not be able to
                // point the player at `\\server\share` or `\\.\PhysicalDrive0`.
                let text = prefix.as_os_str().to_string_lossy();
                if text.starts_with(r"\\") {
                    log::warn!("playlist: refusing non-local path {}", path.display());
                    return None;
                }
                out.push(component.as_os_str());
            }
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// `file:///D:/music/a.flac` and `file://localhost/...` become a plain path.
fn strip_file_uri(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    if !lower.starts_with("file://") {
        return None;
    }
    let rest = &raw["file://".len()..];
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    // `/D:/music` -> `D:/music`; a bare `/music` on unix keeps its root.
    let bytes = decoded.as_bytes();
    let trimmed = match bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[2] == b':'
        && bytes[1].is_ascii_alphabetic()
    {
        true => decoded[1..].to_owned(),
        false => decoded,
    };
    Some(trimmed.replace('/', std::path::MAIN_SEPARATOR_STR))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// How a path is written into a playlist file: relative when it sits under the
/// playlist's folder, absolute otherwise. Relative entries survive the user
/// moving the whole music folder.
fn display(path: &Path, relative_to: Option<&Path>) -> String {
    let text = relative_to
        .and_then(|base| path.strip_prefix(base).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    // Forward slashes travel between platforms; every player accepts them.
    text.replace('\\', "/")
}

fn file_uri(path: &Path, relative_to: Option<&Path>) -> String {
    let text = display(path, relative_to);
    let absolute = Path::new(&text).is_absolute() || text.chars().nth(1) == Some(':');
    match absolute {
        true => format!("file:///{}", text.trim_start_matches('/')),
        false => text,
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            r"D:\music\album"
        } else {
            "/music/album"
        })
    }

    fn joined(parts: &[&str]) -> PathBuf {
        let mut path = base();
        for part in parts {
            path.push(part);
        }
        path
    }

    #[test]
    fn reads_extended_m3u() {
        let text =
            "#EXTM3U\n#EXTINF:213,Pink Floyd - Time\n01 time.flac\n\n#comment\n02 money.flac\n";
        let entries = parse(text, Format::M3u8, &base());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, joined(&["01 time.flac"]));
        assert_eq!(entries[0].title.as_deref(), Some("Pink Floyd - Time"));
        assert_eq!(entries[0].duration, Some(213.0));
        assert_eq!(entries[1].title, None);
    }

    #[test]
    fn m3u_round_trips() {
        let entries = vec![Entry {
            path: joined(&["01 time.flac"]),
            title: Some("Time".into()),
            duration: Some(413.0),
        }];
        let text = render(&entries, Format::M3u8, Some(&base()));
        assert!(text.contains("01 time.flac"));
        assert_eq!(parse(&text, Format::M3u8, &base()), entries);
    }

    #[test]
    fn reads_pls_out_of_order() {
        let text = "[playlist]\nFile2=b.flac\nTitle2=B\nLength2=100\nFile1=a.flac\nTitle1=A\nNumberOfEntries=2\n";
        let entries = parse(text, Format::Pls, &base());
        assert_eq!(entries.len(), 2);
        // Sorted by their index, not by the order the lines appeared.
        assert_eq!(entries[0].title.as_deref(), Some("A"));
        assert_eq!(entries[1].duration, Some(100.0));
    }

    #[test]
    fn pls_round_trips() {
        let entries = vec![
            Entry {
                path: joined(&["a.flac"]),
                title: Some("A".into()),
                duration: Some(10.0),
            },
            Entry {
                path: joined(&["b.flac"]),
                title: Some("B".into()),
                duration: None,
            },
        ];
        let text = render(&entries, Format::Pls, Some(&base()));
        let back = parse(&text, Format::Pls, &base());
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].path, entries[0].path);
        assert_eq!(back[1].duration, None);
    }

    #[test]
    fn reads_xspf_with_millisecond_durations() {
        let text = r#"<?xml version="1.0"?>
            <playlist version="1" xmlns="http://xspf.org/ns/0/"><trackList>
              <track><location>a.flac</location><title>A</title><duration>213000</duration></track>
            </trackList></playlist>"#;
        let entries = parse(text, Format::Xspf, &base());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].duration, Some(213.0));
        assert_eq!(entries[0].title.as_deref(), Some("A"));
    }

    #[test]
    fn xspf_round_trips_and_escapes() {
        let entries = vec![Entry {
            path: joined(&["Rock & Roll.flac"]),
            title: Some("Rock & <Roll>".into()),
            duration: Some(200.0),
        }];
        let text = render(&entries, Format::Xspf, None);
        assert!(text.contains("&amp;"), "ampersand must be escaped");
        let back = parse(&text, Format::Xspf, &base());
        assert_eq!(back[0].title.as_deref(), Some("Rock & <Roll>"));
        assert_eq!(back[0].duration, Some(200.0));
    }

    #[test]
    fn malformed_input_yields_nothing_rather_than_panicking() {
        assert!(parse("<playlist><trackList>", Format::Xspf, &base()).is_empty());
        assert!(parse("", Format::Xspf, &base()).is_empty());
        assert!(parse("", Format::M3u8, &base()).is_empty());
        assert!(parse("nonsense with no equals", Format::Pls, &base()).is_empty());
    }

    #[test]
    fn decodes_file_uris() {
        let text = if cfg!(windows) {
            "file:///D:/music/My%20Album/01%20song.flac"
        } else {
            "file:///music/My%20Album/01%20song.flac"
        };
        let entries = parse(text, Format::M3u8, &base());
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].path.to_string_lossy().contains("My Album"),
            "percent-encoding must be decoded: {:?}",
            entries[0].path
        );
    }

    #[test]
    fn refuses_remote_entries() {
        // Internet radio is future work, not something to half-support today.
        assert!(parse("http://example.com/stream", Format::M3u8, &base()).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn refuses_unc_and_device_paths() {
        // A playlist must not be able to aim the player at a network share or a
        // raw device.
        assert!(resolve(r"\\server\share\a.flac", &base()).is_none());
        assert!(resolve(r"\\.\PhysicalDrive0", &base()).is_none());
    }

    #[test]
    fn relative_entries_stay_inside_a_moved_folder() {
        let entries = parse("sub/track.flac", Format::M3u8, &base());
        assert_eq!(entries[0].path, joined(&["sub", "track.flac"]));
    }

    #[test]
    fn parent_traversal_resolves_rather_than_escaping_into_nonsense() {
        let entries = parse("../other/track.flac", Format::M3u8, &base());
        let expected = base().parent().unwrap().join("other").join("track.flac");
        assert_eq!(entries[0].path, expected);
        // Walking above the root is refused instead of producing a stub path.
        let root = PathBuf::from(if cfg!(windows) { r"C:\a" } else { "/a" });
        assert!(resolve("../../../../../../../../etc/passwd", &root).is_none());
    }

    #[test]
    fn format_is_chosen_by_extension() {
        assert_eq!(Format::from_path(Path::new("a.M3U8")), Some(Format::M3u8));
        assert_eq!(Format::from_path(Path::new("a.pls")), Some(Format::Pls));
        assert_eq!(Format::from_path(Path::new("a.txt")), None);
    }

    #[test]
    fn writes_and_reads_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("mix.m3u8");
        let track = dir.path().join("song.flac");
        std::fs::write(&track, b"placeholder").unwrap();

        write(
            &list,
            &[Entry {
                path: track.clone(),
                title: Some("Song".into()),
                duration: Some(120.0),
            }],
            Some(dir.path()),
        )
        .unwrap();

        let back = read(&list).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, track);
    }
}
