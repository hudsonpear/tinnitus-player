//! The playback queue.
//!
//! The queue is deliberately separate from playlists: sending a song to the
//! queue must never edit the playlist it came from. Playing a playlist *fills*
//! the queue from it; after that the two are independent.
//!
//! The queue holds track ids, not rows, so queueing ten thousand tracks costs a
//! vector of integers.

use serde::{Deserialize, Serialize};

pub type TrackId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    /// Wrap around to the start when the last track finishes.
    All,
    /// Repeat the current track. An explicit Next still moves on — repeat-one is
    /// about what happens when a track *ends*, not about trapping the user.
    One,
}

impl Repeat {
    /// Cycles Off -> All -> One -> Off, which is what the toolbar button does.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

/// Why the queue is advancing. A track that ran out obeys repeat-one; a user
/// pressing Next does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// The current track ended on its own.
    Ended,
    /// The user asked for the next track.
    User,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    /// Tracks in the order they were added.
    tracks: Vec<TrackId>,
    /// Indices into `tracks`, in playback order. Identity while unshuffled.
    order: Vec<usize>,
    /// Index into `order`, not into `tracks`.
    position: Option<usize>,
    shuffle: bool,
    repeat: Repeat,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the queue with `tracks` and starts at `start` (an index into
    /// `tracks`). This is "play this album from track 4".
    pub fn fill(&mut self, tracks: Vec<TrackId>, start: usize) {
        self.tracks = tracks;
        self.rebuild_order();
        self.position = match self.tracks.is_empty() {
            true => None,
            false => match self.order.iter().position(|index| *index == start) {
                // When shuffled, the track the user clicked leads the queue, so
                // the panel reads from the top rather than from the middle.
                Some(at) if self.shuffle => self.lead_with(at),
                Some(at) => Some(at),
                None => Some(0),
            },
        };
    }

    /// Replaces the queue and starts on whatever the shuffle dealt first. This
    /// is "Shuffle All": no track was clicked, so no track gets to lead, and
    /// forcing one in would open every shuffle with the same song.
    pub fn fill_shuffled(&mut self, tracks: Vec<TrackId>) {
        self.tracks = tracks;
        self.shuffle = true;
        self.rebuild_order();
        self.position = (!self.order.is_empty()).then_some(0);
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// One track in playback order. The queue panel draws a row at a time, and
    /// rebuilding the whole order for each of them would make scrolling a long
    /// queue quadratic.
    pub fn id_at(&self, index: usize) -> Option<TrackId> {
        let index = *self.order.get(index)?;
        self.tracks.get(index).copied()
    }

    /// Tracks in playback order, which is what the queue panel shows.
    pub fn in_play_order(&self) -> Vec<TrackId> {
        self.order
            .iter()
            .filter_map(|index| self.tracks.get(*index))
            .copied()
            .collect()
    }

    pub fn current(&self) -> Option<TrackId> {
        let index = *self.order.get(self.position?)?;
        self.tracks.get(index).copied()
    }

    /// Position of the playing track within `in_play_order`.
    pub fn current_index(&self) -> Option<usize> {
        self.position
    }

    /// The track that would play next, without moving. Used to preload for
    /// gapless playback.
    pub fn peek_next(&self) -> Option<TrackId> {
        let position = self.position?;
        if self.repeat == Repeat::One {
            return self.current();
        }
        let next = position + 1;
        let next = match (next >= self.order.len(), self.repeat) {
            (true, Repeat::All) => 0,
            (true, _) => return None,
            (false, _) => next,
        };
        self.tracks.get(*self.order.get(next)?).copied()
    }

    /// Moves to the next track and returns it, or `None` when playback should stop.
    pub fn advance(&mut self, reason: Advance) -> Option<TrackId> {
        let position = self.position?;
        if reason == Advance::Ended && self.repeat == Repeat::One {
            return self.current();
        }

        let next = position + 1;
        if next >= self.order.len() {
            // Repeat-all wraps. A user pressing Next at the end also wraps: the
            // alternative is a dead button.
            if self.repeat == Repeat::All || reason == Advance::User {
                if self.order.is_empty() {
                    return None;
                }
                self.position = Some(0);
                return self.current();
            }
            return None;
        }
        self.position = Some(next);
        self.current()
    }

    /// Moves to the previous track. Wraps to the end from the first track.
    pub fn previous(&mut self) -> Option<TrackId> {
        let position = self.position?;
        let previous = match position == 0 {
            true => self.order.len().checked_sub(1)?,
            false => position - 1,
        };
        self.position = Some(previous);
        self.current()
    }

    /// Jumps to a position in the play order, e.g. a double-click in the queue.
    pub fn jump_to(&mut self, index: usize) -> Option<TrackId> {
        if index >= self.order.len() {
            return None;
        }
        self.position = Some(index);
        self.current()
    }

    /// Appends to the end of the queue.
    pub fn append(&mut self, tracks: &[TrackId]) {
        for track in tracks {
            self.tracks.push(*track);
            self.order.push(self.tracks.len() - 1);
        }
        if self.position.is_none() && !self.order.is_empty() {
            self.position = Some(0);
        }
    }

    /// Inserts right after the current track, so "Play next" jumps the line
    /// without disturbing the rest of the queue.
    pub fn play_next(&mut self, tracks: &[TrackId]) {
        let mut at = self.position.map(|position| position + 1).unwrap_or(0);
        for track in tracks {
            self.tracks.push(*track);
            let index = self.tracks.len() - 1;
            let at_clamped = at.min(self.order.len());
            self.order.insert(at_clamped, index);
            at = at_clamped + 1;
        }
        if self.position.is_none() && !self.order.is_empty() {
            self.position = Some(0);
        }
    }

    /// Removes the entry at a play-order position. Removing the playing track
    /// leaves the cursor on whatever slid into its place.
    pub fn remove(&mut self, index: usize) {
        if index >= self.order.len() {
            return;
        }
        let track_index = self.order.remove(index);
        self.tracks.remove(track_index);
        // Every stored index past the removed one shifts down by one.
        for entry in &mut self.order {
            if *entry > track_index {
                *entry -= 1;
            }
        }

        self.position = match self.position {
            None => None,
            Some(_) if self.order.is_empty() => None,
            Some(position) if position > index => Some(position - 1),
            Some(position) => Some(position.min(self.order.len() - 1)),
        };
    }

    /// Drag-and-drop reordering inside the queue.
    pub fn move_entry(&mut self, from: usize, to: usize) {
        if from >= self.order.len() || to >= self.order.len() || from == to {
            return;
        }
        let playing = self.current();
        let entry = self.order.remove(from);
        self.order.insert(to, entry);
        // The cursor follows the track that is playing, not the slot it was in.
        if let Some(playing) = playing {
            self.position = self
                .order
                .iter()
                .position(|index| self.tracks.get(*index) == Some(&playing));
        }
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.order.clear();
        self.position = None;
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Turning shuffle on reshuffles everything around the current track, which
    /// keeps playing and becomes the first row of the queue — what is playing is
    /// the head of the list, and everything below it is what comes next.
    /// Turning shuffle off restores the added order and finds the track in it.
    pub fn set_shuffle(&mut self, shuffle: bool) {
        if self.shuffle == shuffle {
            return;
        }
        self.shuffle = shuffle;
        let playing = self.current();
        self.rebuild_order();
        self.position = match playing {
            Some(playing) => {
                let at = self
                    .order
                    .iter()
                    .position(|index| self.tracks.get(*index) == Some(&playing));
                match at {
                    Some(at) if self.shuffle => self.lead_with(at),
                    other => other,
                }
            }
            None => self.position.filter(|_| !self.order.is_empty()),
        };
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Drops ids that no longer exist, e.g. after the library was cleaned up.
    pub fn retain_known(&mut self, known: &dyn Fn(TrackId) -> bool) {
        let playing = self.current();
        self.tracks.retain(|id| known(*id));
        self.rebuild_order();
        self.position = playing
            .and_then(|playing| {
                self.order
                    .iter()
                    .position(|index| self.tracks.get(*index) == Some(&playing))
            })
            .or_else(|| (!self.order.is_empty()).then_some(0));
    }

    /// Moves the play-order entry at `at` to the front and returns its new
    /// position, which is always the first row.
    fn lead_with(&mut self, at: usize) -> Option<usize> {
        let entry = self.order.remove(at);
        self.order.insert(0, entry);
        Some(0)
    }

    fn rebuild_order(&mut self) {
        self.order = (0..self.tracks.len()).collect();
        if self.shuffle {
            fastrand::shuffle(&mut self.order);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> Queue {
        let mut queue = Queue::new();
        queue.fill(vec![10, 20, 30], 0);
        queue
    }

    #[test]
    fn plays_in_order_and_stops_at_the_end() {
        let mut queue = filled();
        assert_eq!(queue.current(), Some(10));
        assert_eq!(queue.advance(Advance::Ended), Some(20));
        assert_eq!(queue.advance(Advance::Ended), Some(30));
        assert_eq!(queue.advance(Advance::Ended), None);
    }

    #[test]
    fn fill_starts_where_the_user_clicked() {
        let mut queue = Queue::new();
        queue.fill(vec![10, 20, 30], 2);
        assert_eq!(queue.current(), Some(30));
    }

    #[test]
    fn repeat_all_wraps_but_repeat_off_does_not() {
        let mut queue = filled();
        queue.jump_to(2);
        queue.set_repeat(Repeat::All);
        assert_eq!(queue.advance(Advance::Ended), Some(10));

        queue.set_repeat(Repeat::Off);
        queue.jump_to(2);
        assert_eq!(queue.advance(Advance::Ended), None);
    }

    #[test]
    fn repeat_one_holds_on_end_but_not_on_a_user_next() {
        let mut queue = filled();
        queue.set_repeat(Repeat::One);
        assert_eq!(queue.advance(Advance::Ended), Some(10));
        assert_eq!(queue.advance(Advance::User), Some(20));
    }

    #[test]
    fn peek_next_matches_what_advance_will_do() {
        for repeat in [Repeat::Off, Repeat::All, Repeat::One] {
            for start in 0..3 {
                let mut queue = filled();
                queue.set_repeat(repeat);
                queue.jump_to(start);
                let peeked = queue.peek_next();
                assert_eq!(
                    peeked,
                    queue.advance(Advance::Ended),
                    "repeat {repeat:?} at {start}"
                );
            }
        }
    }

    #[test]
    fn previous_wraps_from_the_first_track() {
        let mut queue = filled();
        assert_eq!(queue.previous(), Some(30));
        assert_eq!(queue.previous(), Some(20));
    }

    #[test]
    fn play_next_jumps_the_line_without_touching_the_rest() {
        let mut queue = filled();
        queue.play_next(&[99]);
        assert_eq!(queue.in_play_order(), vec![10, 99, 20, 30]);
        assert_eq!(queue.current(), Some(10));
        assert_eq!(queue.advance(Advance::Ended), Some(99));
        assert_eq!(queue.advance(Advance::Ended), Some(20));
    }

    #[test]
    fn play_next_keeps_the_order_of_several_tracks() {
        let mut queue = filled();
        queue.play_next(&[97, 98, 99]);
        assert_eq!(queue.in_play_order(), vec![10, 97, 98, 99, 20, 30]);
    }

    #[test]
    fn append_goes_to_the_end() {
        let mut queue = filled();
        queue.append(&[99]);
        assert_eq!(queue.in_play_order(), vec![10, 20, 30, 99]);
    }

    #[test]
    fn removing_keeps_the_cursor_on_the_playing_track() {
        let mut queue = filled();
        queue.jump_to(2);
        assert_eq!(queue.current(), Some(30));
        queue.remove(0);
        assert_eq!(queue.in_play_order(), vec![20, 30]);
        assert_eq!(queue.current(), Some(30));
    }

    #[test]
    fn removing_the_playing_track_lands_on_its_replacement() {
        let mut queue = filled();
        queue.jump_to(1);
        queue.remove(1);
        assert_eq!(queue.in_play_order(), vec![10, 30]);
        assert_eq!(queue.current(), Some(30));
    }

    #[test]
    fn removing_the_last_track_empties_the_queue() {
        let mut queue = Queue::new();
        queue.fill(vec![10], 0);
        queue.remove(0);
        assert!(queue.is_empty());
        assert_eq!(queue.current(), None);
        assert_eq!(queue.advance(Advance::Ended), None);
    }

    #[test]
    fn reordering_follows_the_playing_track() {
        let mut queue = filled();
        queue.jump_to(0);
        queue.move_entry(0, 2);
        assert_eq!(queue.in_play_order(), vec![20, 30, 10]);
        assert_eq!(queue.current(), Some(10));
    }

    #[test]
    fn shuffle_keeps_the_current_track_and_every_other_one() {
        let mut queue = Queue::new();
        queue.fill((1..=200).collect(), 0);
        queue.jump_to(7);
        let playing = queue.current().unwrap();

        queue.set_shuffle(true);
        assert_eq!(queue.current(), Some(playing));
        let mut shuffled = queue.in_play_order();
        assert_eq!(shuffled.len(), 200);
        shuffled.sort_unstable();
        assert_eq!(shuffled, (1..=200).collect::<Vec<_>>());

        queue.set_shuffle(false);
        assert_eq!(queue.current(), Some(playing));
        assert_eq!(queue.in_play_order(), (1..=200).collect::<Vec<_>>());
    }

    #[test]
    fn shuffling_puts_the_playing_track_at_the_top_of_the_queue() {
        let mut queue = Queue::new();
        queue.fill((1..=200).collect(), 0);
        queue.jump_to(7);
        let playing = queue.current().unwrap();

        queue.set_shuffle(true);
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(queue.in_play_order().first(), Some(&playing));
        assert_eq!(queue.in_play_order().len(), 200);

        // And starting a shuffled queue from a click leads with what was clicked.
        let mut queue = Queue::new();
        queue.set_shuffle(true);
        queue.fill((1..=200).collect(), 42);
        assert_eq!(queue.current(), Some(43));
        assert_eq!(queue.current_index(), Some(0));
    }

    #[test]
    fn shuffle_all_leads_with_the_shuffle_not_with_track_one() {
        // Over many deals the head has to move around; a forced lead would pin
        // it to the first track of the library every time.
        let mut heads = std::collections::HashSet::new();
        for _ in 0..50 {
            let mut queue = Queue::new();
            queue.fill_shuffled((1..=200).collect());
            assert_eq!(queue.current_index(), Some(0));
            assert_eq!(queue.in_play_order().len(), 200);
            heads.insert(queue.current().unwrap());
        }
        assert!(heads.len() > 1, "shuffle all always started on {heads:?}");
    }

    #[test]
    fn a_missing_track_is_dropped_without_losing_the_others() {
        let mut queue = filled();
        queue.jump_to(1);
        queue.retain_known(&|id| id != 10);
        assert_eq!(queue.in_play_order(), vec![20, 30]);
        assert_eq!(queue.current(), Some(20));
    }

    #[test]
    fn an_empty_queue_answers_every_question_without_panicking() {
        let mut queue = Queue::new();
        assert_eq!(queue.current(), None);
        assert_eq!(queue.advance(Advance::User), None);
        assert_eq!(queue.previous(), None);
        assert_eq!(queue.peek_next(), None);
        assert_eq!(queue.jump_to(3), None);
        queue.remove(0);
        queue.move_entry(0, 1);
        queue.set_shuffle(true);
        assert!(queue.is_empty());
    }
}
