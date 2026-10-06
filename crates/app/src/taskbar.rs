//! The taskbar button: a second seek bar, and a second transport.
//!
//! Windows draws a progress bar behind an application's taskbar button, and it
//! is coloured by state rather than by us: normal is green, paused is yellow.
//! That maps exactly onto a player, so playing a track fills the button green as
//! it goes and pausing turns the same fill yellow — how far through the song we
//! are stays readable with the window buried.
//!
//! Hovering that button also raises a thumbnail, and the shell will draw a small
//! toolbar under it: previous, play/pause and next.
//! Everywhere that is not Windows this is a no-op.

use gpui::{App, Window};

#[cfg(not(windows))]
pub fn attach(_window: &Window, _cx: &mut App) {}

#[cfg(windows)]
pub fn attach(window: &Window, cx: &mut App) {
    use futures::StreamExt as _;
    use state::Tinnitus;

    let Some(hwnd) = platform::hwnd_of(window) else {
        log::warn!("taskbar: no native window handle, the taskbar button is plain");
        return;
    };
    let Some(taskbar) = platform::taskbar() else {
        return;
    };

    // Clicks come in on the window procedure, which is not a place a GPUI
    // entity can be touched. They are sent down this channel instead and applied
    // by a task on the foreground executor.
    let (clicked, mut clicks) = futures::channel::mpsc::unbounded();
    let Some(bar) = platform::Bar::install(hwnd, taskbar, clicked) else {
        return;
    };

    let player = Tinnitus::global(cx).player.clone();
    // The player notifies five times a second while it plays. The bar keeps the
    // last values it wrote, so a repeat is dropped rather than redrawing the
    // button for a change no one can see.
    cx.observe(&player, move |player, cx| {
        let read = player.read(cx);
        let (playing, at, of) = (read.is_playing(), read.position(), read.length());
        bar.borrow_mut().show(playing, at, of);
    })
    .detach();

    let player = Tinnitus::global(cx).player.clone();
    cx.spawn(async move |cx| {
        while let Some(command) = clicks.next().await {
            match command {
                platform::Command::Previous => {
                    player.update(cx, |player, cx| player.previous(cx));
                }
                platform::Command::Toggle => {
                    player.update(cx, |player, cx| player.toggle(cx));
                }
                platform::Command::Next => {
                    player.update(cx, |player, cx| player.next(cx));
                }
            }
        }
    })
    .detach();
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::rc::Rc;

    use futures::channel::mpsc::UnboundedSender;
    use gpui::Window;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject,
    };
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::Win32::UI::Shell::{
        DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, TBPF_NOPROGRESS,
        TBPF_NORMAL, TBPF_PAUSED, TBPFLAG, THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_ENABLED,
        THBN_CLICKED, THUMBBUTTON, TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, RegisterWindowMessageW,
        SM_CXSMICON, WM_COMMAND, WM_NCDESTROY,
    };
    use windows::core::w;

    /// Which button was pressed. Ids are what the shell hands back in the
    /// `WM_COMMAND`, so they have to stay stable between adding and clicking.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Command {
        Previous,
        Toggle,
        Next,
    }

    const PREVIOUS: u32 = 1;
    const TOGGLE: u32 = 2;
    const NEXT: u32 = 3;

    /// Ours alone among whatever else subclasses this window.
    const SUBCLASS: usize = 0x54_49_4e;

    /// The window's `HWND`, which GPUI hands out through `raw-window-handle`.
    pub fn hwnd_of(window: &Window) -> Option<HWND> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        // Fully qualified: `Window` has an inherent `window_handle` of its own
        // that returns GPUI's handle, not the platform's.
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(win32) => Some(HWND(win32.hwnd.get() as *mut std::ffi::c_void)),
            _ => None,
        }
    }

    /// The shell's taskbar list. GPUI has already initialised COM on this
    /// thread; a failure here costs the progress bar and nothing else.
    pub fn taskbar() -> Option<ITaskbarList3> {
        let list: ITaskbarList3 = match unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_ALL) }
        {
            Ok(list) => list,
            Err(error) => {
                log::warn!("taskbar: cannot reach the shell, progress is off: {error}");
                return None;
            }
        };
        if let Err(error) = unsafe { list.HrInit() } {
            log::warn!("taskbar: cannot initialise the taskbar list: {error}");
            return None;
        }
        Some(list)
    }

    /// Everything the taskbar button needs, in one place because the window
    /// procedure and the player observer both write to it.
    pub struct Bar {
        hwnd: HWND,
        taskbar: ITaskbarList3,
        /// Previous, play, pause, next. Owned here so they can be destroyed
        /// with the window; the buttons only borrow them.
        icons: [HICON; 4],
        buttons: [THUMBBUTTON; 3],
        /// The toolbar can only be added once the shell has made the button,
        /// and has to be added again if Explorer restarts.
        added: bool,
        adding: bool,
        playing: bool,
        shown: Option<(TBPFLAG, u64, u64)>,
        /// `TaskbarButtonCreated`, which is registered rather than constant.
        created: u32,
        /// `TaskbarCreated`, sent when Explorer itself restarts.
        shell_restarted: u32,
        clicked: UnboundedSender<Command>,
    }

    impl Bar {
        /// Draws the icons, subclasses the window and hands back the shared bar.
        ///
        /// Subclassing is the whole trick: GPUI owns the window procedure, and a
        /// thumbnail-toolbar click arrives as an ordinary `WM_COMMAND` that
        /// nothing else would look at.
        pub fn install(
            hwnd: HWND,
            taskbar: ITaskbarList3,
            clicked: UnboundedSender<Command>,
        ) -> Option<Rc<RefCell<Bar>>> {
            let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
            let dark = dark_glyphs();
            let icons = [
                icon(Glyph::Previous, size, dark)?,
                icon(Glyph::Play, size, dark)?,
                icon(Glyph::Pause, size, dark)?,
                icon(Glyph::Next, size, dark)?,
            ];
            let buttons = [
                button(PREVIOUS, icons[0], "Previous"),
                button(TOGGLE, icons[1], "Play"),
                button(NEXT, icons[3], "Next"),
            ];

            let bar = Rc::new(RefCell::new(Bar {
                hwnd,
                taskbar,
                icons,
                buttons,
                added: false,
                adding: false,
                playing: false,
                shown: None,
                created: unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) },
                shell_restarted: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
                clicked,
            }));

            let held = Rc::into_raw(bar.clone());
            let watched =
                unsafe { SetWindowSubclass(hwnd, Some(handle), SUBCLASS, held as usize) }.as_bool();
            if !watched {
                // The raw handle is the subclass's; nothing holds it now.
                drop(unsafe { Rc::from_raw(held) });
                log::warn!("taskbar: cannot watch the window, the transport buttons are off");
                return None;
            }
            Some(bar)
        }

        /// The progress bar and the play/pause icon, from one player update.
        pub fn show(&mut self, playing: bool, at: f64, of: f64) {
            self.set_playing(playing);

            // A track whose length is not known yet has nothing to show a
            // fraction of, and neither does an empty player.
            let next = match of > 0.0 {
                false => (TBPF_NOPROGRESS, 0, 0),
                true => (
                    match playing {
                        true => TBPF_NORMAL,
                        false => TBPF_PAUSED,
                    },
                    // Whole seconds. The bar is a hundred or so pixels wide.
                    at.max(0.0) as u64,
                    of as u64,
                ),
            };
            if self.shown == Some(next) {
                return;
            }
            self.shown = Some(next);
            unsafe {
                // The state goes first: a value written while the button is in
                // NOPROGRESS is discarded.
                let _ = self.taskbar.SetProgressState(self.hwnd, next.0);
                if next.2 > 0 {
                    let _ = self
                        .taskbar
                        .SetProgressValue(self.hwnd, next.1.min(next.2), next.2);
                }
            }
        }

        fn set_playing(&mut self, playing: bool) {
            if self.playing == playing {
                return;
            }
            self.playing = playing;
            let (glyph, tip) = match playing {
                true => (self.icons[2], "Pause"),
                false => (self.icons[1], "Play"),
            };
            self.buttons[1].hIcon = glyph;
            self.buttons[1].szTip = tip16(tip);
            if self.added
                && let Err(error) = unsafe {
                    self.taskbar
                        .ThumbBarUpdateButtons(self.hwnd, &self.buttons[1..2])
                }
            {
                log::warn!("taskbar: cannot redraw the play button: {error}");
            }
        }

        /// Adds the toolbar. Called on `TaskbarButtonCreated`, which arrives
        /// when the window is first shown and again if Explorer restarts — the
        /// second time the buttons are gone and have to be put back.
        fn add(bar: &RefCell<Bar>) {
            // ThumbBarAddButtons can synchronously dispatch another window
            // message. Do not keep a RefCell borrow across that shell call.
            let (taskbar, hwnd, buttons) = {
                let mut bar = bar.borrow_mut();
                if bar.added || bar.adding {
                    return;
                }
                bar.adding = true;
                (bar.taskbar.clone(), bar.hwnd, bar.buttons)
            };

            let result = unsafe { taskbar.ThumbBarAddButtons(hwnd, &buttons) };
            let mut bar = bar.borrow_mut();
            bar.adding = false;
            bar.added = result.is_ok();
            if let Err(error) = result {
                log::warn!("taskbar: cannot add the transport buttons: {error}");
            }
        }

        fn click(&self, id: u32) {
            let command = match id {
                PREVIOUS => Command::Previous,
                TOGGLE => Command::Toggle,
                NEXT => Command::Next,
                _ => return,
            };
            // A closed channel means the app is on its way out.
            let _ = self.clicked.unbounded_send(command);
        }
    }

    impl Drop for Bar {
        fn drop(&mut self) {
            for icon in self.icons {
                unsafe { DestroyIcon(icon).ok() };
            }
        }
    }

    /// The subclass procedure: it runs ahead of GPUI's own, takes the two
    /// messages we care about, and passes everything else straight down.
    ///
    /// **Nothing here holds a borrow of the `Bar` across a Win32 call.**
    unsafe extern "system" fn handle(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        let bar = unsafe { &*(data as *const RefCell<Bar>) };

        // The shell reports a thumbnail-toolbar press as a WM_COMMAND whose
        // high word says so and whose low word is the button's id.
        if message == WM_COMMAND && ((wparam.0 >> 16) & 0xffff) as u32 == THBN_CLICKED {
            bar.borrow().click((wparam.0 & 0xffff) as u32);
            return LRESULT(0);
        }

        // Both registered ids in one short borrow, rather than a borrow per
        // comparison interleaved with the calls that act on them.
        let (created, shell_restarted) = {
            let bar = bar.borrow();
            (bar.created, bar.shell_restarted)
        };
        if message == created || message == shell_restarted {
            Bar::add(bar);
        }
        if message == WM_NCDESTROY {
            // Before the subclass goes, while the handle is still good.
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(handle), SUBCLASS).ok() };
            // The reference the subclass was holding. `bar` must not be touched
            // after this.
            drop(unsafe { Rc::from_raw(data as *const RefCell<Bar>) });
        }
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    fn button(id: u32, icon: HICON, tip: &str) -> THUMBBUTTON {
        THUMBBUTTON {
            dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
            iId: id,
            iBitmap: 0,
            hIcon: icon,
            szTip: tip16(tip),
            // Always enabled: pressing next on an empty queue stops, which is
            // what the same button in the window does.
            dwFlags: THBF_ENABLED,
        }
    }

    /// A tooltip in the fixed-size buffer the shell expects, always terminated.
    fn tip16(text: &str) -> [u16; 260] {
        let mut buffer = [0u16; 260];
        for (slot, unit) in buffer.iter_mut().take(259).zip(text.encode_utf16()) {
            *slot = unit;
        }
        buffer
    }

    // -- the glyphs -------------------------------------------------------
    //
    // Three triangles and a few bars, drawn here rather than shipped as an .ico:
    // the app's own icon set is SVG, which is no use to Win32, and rasterising
    // shapes this simple is less code than carrying a second set of assets.

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Glyph {
        Previous,
        Play,
        Pause,
        Next,
    }

    /// How tall the glyphs are, as a half-height either side of the middle.
    const REACH: f32 = 0.38;

    /// Whether a point in the unit square is inside the glyph.
    fn covered(glyph: Glyph, x: f32, y: f32) -> bool {
        match glyph {
            Glyph::Play => wedge(x, y, 0.26, 0.84),
            Glyph::Pause => bar(x, y, 0.24, 0.42) || bar(x, y, 0.58, 0.76),
            Glyph::Next => wedge(x, y, 0.16, 0.62) || bar(x, y, 0.70, 0.84),
            // The same glyph, seen in a mirror.
            Glyph::Previous => covered(Glyph::Next, 1.0 - x, y),
        }
    }

    /// A triangle on its side: a vertical edge at `back`, a point at `tip`.
    fn wedge(x: f32, y: f32, back: f32, tip: f32) -> bool {
        if x < back || x > tip {
            return false;
        }
        let along = (tip - x) / (tip - back);
        (y - 0.5).abs() <= REACH * along
    }

    fn bar(x: f32, y: f32, left: f32, right: f32) -> bool {
        x >= left && x <= right && (y - 0.5).abs() <= REACH
    }

    /// How much of one pixel the glyph covers, 0.0..=1.0.
    ///
    /// Sampled on a 3x3 grid. At sixteen pixels across, a hard edge on a
    /// diagonal is the difference between a play button and a staircase.
    fn coverage(glyph: Glyph, px: i32, py: i32, size: i32) -> f32 {
        const GRID: i32 = 3;
        let mut hits = 0;
        for sy in 0..GRID {
            for sx in 0..GRID {
                let x = (px as f32 + (sx as f32 + 0.5) / GRID as f32) / size as f32;
                let y = (py as f32 + (sy as f32 + 0.5) / GRID as f32) / size as f32;
                if covered(glyph, x, y) {
                    hits += 1;
                }
            }
        }
        hits as f32 / (GRID * GRID) as f32
    }

    /// Whether the thumbnail toolbar is drawn dark, and so wants light glyphs.
    ///
    /// The flyout follows the taskbar's theme rather than the app's, and that is
    /// the value the taskbar itself reads. Missing (an older build, a locked-down
    /// profile) means dark, which is Windows' own default.
    ///
    /// ponytail: read once, at startup. Someone who switches the system theme
    /// while the app is running gets the old glyphs until they restart it —
    /// fixing that means handling WM_SETTINGCHANGE and redrawing all four.
    fn dark_glyphs() -> bool {
        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        let read = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("SystemUsesLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(std::ptr::addr_of_mut!(value).cast()),
                Some(&mut size),
            )
        };
        read.is_err() || value == 0
    }

    /// One glyph as an icon, white on nothing or black on nothing.
    fn icon(glyph: Glyph, size: i32, dark: bool) -> Option<HICON> {
        let tone: u32 = match dark {
            true => 255,
            false => 0,
        };

        let header = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            // Negative: top-down, so row 0 is the top one.
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };

        let mut pixels: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut pixels, None, 0) }
            .inspect_err(|error| log::warn!("taskbar: cannot draw a button: {error}"))
            .ok()?;

        // Premultiplied BGRA, which is what an alpha-blended icon is made of.
        let pixels = pixels.cast::<u32>();
        for py in 0..size {
            for px in 0..size {
                let alpha = (coverage(glyph, px, py, size) * 255.0).round() as u32;
                let lit = tone * alpha / 255;
                let value = (alpha << 24) | (lit << 16) | (lit << 8) | lit;
                unsafe { pixels.offset((py * size + px) as isize).write(value) };
            }
        }

        // The alpha channel does the masking; this only has to exist.
        let mask = unsafe { CreateBitmap(size, size, 1, 1, None) };
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = unsafe { CreateIconIndirect(&info) }
            .inspect_err(|error| log::warn!("taskbar: cannot make a button icon: {error}"))
            .ok();

        // CreateIconIndirect copies both.
        unsafe {
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
        }
        icon
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_play_glyph_is_a_triangle_pointing_right() {
            // Solid behind the edge, empty past the point, and cut away at the
            // corners — which is what makes it a triangle rather than a block.
            assert!(covered(Glyph::Play, 0.30, 0.5));
            assert!(covered(Glyph::Play, 0.70, 0.5));
            assert!(!covered(Glyph::Play, 0.90, 0.5));
            assert!(!covered(Glyph::Play, 0.20, 0.5));
            assert!(covered(Glyph::Play, 0.28, 0.20));
            assert!(!covered(Glyph::Play, 0.80, 0.20));
        }

        #[test]
        fn the_pause_glyph_has_a_gap_down_the_middle() {
            assert!(covered(Glyph::Pause, 0.30, 0.5));
            assert!(!covered(Glyph::Pause, 0.50, 0.5));
            assert!(covered(Glyph::Pause, 0.65, 0.5));
        }

        #[test]
        fn previous_is_next_in_a_mirror() {
            for step in 0..=20 {
                let x = step as f32 / 20.0;
                for y in [0.1, 0.35, 0.5, 0.65, 0.9] {
                    assert_eq!(
                        covered(Glyph::Next, x, y),
                        covered(Glyph::Previous, 1.0 - x, y),
                        "{x} {y}"
                    );
                }
            }
        }

        #[test]
        fn the_skip_glyphs_have_their_bar_on_the_leading_side() {
            // The bar belongs in front of the triangle for next and behind it
            // for previous; getting that backwards draws two buttons that point
            // the same way.
            assert!(covered(Glyph::Next, 0.78, 0.5));
            assert!(!covered(Glyph::Next, 0.66, 0.5));
            assert!(!covered(Glyph::Next, 0.10, 0.5));
            assert!(covered(Glyph::Previous, 0.22, 0.5));
            assert!(!covered(Glyph::Previous, 0.34, 0.5));
            assert!(!covered(Glyph::Previous, 0.90, 0.5));
        }

        #[test]
        fn coverage_stays_a_fraction_and_softens_the_edges() {
            let size = 16;
            let mut edges = 0;
            for py in 0..size {
                for px in 0..size {
                    let at = coverage(Glyph::Play, px, py, size);
                    assert!((0.0..=1.0).contains(&at), "{px},{py} = {at}");
                    if at > 0.0 && at < 1.0 {
                        edges += 1;
                    }
                }
            }
            // The diagonals have to land on something between on and off, or
            // there is no antialiasing happening at all.
            assert!(edges > 0);
        }

        #[test]
        fn a_tooltip_is_copied_in_and_terminated() {
            let tip = tip16("Play");
            assert_eq!(
                &tip[..4],
                &[b'P' as u16, b'l' as u16, b'a' as u16, b'y' as u16]
            );
            assert_eq!(tip[4], 0);
            // Longer than the buffer: truncated, still terminated.
            let long = tip16(&"x".repeat(400));
            assert_eq!(long[258], b'x' as u16);
            assert_eq!(long[259], 0);
        }
    }
}
