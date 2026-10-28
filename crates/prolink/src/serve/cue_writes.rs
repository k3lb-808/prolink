// SPDX-License-Identifier: GPL-3.0-only

//! Cue writes from the decks, held in memory until the splice lands them.
//!
//! A deck writes a cue straight into the source (`0x2705`, F65): third
//! argument `1` sets a hot cue, `0` removes it, and the deck re-asserts each
//! state about every 15 s while the track stays loaded — the writes are
//! idempotent, so a repeat costs nothing. The replies overlay the writes
//! onto the analysis records at reply time ([`crate::serve::dbserver`]), and
//! the splice ([`crate::serve::splice`]) lands the same state into the
//! served tree's own ANLZ files.
//!
//! The overlay is deliberately **memory only**. The spliced files are the
//! persistence: once a track's splice has run, its cue state is in the
//! tree's own analysis files, where a restart parses it straight back. What
//! is lost when the process dies before the splice is exactly what the deck
//! still holds and will re-assert — the deck repeats every state it carries
//! about every 15 s, so anything lost mid-set is captured again on the next
//! pass. There is no sidecar to replay: a `.prolink-cues` file an older
//! prolink left behind is simply ignored.
//!
//! Hot cues and memory cues part ways in the model (F66): a hot cue is
//! identified by its slot number and the deck holds eight; a memory cue
//! travels with number `0` and its identity is the position — each set at a
//! fresh position adds another cue (rekordbox's own served list kept every
//! added one; the deck's single-remaining-write look of the bare-ack
//! sessions was its unconfirmed-write loop, not a move), and a remove
//! clears exactly the position it names. A memory set also un-clears its
//! position: the deck re-set a cue the same DJ had just deleted is the
//! deck's truth.
//!
//! The store also tracks what is **dirty**: which medium's tracks changed
//! and when last. Re-assertions of an unchanged state mark nothing — the
//! splice trigger reads this, and a quiesce without a change costs nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use prolink_proto::dbserver::CueWriteRecord;

/// What the decks have written for one track.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TrackOverlay {
    /// Hot-cue number → the record as written, verbatim.
    pub upserts: BTreeMap<u16, CueWriteRecord>,
    /// Hot-cue numbers a deck removed.
    pub removed: BTreeSet<u16>,
    /// Memory cues the decks added, keyed by position: each set at a fresh
    /// position is another cue (F66).
    pub memories: BTreeMap<u32, CueWriteRecord>,
    /// Memory-cue positions a deck cleared, in milliseconds.
    pub memory_cleared: BTreeSet<u32>,
}

/// Every track's overlay for one medium, and what in it has changed.
#[derive(Debug, Default)]
pub struct CueWrites {
    media: Mutex<HashMap<PathBuf, HashMap<u32, TrackOverlay>>>,
    dirty: Mutex<BTreeMap<PathBuf, Dirty>>,
}

/// What has changed on one medium, and when it last did.
#[derive(Debug, Default)]
struct Dirty {
    tracks: BTreeSet<u32>,
    last_write: Option<Instant>,
}

impl CueWrites {
    /// An empty store. Each medium's overlay builds up from the deck's
    /// writes; its persistence is the splice, not anything kept here.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one deck write. `set` is the request's third argument as a
    /// boolean: `true` sets the cue, `false` removes it.
    ///
    /// Returns whether the track's state actually changed: a re-assertion of
    /// a state already held changes nothing, marks nothing dirty, and costs
    /// the splice nothing.
    pub fn apply(
        &self,
        root: Option<&Path>,
        track: u32,
        set: bool,
        record: &CueWriteRecord,
    ) -> bool {
        let Some(root) = root else {
            return false;
        };
        let hot_cue = record.hot_cue();
        let changed = self.with_track_overlay(root, track, |overlay| {
            let before = overlay.clone();
            if hot_cue == 0 {
                // A memory cue: position-keyed, each fresh set another cue
                // and a remove clearing its own position (F66).
                if set {
                    overlay.memories.insert(record.time_ms(), record.clone());
                    overlay.memory_cleared.remove(&record.time_ms());
                } else {
                    overlay.memories.remove(&record.time_ms());
                    overlay.memory_cleared.insert(record.time_ms());
                }
            } else if set {
                overlay.upserts.insert(hot_cue, record.clone());
                overlay.removed.remove(&hot_cue);
            } else {
                overlay.upserts.remove(&hot_cue);
                overlay.removed.insert(hot_cue);
            }
            before != *overlay
        });
        if changed {
            let mut dirty = self
                .dirty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = dirty.entry(root.to_path_buf()).or_default();
            entry.tracks.insert(track);
            entry.last_write = Some(Instant::now());
        }
        changed
    }

    /// The overlay for one track: what to replace, what to drop.
    pub fn overlay_for(&self, root: Option<&Path>, track: u32) -> TrackOverlay {
        let Some(root) = root else {
            return TrackOverlay::default();
        };
        self.with_track_overlay(root, track, |overlay| overlay.clone())
    }

    /// The overlay for one track of one medium. The guard is the store's, so
    /// mutations persist.
    fn with_track_overlay<R>(
        &self,
        root: &Path,
        track: u32,
        apply: impl FnOnce(&mut TrackOverlay) -> R,
    ) -> R {
        let mut media = self
            .media
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        apply(
            media
                .entry(root.to_path_buf())
                .or_default()
                .entry(track)
                .or_default(),
        )
    }

    /// Take the overlays of every medium that has been quiet for `after`
    /// since its last write.
    ///
    /// Taking is draining: a splice that fails loses its dirty mark, and the
    /// deck's next re-assertion — the writes are idempotent and repeated
    /// about every 15 s — makes the track dirty again. A quiet medium with
    /// nothing taken costs this call nothing.
    pub fn take_quiesced(&self, after: Duration) -> Vec<(PathBuf, BTreeSet<u32>)> {
        self.take(|dirty| dirty.last_write.is_some_and(|at| at.elapsed() >= after))
    }

    /// Take everything dirty, quiesced or not: the shutdown splice.
    pub fn take_all(&self) -> Vec<(PathBuf, BTreeSet<u32>)> {
        self.take(|_| true)
    }

    /// Drain dirty media the predicate admits, newest write first.
    fn take(&self, admit: impl Fn(&Dirty) -> bool) -> Vec<(PathBuf, BTreeSet<u32>)> {
        let mut dirty = self
            .dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let taken = dirty
            .iter()
            .filter(|(_, entry)| admit(entry))
            .map(|(root, entry)| (root.clone(), entry.tracks.clone()))
            .collect::<Vec<_>>();
        for (root, _) in &taken {
            dirty.remove(root);
        }
        taken
    }
}
