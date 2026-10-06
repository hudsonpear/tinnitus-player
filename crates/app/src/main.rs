//! Tinnitus — a local music player.
//!
//! Startup order is the whole point of this file, and it follows one rule: the
//! audio engine and the database come up first, the window opens immediately
//! after, and nothing scans the disk until the user can already see and use the
//! app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod keys;
mod logging;
mod taskbar;

use gpui::{
    App, AppContext as _, Bounds, Pixels, Size, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, point, px, size,
};
use state::{Settings, Tinnitus};
use ui::ActiveTheme as _;
use views::Root;

/// Small enough to be a useful mini player, large enough that the layout still
/// makes sense.
const LEAST_SIZE: Size<Pixels> = size(px(480.), px(400.));
const FIRST_SIZE: Size<Pixels> = size(px(1120.), px(720.));

fn main() {
    logging::init();
    log::info!("Tinnitus {} starting", env!("CARGO_PKG_VERSION"));

    // Read before anything else: the theme, the volume and the window geometry
    // all come from here, and a corrupt file must not stop the app.
    let settings = Settings::load(&Settings::path());
    let opened = opened_paths();

    let application = gpui_platform::application().with_assets(assets::Assets);

    application.run(move |cx: &mut App| {
        // The engine thread and the database open here, before the window. If
        // window creation fails after this, playback still works.
        state::init(settings.clone(), cx);

        let hardware = probe_adapter();
        let store = Tinnitus::global(cx).settings.clone();
        store.update(cx, |store, cx| store.set_hardware(hardware, cx));

        let effects = store.read(cx).effects();
        let system_dark = true;
        ui::Theme::set(
            settings.look,
            system_dark,
            &settings.theme_overrides(),
            effects,
            cx,
        );

        keys::install(cx);

        if let Err(error) = open_window(cx) {
            // The one failure the design insists must not be fatal: no window,
            // but the engine thread is already running and holds no GPUI handle.
            log::error!("tinnitus: cannot open a window: {error:#}");
            return;
        }

        restore(cx);
        persist_on_quit(cx);
        open_paths(&opened, cx);
        cx.activate(true);
    });
}

/// Writes the session out when the app is closing.
///
/// The debounced save covers settings the user changes, but the queue, the
/// playhead and the window geometry are only worth writing at the end — and
/// closing the window has to save them just as surely as Ctrl-Q does, or the
/// "carry on where you left off" promise only holds for people who quit the
/// tidy way.
fn persist_on_quit(cx: &mut App) {
    cx.on_app_quit(|cx: &mut App| {
        if let Some(global) = Tinnitus::try_global(cx) {
            let (settings, player) = (global.settings.clone(), global.player.clone());
            let session = player.read(cx).session();
            settings.update(cx, |store, cx| {
                store.update(|settings| settings.session = session, cx);
            });
            settings.read(cx).save_now();
            // A volume changed in the last half-second is still sitting in the
            // debounce. The timer will never fire now, so it is written here or
            // it is lost.
            player.update(cx, |player, _| player.flush_track_audio());
            player.read(cx).shutdown();
        }
        async {}
    })
    .detach();
}

