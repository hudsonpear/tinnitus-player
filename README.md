<p align="center">
  <img height="150px" src="TinIcon.png">
</p>

# <p align="center">Tinnitus</p>

<p align="center"> A fast, native desktop music player built with Rust and GPUI. </p>

<p align="center">  <b>Supported Audio Formats:</b> mp3, flac, wav, ogg, oga, opus, m4a, m4b, mp4, aac, aiff, aif, aifc </p>

## Features

- 🎶 Media Library Management - Indexes your music into a SQLite library with metadata extraction
- 📁 Folder Watching - Monitors added folders, automatically detects new files, renames, and deletions
- 📑 Queue Management - Build and manage your playing queue
- 📜 Custom & Smart Playlists - Create your own playlists, plus auto-generated smart playlists
- ❤️ Favorites System - Mark and track your favorite tracks
- 🔍 Powerful Search - Search across your whole library, with search history
- 🗂️ Browse Your Way - Albums, artists, genres, years and folders
- 🔊 Volume Boost - Configurable max volume
- 🎚️ Equalizer - Fine-tune your audio with built-in EQ
- ⚖️ ReplayGain - Consistent loudness across tracks
- 🔀 Gapless & Crossfade - Seamless transitions between songs
- ⏩ Playback Speed - Slow down or speed up playback
- 🎨 Theming - Light/dark look, custom accent color picker, font size, rounding and density
- ☰ M3U8 Support - Import and export playlists
- ⌨️ Command Palette - Run any action from the keyboard
- 💾 Resume Playback - Picks up the queue, track and position where you left off
- 🪟 Windows Integration - Taskbar progress and thumbnail controls
- 🌐 No Internet Required - Fully offline music player

### Keyboard Shortcuts
- `Space` - Play/Pause
- `Ctrl+→` / `Ctrl+←` - Next / Previous Track
- `Ctrl+.` - Stop
- `→` / `←` - Seek Forward / Back
- `↑` / `↓` - Volume Up / Down
- `M` - Mute
- `S` - Shuffle
- `R` - Cycle Repeat
- `F` - Toggle Favorite
- `Ctrl+F` - Search
- `Ctrl+Space` - Command Palette
- `Ctrl+O` / `Ctrl+Shift+O` - Open Files / Open Folder
- `Ctrl+E` - Equalizer
- `Ctrl+J` - Queue
- `Ctrl+G` - Show Current Track
- `Ctrl+,` - Settings
- `Alt+←` / `Alt+→` - Back / Forward
- `Ctrl+A` - Select All
- `Enter` - Play Selected
- `Delete` - Delete Selected
- `F5` - Rescan Library
- `Esc` - Dismiss
- `Ctrl+Q` - Quit

## Download

Available for Windows

[Download Latest Release](https://github.com/hudsonpear/tinnitus-player/releases)

## Screenshots

![screenshot1](screenshots/1.png)
![screenshot2](screenshots/2.png)
![screenshot3](screenshots/3.png)

## How to Build

Install [Rust](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`), then run with:

```
cargo run -p tinnitus
```

Optionally pass a folder to add and scan, or a file to play:

```
cargo run -p tinnitus -- "D:\music"
```

Build the Windows installer ([Inno Setup 6](https://jrsoftware.org/isinfo.php)):

```
cargo build --release -p tinnitus
ISCC.exe installer.iss
```
