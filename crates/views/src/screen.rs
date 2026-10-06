//! What the main content area is showing, and how the user got there.

use library::models::LibraryView;

/// A screen, plus enough history to make Back work.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    /// The landing page: what is playing, what the library holds, where to go.
    Home,
    /// A track list over some slice of the library.
    Tracks(LibraryView),
    /// The album grid.
    Albums,
    Artists,
    Genres,
    Years,
    Folders,
    /// Every playlist on one page: the hand-made ones, then the smart ones.
    Playlists,
    Queue,
    Search,
    Settings,
    Equalizer,
}

/// Which kind of result the Search page is showing.
///
/// ponytail: no Videos chip. The library indexes audio only, so it would be a
/// tab that is always empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Playlists,
    Albums,
    Artists,
    Genres,
}

impl SearchFilter {
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Songs,
        Self::Playlists,
        Self::Albums,
        Self::Artists,
        Self::Genres,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Songs => "Songs",
            Self::Playlists => "Playlists",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Genres => "Genres",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::All => "grid",
            Self::Songs => "music",
            Self::Playlists => "playlist",
            Self::Albums => "album",
            Self::Artists => "artist",
            Self::Genres => "genre",
        }
    }

    /// Whether a section belongs on screen under this chip.
    pub fn shows(self, section: Self) -> bool {
        self == Self::All || self == section
    }
}

impl Screen {
    /// The title shown at the top of the content area.
    pub fn title(&self) -> String {
        match self {
            Self::Home => "Home".into(),
            Self::Tracks(view) => view_title(view),
            Self::Albums => "Albums".into(),
            Self::Artists => "Artists".into(),
            Self::Genres => "Genres".into(),
            Self::Years => "Years".into(),
            Self::Folders => "Folders".into(),
            Self::Playlists => "Playlists".into(),
            Self::Queue => "Queue".into(),
            Self::Search => "Search".into(),
            Self::Settings => "Settings".into(),
            Self::Equalizer => "Equalizer".into(),
        }
    }

    /// The library view this screen reads, if it reads one.
    pub fn library_view(&self) -> Option<LibraryView> {
        match self {
            Self::Tracks(view) => Some(view.clone()),
            _ => None,
        }
    }
}

fn view_title(view: &LibraryView) -> String {
    match view {
        LibraryView::AllSongs => "All Songs".into(),
        LibraryView::RecentlyAdded => "Recently Added".into(),
        LibraryView::RecentlyPlayed => "Recently Played".into(),
        LibraryView::MostPlayed => "Most Played".into(),
        LibraryView::Favorites => "Favorites".into(),
        LibraryView::Album(_) => "Album".into(),
        LibraryView::Artist(_) => "Artist".into(),
        LibraryView::Genre(name) => name.clone(),
        LibraryView::Year(year) => year.to_string(),
        LibraryView::Folder(_) => "Folder".into(),
        LibraryView::Playlist(_) => "Playlist".into(),
        LibraryView::Albums => "Albums".into(),
        LibraryView::Artists => "Artists".into(),
        LibraryView::Genres => "Genres".into(),
        LibraryView::Years => "Years".into(),
        LibraryView::Folders => "Folders".into(),
    }
}

/// Navigation history. Small on purpose: this is a music player, not a browser.
#[derive(Debug, Clone)]
pub struct History {
    current: Screen,
    back: Vec<Screen>,
    forward: Vec<Screen>,
}

/// Deepest history kept. Enough to retrace a browse, not enough to leak.
const DEPTH: usize = 32;

impl History {
    pub fn new(start: Screen) -> Self {
        Self {
            current: start,
            back: vec![],
            forward: vec![],
        }
    }

    pub fn current(&self) -> &Screen {
        &self.current
    }

    /// Where `back` would land, without going there. A caller has to know what
    /// it is leaving for before it leaves.
    pub fn peek_back(&self) -> Option<&Screen> {
        self.back.last()
    }

    pub fn peek_forward(&self) -> Option<&Screen> {
        self.forward.last()
    }

    pub fn go(&mut self, screen: Screen) {
        if self.current == screen {
            return;
        }
        self.back.push(std::mem::replace(&mut self.current, screen));
        if self.back.len() > DEPTH {
            self.back.remove(0);
        }
        // A new destination ends the forward trail, as everywhere else.
        self.forward.clear();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn back(&mut self) -> Option<&Screen> {
        let previous = self.back.pop()?;
        self.forward
            .push(std::mem::replace(&mut self.current, previous));
        Some(&self.current)
    }

    pub fn forward(&mut self) -> Option<&Screen> {
        let next = self.forward.pop()?;
        self.back.push(std::mem::replace(&mut self.current, next));
        Some(&self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward_retrace_the_same_path() {
        let mut history = History::new(Screen::Tracks(LibraryView::AllSongs));
        history.go(Screen::Albums);
        history.go(Screen::Queue);

        assert_eq!(history.back(), Some(&Screen::Albums));
        assert_eq!(history.back(), Some(&Screen::Tracks(LibraryView::AllSongs)));
        assert_eq!(history.back(), None);

        assert_eq!(history.forward(), Some(&Screen::Albums));
        assert_eq!(history.forward(), Some(&Screen::Queue));
        assert_eq!(history.forward(), None);
    }

    #[test]
    fn a_new_destination_ends_the_forward_trail() {
        let mut history = History::new(Screen::Albums);
        history.go(Screen::Queue);
        history.back();
        assert!(history.can_go_forward());

        history.go(Screen::Settings);
        assert!(!history.can_go_forward());
    }

    #[test]
    fn navigating_to_where_you_already_are_does_nothing() {
        let mut history = History::new(Screen::Albums);
        history.go(Screen::Albums);
        assert!(!history.can_go_back());
    }

    #[test]
    fn history_does_not_grow_without_bound() {
        let mut history = History::new(Screen::Albums);
        for year in 0..(DEPTH as i32 * 3) {
            history.go(Screen::Tracks(LibraryView::Year(year)));
        }
        assert!(history.back.len() <= DEPTH);
    }

    #[test]
    fn every_screen_has_a_title() {
        for screen in [
            Screen::Home,
            Screen::Tracks(LibraryView::AllSongs),
            Screen::Tracks(LibraryView::Genre("Rock".into())),
            Screen::Albums,
            Screen::Queue,
            Screen::Settings,
            Screen::Equalizer,
        ] {
            assert!(!screen.title().is_empty(), "{screen:?} has no title");
        }
        assert_eq!(
            Screen::Tracks(LibraryView::Genre("Jazz".into())).title(),
            "Jazz"
        );
    }
}