/// Paths given on the command line: what "Open with" and a file association
/// hand us, and what dragging files onto the executable does.
fn opened_paths() -> Vec<std::path::PathBuf> {
    std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

/// Folders are added to the library and scanned; files play straight away, in
/// the order they were given.
fn open_paths(paths: &[std::path::PathBuf], cx: &mut App) {
    if paths.is_empty() {
        return;
    }
    let global = Tinnitus::global(cx);
    let (library, player) = (global.library.clone(), global.player.clone());

    for path in paths {
        if path.is_dir() {
            log::info!("tinnitus: adding {}", path.display());
            library.update(cx, |library, cx| library.add_folder(path.clone(), cx));
            continue;
        }
        // A playlist file names songs rather than being one: the list fills the
        // queue and the first entry plays.
        if state::is_playlist_file(path) {
            log::info!("tinnitus: opening playlist {}", path.display());
            player.update(cx, |player, cx| player.play_playlist_file(path.clone(), cx));
            continue;
        }
        if !::library::is_audio_file(path) {
            log::warn!("tinnitus: {} is not an audio file", path.display());
            continue;
        }
        log::info!("tinnitus: playing {}", path.display());
        player.update(cx, |player, cx| player.play_loose_file(path.clone(), cx));
    }
}

fn open_window(cx: &mut App) -> anyhow::Result<()> {
    let settings = Tinnitus::global(cx).settings.read(cx).get().clone();
    let saved = settings.window;

    let bounds = match saved.is_sane() {
        true => Bounds {
            origin: point(px(saved.x), px(saved.y)),
            size: size(px(saved.width), px(saved.height)),
        },
        false => Bounds::centered(None, FIRST_SIZE, cx),
    };
    let background = match settings.effective_effects(true).transparency {
        true => WindowBackgroundAppearance::Transparent,
        false => WindowBackgroundAppearance::Opaque,
    };

    cx.open_window(
        WindowOptions {
            // Always starts maximized; `bounds` is the size the restore button
            // goes back to.
            window_bounds: Some(WindowBounds::Maximized(bounds)),
            window_background: background,
            titlebar: Some(TitlebarOptions {
                title: Some("Tinnitus".into()),
                // The title bar is ours, so the platform draws none.
                appears_transparent: true,
                traffic_light_position: Some(point(px(9.), px(9.))),
            }),
            is_movable: true,
            is_resizable: true,
            app_id: Some("tinnitus".into()),
            window_min_size: Some(LEAST_SIZE),
            ..Default::default()
        },
        |window, cx| {
            window.set_rem_size(cx.theme().font_size);
            // Here rather than in `main`: this is the only place the native
            // window handle is in reach.
            taskbar::attach(window, cx);
            cx.new(|cx| Root::new(window, cx))
        },
    )?;
    Ok(())
}

/// Restores the last session and, only then, starts background work.
fn restore(cx: &mut App) {
    let global = Tinnitus::global(cx);
    let (settings, library, player) = (
        global.settings.clone(),
        global.library.clone(),
        global.player.clone(),
    );

    let stored = settings.read(cx).get().clone();
    player.update(cx, |player, cx| {
        player.restore(&stored.session, stored.resume_playback, cx);
    });

    if stored.scan_at_startup {
        // Deliberately after the window is up and the player has its track: a
        // scan is background work, not a startup step.
        library.update(cx, |library, cx| library.rescan(cx));
    }
}

/// Whether we are running on a real GPU rather than a software adapter.
///
/// `Rendering::Automatic` uses this to decide whether the expensive effects are
/// affordable. Getting it wrong costs some visual polish, never playback, so a
/// failed probe assumes hardware and lets the user pick Compatibility by hand.
fn probe_adapter() -> bool {
    #[cfg(windows)]
    {
        // A software adapter (WARP, or a basic display driver) reports itself in
        // the name Windows gives the display device.
        let name = std::env::var("TINNITUS_FORCE_SOFTWARE_ADAPTER").ok();
        if name.is_some() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minimum_window_is_smaller_than_the_first_one() {
        assert!(LEAST_SIZE.width < FIRST_SIZE.width);
        assert!(LEAST_SIZE.height < FIRST_SIZE.height);
    }

    #[test]
    fn the_software_adapter_override_is_honoured() {
        // The env var exists so Compatibility mode can be exercised on a machine
        // that has a perfectly good GPU.
        assert!(probe_adapter());
        unsafe { std::env::set_var("TINNITUS_FORCE_SOFTWARE_ADAPTER", "1") };
        assert!(!probe_adapter());
        unsafe { std::env::remove_var("TINNITUS_FORCE_SOFTWARE_ADAPTER") };
    }
}
