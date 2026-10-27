// SPDX-License-Identifier: GPL-3.0-only

//! Cue writes from the decks, persisted beside the medium they name.
//!
//! A deck writes a cue straight into the source (`0x2705`, F65): third
//! argument `1` sets a hot cue, `0` removes it, and the deck re-asserts each
//! state about every 15 s while the track stays loaded — the writes are
//! idempotent, so a repeat costs nothing. The writes arrive while the deck
//! is *playing* our tree, so they cannot be folded back into the cratify
//! export that tree was generated from; instead they live in a sidecar
//! beside it, `.prolink-cues`, and the cue replies overlay them onto the
//! analysis records. A cratify re-export leaves the sidecar untouched.
//!
//! The sidecar is a line log, oldest first, replayed in order:
//!
//! ```text
//! S <track> <hot cue> <the record bytes, hex>
//! R <track> <hot cue> <position in ms>
//! ```
//!
//! The position on an `R` line is what a memory remove needs — its identity
//! is the position, and the line is all a replay sees. Older sidecars without
//! it replay with the memory clear simply unrepresented; the deck re-asserts
//! nothing for a cue it dropped, so nothing resurfaces that matters.
//!
//! Replay rebuilds the overlay exactly, later lines winning; a bad line is
//! skipped rather than fatal, because the deck will simply rewrite what it
//! still holds.
//!
//! Hot cues and memory cues part ways in the model (F66): a hot cue is
//! identified by its slot number and the deck holds eight; a memory cue
//! travels with number `0` and the deck holds **one** working placement that
//! follows the button — pressing it again *moves* the cue, and the wire
//! shows the move as a bare second set with no remove in between (both live
//! sessions did exactly this). So a memory set replaces the stored one, and
//! a memory remove clears both the stored one and the base list's entry at
//! that position.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use prolink_proto::dbserver::CueWriteRecord;

/// The sidecar file's name, relative to a medium's root.
const SIDECAR: &str = ".prolink-cues";

/// What the decks have written for one track.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TrackOverlay {
    /// Hot-cue number → the record as written, verbatim.
    pub upserts: BTreeMap<u16, CueWriteRecord>,
    /// Hot-cue numbers a deck removed.
    pub removed: BTreeSet<u16>,
    /// The deck's one working memory cue, as last set: a later set moves it,
    /// since the wire carries no identity beyond the position (F66).
    pub memory: Option<CueWriteRecord>,
    /// Memory-cue positions a deck cleared, in milliseconds.
    pub memory_cleared: BTreeSet<u32>,
}

/// Every track's overlay for one medium, loaded lazily and written through.
#[derive(Debug, Default)]
pub struct CueWrites {
    media: Mutex<HashMap<PathBuf, MediaOverlay>>,
}

#[derive(Debug, Default)]
struct MediaOverlay {
    loaded: bool,
    tracks: HashMap<u32, TrackOverlay>,
}

impl CueWrites {
    /// An empty store. Each medium's overlay is loaded from its sidecar the
    /// first time it is needed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one deck write. `set` is the request's third argument as a
    /// boolean: `true` sets the cue, `false` removes it.
    pub fn apply(&self, root: Option<&Path>, track: u32, set: bool, record: &CueWriteRecord) {
        let Some(root) = root else {
            return;
        };
        let hot_cue = record.hot_cue();
        self.with_track_overlay(root, track, |overlay| {
            if hot_cue == 0 {
                // A memory cue: the deck's one working placement, moved by
                // the next set, cleared by a remove (F66).
                if set {
                    overlay.memory = Some(record.clone());
                    overlay.memory_cleared.remove(&record.time_ms());
                } else {
                    overlay.memory = None;
                    overlay.memory_cleared.insert(record.time_ms());
                }
            } else if set {
                overlay.upserts.insert(hot_cue, record.clone());
                overlay.removed.remove(&hot_cue);
            } else {
                overlay.upserts.remove(&hot_cue);
                overlay.removed.insert(hot_cue);
            }
        });
        append_line(root, track, set, hot_cue, record);
    }

    /// The overlay for one track: what to replace, what to drop.
    pub fn overlay_for(&self, root: Option<&Path>, track: u32) -> TrackOverlay {
        let Some(root) = root else {
            return TrackOverlay::default();
        };
        self.with_track_overlay(root, track, |overlay| overlay.clone())
    }

    /// The overlay for one track of one medium, loading the sidecar on
    /// first touch. The guard is the store's, so mutations persist.
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
        let overlay = media.entry(root.to_path_buf()).or_default();
        if !overlay.loaded {
            overlay.tracks = load(&root.join(SIDECAR));
            overlay.loaded = true;
        }
        apply(overlay.tracks.entry(track).or_default())
    }
}

/// Append one write to the sidecar, write-and-rename so a crash mid-write
/// cannot leave a half line behind a reader would trust.
fn append_line(root: &Path, track: u32, set: bool, hot_cue: u16, record: &CueWriteRecord) {
    let path = root.join(SIDECAR);
    // The record travels in the line, so a replay rebuilds it whole.
    let line = if set {
        format!("S {track} {hot_cue} {}\n", hex_encode(record.raw()))
    } else {
        format!("R {track} {hot_cue} {}\n", record.time_ms())
    };
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    existing.push_str(&line);
    let tmp = path.with_extension("tmp");
    if let Err(error) = std::fs::write(&tmp, existing).and_then(|()| std::fs::rename(&tmp, &path)) {
        tracing::warn!(%error, "the cue sidecar could not be renamed into place");
    }
}

/// Replay a sidecar. Later lines win; a bad line is skipped.
fn load(path: &Path) -> HashMap<u32, TrackOverlay> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut tracks: HashMap<u32, TrackOverlay> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split(' ');
        let op = fields.next().unwrap_or("");
        let track: u32 = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
        let hot_cue: u16 = fields.next().and_then(|f| f.parse().ok()).unwrap_or(0);
        let overlay = tracks.entry(track).or_default();
        match (op, fields.next()) {
            ("S", Some(hex)) => {
                if let Some(bytes) = hex_decode(hex)
                    && let Ok(record) = CueWriteRecord::parse(&bytes)
                {
                    if hot_cue == 0 {
                        overlay.memory = Some(record);
                        if let Some(record) = &overlay.memory {
                            overlay.memory_cleared.remove(&record.time_ms());
                        }
                    } else {
                        overlay.upserts.insert(hot_cue, record);
                        overlay.removed.remove(&hot_cue);
                    }
                }
            }
            ("R", position) => {
                if hot_cue == 0 {
                    overlay.memory = None;
                    if let Some(ms) = position.and_then(|f| f.parse().ok()) {
                        overlay.memory_cleared.insert(ms);
                    }
                } else {
                    overlay.upserts.remove(&hot_cue);
                    overlay.removed.insert(hot_cue);
                }
            }
            _ => {}
        }
    }
    tracks
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len().div_ceil(2));
    let mut chars = text.chars();
    loop {
        match (chars.next(), chars.next()) {
            (Some(hi), Some(lo)) => {
                let value = hi
                    .to_digit(16)?
                    .checked_mul(16)?
                    .checked_add(lo.to_digit(16)?)?;
                out.push(u8::try_from(value).ok()?);
            }
            (None, None) => return Some(out),
            (_, Some(_)) | (Some(_), None) => return None,
        }
    }
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let value = u32::from(*byte);
        out.push(char::from_digit(value / 16, 16).unwrap_or('0'));
        out.push(char::from_digit(value % 16, 16).unwrap_or('0'));
    }
    out
}
