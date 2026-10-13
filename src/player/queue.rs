use crate::api::model::Track;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Off => "↻",
            Self::All => "↻",
            Self::One => "↻1",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Queue {
    tracks: Vec<Track>,
    /// Play order indices (for shuffle). When not shuffled, identity.
    order: Vec<usize>,
    /// Position in `order`
    cursor: usize,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Original order before shuffle (track indices into `tracks`)
    unshuffled: Vec<usize>,
}

impl Queue {
    pub fn from_session(tracks: Vec<Track>, index: usize, shuffle: bool, repeat: RepeatMode) -> Self {
        let n = tracks.len();
        let mut q = Self {
            tracks,
            order: (0..n).collect(),
            cursor: index.min(n.saturating_sub(1)),
            shuffle: false,
            repeat,
            unshuffled: (0..n).collect(),
        };
        if shuffle && n > 0 {
            q.set_shuffle(true);
            // Try to keep current track at cursor
            if let Some(pos) = q.order.iter().position(|&i| i == index.min(n - 1)) {
                q.cursor = pos;
            }
        }
        q
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn tracks_in_play_order(&self) -> Vec<&Track> {
        self.order.iter().map(|&i| &self.tracks[i]).collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn current(&self) -> Option<&Track> {
        self.order.get(self.cursor).map(|&i| &self.tracks[i])
    }

    pub fn current_index_in_tracks(&self) -> Option<usize> {
        self.order.get(self.cursor).copied()
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.order.clear();
        self.unshuffled.clear();
        self.cursor = 0;
    }

    pub fn clear_upcoming(&mut self) {
        if self.is_empty() {
            return;
        }
        let keep = self.current().cloned();
        self.tracks.clear();
        self.order.clear();
        self.unshuffled.clear();
        self.cursor = 0;
        if let Some(t) = keep {
            self.tracks.push(t);
            self.order.push(0);
            self.unshuffled.push(0);
        }
    }

    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) {
        let n = tracks.len();
        self.tracks = tracks;
        self.order = (0..n).collect();
        self.unshuffled = (0..n).collect();
        self.cursor = start.min(n.saturating_sub(1));
        if self.shuffle && n > 1 {
            self.reshuffle_keeping_current();
        }
    }

    pub fn play_track_now(&mut self, track: Track) {
        // Insert after current and jump to it, or replace empty
        if self.is_empty() {
            self.replace(vec![track], 0);
            return;
        }
        let insert_at = self.current_index_in_tracks().unwrap_or(0) + 1;
        self.tracks.insert(insert_at, track);
        self.rebuild_order_after_insert(insert_at);
        // Move cursor to the new track
        if let Some(pos) = self.order.iter().position(|&i| i == insert_at) {
            self.cursor = pos;
        }
    }

    pub fn add(&mut self, track: Track) {
        let idx = self.tracks.len();
        self.tracks.push(track);
        self.order.push(idx);
        self.unshuffled.push(idx);
    }

    pub fn add_many(&mut self, tracks: Vec<Track>) {
        for t in tracks {
            self.add(t);
        }
    }

    pub fn play_next(&mut self, track: Track) {
        if self.is_empty() {
            self.replace(vec![track], 0);
            return;
        }
        let insert_at = self.current_index_in_tracks().unwrap_or(0) + 1;
        self.tracks.insert(insert_at, track);
        self.rebuild_order_after_insert(insert_at);
        // Insert into play order right after cursor
        let order_insert = self.cursor + 1;
        // Fix indices in order that were >= insert_at
        for i in self.order.iter_mut() {
            if *i >= insert_at {
                // already adjusted in rebuild — skip alternate path
            }
        }
        // Simpler: rebuild order from unshuffled/shuffle state
        let _ = order_insert;
        if self.shuffle {
            self.order.insert(self.cursor + 1, insert_at);
        }
    }

    fn rebuild_order_after_insert(&mut self, inserted: usize) {
        // Bump indices >= inserted
        for i in self.order.iter_mut() {
            if *i >= inserted {
                *i += 1;
            }
        }
        for i in self.unshuffled.iter_mut() {
            if *i >= inserted {
                *i += 1;
            }
        }
        if !self.shuffle {
            self.order = (0..self.tracks.len()).collect();
            self.unshuffled = self.order.clone();
        } else {
            // ensure inserted index exists somewhere; play_next inserts explicitly
            if !self.order.contains(&inserted) {
                self.order.insert(self.cursor + 1, inserted);
            }
            if !self.unshuffled.contains(&inserted) {
                self.unshuffled.push(inserted);
            }
        }
    }

    pub fn remove_at_play_order(&mut self, play_idx: usize) -> Option<Track> {
        if play_idx >= self.order.len() {
            return None;
        }
        let track_idx = self.order.remove(play_idx);
        self.unshuffled.retain(|&i| i != track_idx);
        for i in self.order.iter_mut() {
            if *i > track_idx {
                *i -= 1;
            }
        }
        for i in self.unshuffled.iter_mut() {
            if *i > track_idx {
                *i -= 1;
            }
        }
        let removed = self.tracks.remove(track_idx);
        if self.cursor >= self.order.len() && !self.order.is_empty() {
            self.cursor = self.order.len() - 1;
        } else if play_idx < self.cursor {
            self.cursor = self.cursor.saturating_sub(1);
        }
        Some(removed)
    }

    pub fn move_play_order(&mut self, from: usize, to: usize) {
        if from >= self.order.len() || to >= self.order.len() || from == to {
            return;
        }
        let item = self.order.remove(from);
        self.order.insert(to, item);
        if self.cursor == from {
            self.cursor = to;
        } else if from < self.cursor && to >= self.cursor {
            self.cursor -= 1;
        } else if from > self.cursor && to <= self.cursor {
            self.cursor += 1;
        }
    }

    pub fn jump_to_play_order(&mut self, play_idx: usize) {
        if play_idx < self.order.len() {
            self.cursor = play_idx;
        }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        if on {
            self.unshuffled = self.order.clone();
            self.reshuffle_keeping_current();
        } else {
            let current_track = self.current_index_in_tracks();
            self.order = self.unshuffled.clone();
            if self.order.is_empty() {
                self.order = (0..self.tracks.len()).collect();
            }
            if let Some(ti) = current_track {
                if let Some(pos) = self.order.iter().position(|&i| i == ti) {
                    self.cursor = pos;
                }
            }
        }
    }

    fn reshuffle_keeping_current(&mut self) {
        let current = self.current_index_in_tracks();
        let mut rest: Vec<usize> = (0..self.tracks.len()).collect();
        if let Some(c) = current {
            rest.retain(|&i| i != c);
        }
        rest.shuffle(&mut rand::thread_rng());
        let mut order = Vec::with_capacity(self.tracks.len());
        if let Some(c) = current {
            order.push(c);
            self.cursor = 0;
        }
        order.extend(rest);
        self.order = order;
    }

    pub fn toggle_shuffle(&mut self) {
        self.set_shuffle(!self.shuffle);
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.next();
    }

    /// Advance after a track ends. Returns the next track to play, if any.
    pub fn advance_on_end(&mut self) -> Option<&Track> {
        if self.is_empty() {
            return None;
        }
        match self.repeat {
            RepeatMode::One => self.current(),
            RepeatMode::All => {
                self.cursor = (self.cursor + 1) % self.order.len();
                self.current()
            }
            RepeatMode::Off => {
                if self.cursor + 1 < self.order.len() {
                    self.cursor += 1;
                    self.current()
                } else {
                    None
                }
            }
        }
    }

    pub fn next(&mut self) -> Option<&Track> {
        if self.is_empty() {
            return None;
        }
        if self.cursor + 1 < self.order.len() {
            self.cursor += 1;
            return self.current();
        }
        if self.repeat == RepeatMode::All {
            self.cursor = 0;
            return self.current();
        }
        None
    }

    pub fn previous(&mut self, position_secs: f64) -> PreviousResult {
        if self.is_empty() {
            return PreviousResult::None;
        }
        if position_secs > 3.0 {
            return PreviousResult::Restart;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
            return PreviousResult::Track;
        }
        if self.repeat == RepeatMode::All {
            self.cursor = self.order.len() - 1;
            return PreviousResult::Track;
        }
        PreviousResult::Restart
    }

    pub fn remaining_count(&self) -> usize {
        self.order.len().saturating_sub(self.cursor + 1)
    }

    pub fn append_radio(&mut self, tracks: Vec<Track>) {
        let existing: std::collections::HashSet<String> =
            self.tracks.iter().map(|t| t.video_id.clone()).collect();
        for mut t in tracks {
            if existing.contains(&t.video_id) {
                continue;
            }
            t.from_radio = true;
            self.add(t);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviousResult {
    None,
    Restart,
    Track,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: vec![],
            album: None,
            duration: Some(Duration::from_secs(180)),
            thumbnails: vec![],
            explicit: false,
            liked: None,
            from_radio: false,
        }
    }

    #[test]
    fn previous_restarts_after_3s() {
        let mut q = Queue::from_session(vec![track("a"), track("b")], 1, false, RepeatMode::Off);
        assert_eq!(q.previous(4.0), PreviousResult::Restart);
        assert_eq!(q.current().unwrap().video_id, "b");
    }

    #[test]
    fn previous_goes_back() {
        let mut q = Queue::from_session(vec![track("a"), track("b")], 1, false, RepeatMode::Off);
        assert_eq!(q.previous(1.0), PreviousResult::Track);
        assert_eq!(q.current().unwrap().video_id, "a");
    }

    #[test]
    fn shuffle_restore() {
        let mut q = Queue::from_session(
            vec![track("a"), track("b"), track("c")],
            0,
            false,
            RepeatMode::Off,
        );
        q.set_shuffle(true);
        let cur = q.current().unwrap().video_id.clone();
        q.set_shuffle(false);
        assert_eq!(q.current().unwrap().video_id, cur);
        assert_eq!(q.tracks().len(), 3);
    }

    #[test]
    fn radio_dedup() {
        let mut q = Queue::from_session(vec![track("a")], 0, false, RepeatMode::Off);
        q.append_radio(vec![track("a"), track("b")]);
        assert_eq!(q.len(), 2);
        assert!(q.tracks()[1].from_radio);
    }

    #[test]
    fn repeat_cycle() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::All);
        assert_eq!(RepeatMode::All.next(), RepeatMode::One);
        assert_eq!(RepeatMode::One.next(), RepeatMode::Off);
    }
}
