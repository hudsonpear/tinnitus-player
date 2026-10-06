//! Application state.
//!
//! Three entities rather than one object: the player, the library, and the
//! settings. They are held in one global so a view can reach any of them, but
//! they do not know about each other beyond the handles they are given.

pub mod library;
pub mod player;
pub mod settings;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Global, Task};

pub use library::{Library, LibraryEvent, ScanProgress};
pub use player::{Player, PlayerEvent, is_playlist_file};
pub use settings::{
    MAX_VOLUMES, SEARCH_HISTORY, Session, Settings, Timeline, TimelinePlace, WindowState,
    effective_gain,
};

/// How long to wait after the last change before writing settings to disk.
/// Dragging a slider should not be a write per pixel.
const SAVE_AFTER: Duration = Duration::from_millis(600);

pub struct Tinnitus {
    pub settings: Entity<SettingsStore>,
    pub library: Entity<Library>,
    pub player: Entity<Player>,
}

impl Global for Tinnitus {}

impl Tinnitus {
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn try_global(cx: &App) -> Option<&Self> {
        cx.try_global::<Self>()
    }
}

/// Opens the database, starts the audio engine, and installs the global.
///
/// Order matters and follows the startup rule: the database and the engine come
/// up first, the window is opened by the caller afterwards, and no scanning
/// happens until the caller asks for it.
pub fn init(settings: Settings, cx: &mut App) {
    let db = match ::library::Db::open(&::library::database_path()) {
        Ok(db) => db,
        Err(error) => {
            // A library we cannot open must not stop the user playing a file.
            log::error!("state: cannot open the library, running without one: {error:#}");
            ::library::Db::in_memory().expect("an in-memory database cannot fail to open")
        }
    };
    let db = Arc::new(Mutex::new(db));

    let artwork = match ::library::ArtworkCache::new(::library::artwork_dir()) {
        Ok(cache) => cache,
        Err(error) => {
            log::error!("state: cannot open the artwork cache: {error:#}");
            // A cache in the temp directory still works for this session.
            ::library::ArtworkCache::new(std::env::temp_dir().join("tinnitus-artwork"))
                .expect("the temp directory is writable")
        }
    };
    let artwork = Arc::new(artwork);

    let settings_store = cx.new(|cx| SettingsStore::new(settings, cx));
    let stored = settings_store.read(cx).settings.clone();

    let library = cx.new(|cx| Library::new(db.clone(), artwork, cx));
    let player = cx.new(|cx| Player::new(db, &stored, cx));

    cx.set_global(Tinnitus {
        settings: settings_store,
        library,
        player,
    });
}

/// Holds the settings and writes them back, debounced.
pub struct SettingsStore {
    pub settings: Settings,
    /// True once the adapter probe says we are on real hardware.
    hardware: bool,
    save: Option<Task<()>>,
}

impl SettingsStore {
    fn new(settings: Settings, _cx: &mut Context<Self>) -> Self {
        Self {
            settings,
            hardware: true,
            save: None,
        }
    }

    pub fn get(&self) -> &Settings {
        &self.settings
    }

    /// Every change goes through here, so nothing can edit the settings without
    /// scheduling a save.
    pub fn update(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.settings);
        self.settings = std::mem::take(&mut self.settings).sanitized();
        self.schedule_save(cx);
        cx.notify();
    }

    /// Records what the graphics adapter turned out to be, which is what
    /// `Rendering::Automatic` decides on.
    pub fn set_hardware(&mut self, hardware: bool, cx: &mut Context<Self>) {
        self.hardware = hardware;
        cx.notify();
    }

    pub fn effects(&self) -> ui::Effects {
        self.settings.effective_effects(self.hardware)
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        self.save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let path = Settings::path();
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = settings.save(&path) {
                        log::warn!("settings: cannot save: {error:#}");
                    }
                })
                .await;
        }));
    }

    /// Writes immediately, for shutdown. The debounced task would never fire.
    pub fn save_now(&self) {
        if let Err(error) = self.settings.save(&Settings::path()) {
            log::warn!("settings: cannot save on exit: {error:#}");
        }
    }
}
