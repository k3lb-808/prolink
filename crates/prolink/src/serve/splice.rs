// SPDX-License-Identifier: GPL-3.0-only

//! Splicing the deck's cue writes into the tree's own ANLZ files.
//!
//! The deck's writes arrive over the wire (`0x2705`) while it plays our tree;
//! a stick pulled after the session must carry the edits itself, the way a
//! stick a deck held directly does (F67: the deck rewrites the track's own
//! `ANLZ0000.DAT` and `.EXT` in place and touches no database). This module
//! performs that same write on the deck's behalf: the overlay's state becomes
//! the files' cue lists, the files become the persistence, and the overlay
//! stays a transient delta over a parse of the — now correct — files.
//!
//! The merge mirrors the deck's own conventions, as observed on hardware:
//!
//! * `.DAT` carries the basic `PCOB` lists, one per flavour.
//! * `.EXT` carries the extended `PCO2` lists, one per flavour, and its own
//!   `PCOB` lists sit **empty** — the deck normalises to that shape on its
//!   first edit, and the splicer does the same whenever it rewrites the file.
//!   Cues the `PCOB` twin carried move into `PCO2` as the compact entries
//!   they can be expressed as.
//! * A fresh entry is the deck's compact shape: no comment, no colour — the
//!   information the write record itself carries, and nothing more.
//! * An entry the overlay replaces but the file already carries keeps its
//!   bytes: only its type, position and loop end are patched in place, so a
//!   rekordbox comment or colour tail survives the deck's re-assertions.
//! * `.2EX`, `PSSI`, waveforms, grids and paths are never touched.
//!
//! Every write is guarded: the file is parsed, only whole files are
//! re-rendered, the render is re-parsed before it lands, and the write
//! itself is temp-and-rename. A file that fails any of it keeps its bytes,
//! and the deck's own re-assertions — about every 15 s — offer the splice
//! another chance.

use std::collections::BTreeMap;
use std::path::Path;

use prolink_proto::dbserver::CueWriteRecord;
use prolink_rekordbox::anlz::CueListType;
use prolink_rekordbox::anlz::write::{self, RawEntry};
use prolink_rekordbox::{AnlzFile, FourCc, Tag};

use crate::serve::Medium;
use crate::serve::cue_writes::TrackOverlay;
use crate::serve::dbserver::analysis::loop_beats;

/// What one track's splice did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SpliceOutcome {
    /// The `.DAT` was rewritten.
    pub dat: bool,
    /// The `.EXT` was rewritten.
    pub ext: bool,
}

impl SpliceOutcome {
    /// Whether any file changed.
    pub(crate) fn changed(&self) -> bool {
        self.dat || self.ext
    }
}

/// Splice one track's overlay into its analysis files.
///
/// Reads both files fresh from disk — never the served parse, which holds the
/// pre-splice truth — and rewrites only the cue tags. The medium's parse
/// cache is invalidated for the track when either file changed, so the next
/// reply is built from the spliced bytes, over which the overlay re-applies
/// harmlessly: the overlay's keys are the same ones the splice merged on.
pub(crate) fn splice_track(
    medium: &Medium,
    track_id: u32,
    overlay: &TrackOverlay,
) -> SpliceOutcome {
    let mut outcome = SpliceOutcome::default();
    if let Some(path) = medium.analysis_dat_path(track_id)
        && splice_file(&path, overlay, File::Dat)
    {
        outcome.dat = true;
    }
    if let Some(path) = medium.analysis_ext_path(track_id)
        && splice_file(&path, overlay, File::Ext)
    {
        outcome.ext = true;
    }
    if outcome.changed() {
        medium.invalidate_analysis(track_id);
    }
    outcome
}

/// Which file a splice serves: the `.DAT`'s basic lists or the `.EXT`'s
/// extended ones.
#[derive(Clone, Copy, PartialEq, Eq)]
enum File {
    /// `ANLZ####.DAT`: `PCOB` lists carry the cues.
    Dat,
    /// `ANLZ####.EXT`: `PCO2` lists carry the cues, `PCOB` sits empty.
    Ext,
}

/// What one file's splice will do: which tags become what, and what gets
/// added where the file carries none.
struct SplicePlan {
    /// Tag index → the tag's new bytes, or `None` to leave it.
    replacements: Vec<Option<Vec<u8>>>,
    /// Fresh cue-list tags, inserted after the last cue-list tag.
    inserted: Vec<Vec<u8>>,
    /// The last cue-list tag's index, where `inserted` goes.
    last_list_slot: Option<usize>,
}

impl SplicePlan {
    /// Whether anything at all is planned.
    fn is_empty(&self) -> bool {
        self.replacements.iter().all(Option::is_none) && self.inserted.is_empty()
    }
}

/// Rewrite one file's cue tags, or leave it alone.
///
/// `true` when the file was rewritten. Every refusal — unreadable, not
/// whole, no cue tags to splice into, a re-render that changes nothing —
/// keeps the file's own bytes.
fn splice_file(path: &Path, overlay: &TrackOverlay, file: File) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        tracing::debug!(file = %path.display(), "an analysis file is unreadable; leaving it");
        return false;
    };
    let Ok(parsed) = AnlzFile::parse(&bytes) else {
        tracing::debug!(file = %path.display(), "an analysis file does not parse; leaving it");
        return false;
    };
    if !parsed.walked_whole(bytes.len()) {
        tracing::warn!(
            file = %path.display(),
            "an analysis file is not whole; leaving it unspliced"
        );
        return false;
    }
    let grid = parsed
        .payload(FourCc::PQTZ)
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    let flavour_base = flavour_bases(&parsed, file);
    let mut plan = SplicePlan {
        replacements: parsed.tags.iter().map(|_| None).collect(),
        inserted: Vec::new(),
        last_list_slot: None,
    };
    plan_tags(
        &parsed,
        overlay,
        file,
        &grid,
        &flavour_base,
        &mut plan,
        path,
    );
    if file == File::Ext {
        plan_missing_lists(&parsed, overlay, &grid, &flavour_base, &mut plan, path);
    }
    if plan.is_empty() {
        return false;
    }
    let rendered = rendered_container(&parsed, &plan);
    if rendered == bytes {
        return false;
    }
    if AnlzFile::parse(&rendered).is_err() {
        tracing::warn!(
            file = %path.display(),
            "a spliced render does not parse; leaving the file"
        );
        return false;
    }
    match write_atomic(path, &rendered) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                file = %path.display(),
                %error,
                "a spliced analysis file could not be written"
            );
            false
        }
    }
}

/// Where each flavour's entries live in one file.
///
/// Only an `.EXT` consults this: its `PCO2` list that holds entries outranks
/// the `PCOB` twin, an empty `PCO2` never hiding what the `PCOB` carries —
/// the order a reader resolves them in. A `.DAT` merges per tag and needs
/// no inventory.
fn flavour_bases(parsed: &AnlzFile, file: File) -> BTreeMap<u32, (FourCc, Vec<RawEntry>)> {
    let mut bases = BTreeMap::new();
    if file == File::Dat {
        return bases;
    }
    for tag in &parsed.tags {
        let Some(flavour) = write::list_flavour(tag) else {
            continue;
        };
        let (kind, entries) = match tag.fourcc {
            f if f == FourCc::PCOB => (FourCc::PCOB, write::parse_pcob_entries(tag.payload())),
            f if f == FourCc::PCO2 => (FourCc::PCO2, write::parse_pco2_entries(tag.payload())),
            _ => continue,
        };
        if entries.is_empty() {
            continue;
        }
        match kind {
            // An extended list outranks anything read before it.
            f if f == FourCc::PCO2 => {
                bases.insert(flavour.0, (kind, entries));
            }
            // A basic list only fills a flavour nothing else carried.
            _ => {
                bases.entry(flavour.0).or_insert((kind, entries));
            }
        }
    }
    bases
}

/// Plan each cue-list tag's replacement.
///
/// The `.DAT`'s `PCOB` lists each merge the overlay into their own entries.
/// The `.EXT`'s `PCOB` lists empty out — the deck's own normalisation — and
/// its `PCO2` lists carry the merged cues, whatever tag the flavour's
/// entries were read from.
fn plan_tags(
    parsed: &AnlzFile,
    overlay: &TrackOverlay,
    file: File,
    grid: &[u8],
    flavour_base: &BTreeMap<u32, (FourCc, Vec<RawEntry>)>,
    plan: &mut SplicePlan,
    path: &Path,
) {
    for (index, tag) in parsed.tags.iter().enumerate() {
        if tag.fourcc == FourCc::PCOB || tag.fourcc == FourCc::PCO2 {
            plan.last_list_slot = Some(index);
        }
        let Some(flavour) = write::list_flavour(tag) else {
            continue;
        };
        let (source, target, base) = match (file, tag.fourcc) {
            (File::Dat, FourCc::PCOB) => (
                FourCc::PCOB,
                FourCc::PCOB,
                write::parse_pcob_entries(tag.payload()),
            ),
            (File::Ext, FourCc::PCOB) => {
                if let Some(slot) = plan.replacements.get_mut(index) {
                    *slot = rebuild_tag(tag, FourCc::PCOB, &[]);
                }
                continue;
            }
            (File::Ext, FourCc::PCO2) => {
                let (source, base) = flavour_base
                    .get(&flavour.0)
                    .cloned()
                    .unwrap_or((FourCc::PCO2, Vec::new()));
                (source, FourCc::PCO2, base)
            }
            // A .DAT carries no PCO2, and an .EXT's splice adds none here.
            _ => continue,
        };
        let merged = merged_entries(&base, overlay, source, target, flavour, grid)
            .into_iter()
            .map(|(_, bytes)| bytes)
            .collect::<Vec<_>>();
        if u16::try_from(merged.len()).is_ok() {
            if let Some(slot) = plan.replacements.get_mut(index) {
                *slot = rebuild_tag(tag, target, &merged);
            }
        } else {
            tracing::warn!(
                file = %path.display(),
                entries = merged.len(),
                "a merged cue list outgrows a cue tag; leaving the tag"
            );
        }
    }
}

/// Plan fresh `PCO2` tags for an `.EXT` that names no tag of a flavour whose
/// merged list has cues for it.
///
/// The deck's own files carry both flavours' `PCO2` lists always, so this is
/// the ancient-export case: a stick whose analysis predates the extended
/// lists. The fresh tags go where the deck's own put theirs, after the last
/// cue-list tag.
fn plan_missing_lists(
    parsed: &AnlzFile,
    overlay: &TrackOverlay,
    grid: &[u8],
    flavour_base: &BTreeMap<u32, (FourCc, Vec<RawEntry>)>,
    plan: &mut SplicePlan,
    path: &Path,
) {
    for flavour in [CueListType::MEMORY, CueListType::HOT] {
        let Some((source, base)) = flavour_base.get(&flavour.0) else {
            continue;
        };
        let carried = parsed
            .tags
            .iter()
            .any(|tag| tag.fourcc == FourCc::PCO2 && write::list_flavour(tag) == Some(flavour));
        if carried {
            continue;
        }
        let merged = merged_entries(base, overlay, *source, FourCc::PCO2, flavour, grid)
            .into_iter()
            .map(|(_, bytes)| bytes)
            .collect::<Vec<_>>();
        let Ok(count) = u16::try_from(merged.len()) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        let Some(header) = write::fresh_list_header(FourCc::PCO2, flavour, count) else {
            continue;
        };
        match list_tag_bytes(FourCc::PCO2, &header, &merged) {
            Ok(tag) => plan.inserted.push(tag),
            Err(error) => tracing::warn!(
                file = %path.display(),
                %error,
                "a fresh PCO2 tag could not be built"
            ),
        }
    }
}

/// Build a cue-list tag's bytes from a list header and entry bytes.
fn list_tag_bytes(
    kind: FourCc,
    header: &[u8],
    entries: &[Vec<u8>],
) -> prolink_rekordbox::Result<Vec<u8>> {
    let mut span = Vec::with_capacity(entries.iter().map(Vec::len).sum());
    for entry in entries {
        span.extend_from_slice(entry);
    }
    match kind {
        f if f == FourCc::PCOB => write::pcob_tag(header, &span),
        _ => write::pco2_tag(header, &span),
    }
}

/// One cue-list tag rebuilt from its header and a new entries span, or
/// `None` when the tag's header does not name a count.
fn rebuild_tag(tag: &Tag, kind: FourCc, entries: &[Vec<u8>]) -> Option<Vec<u8>> {
    let count = u16::try_from(entries.len()).ok()?;
    let header = write::with_entry_count(kind, tag.header_extra(), count)?;
    list_tag_bytes(kind, &header, entries).ok()
}

/// The container rendered: the parse's own header, each tag replaced or
/// itself, fresh tags after the last cue-list tag, and the declared file
/// length recomputed.
fn rendered_container(parsed: &AnlzFile, plan: &SplicePlan) -> Vec<u8> {
    let added: usize = plan.inserted.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(
        usize::try_from(parsed.file_len)
            .unwrap_or(0)
            .saturating_add(added),
    );
    out.extend_from_slice(b"PMAI");
    out.extend_from_slice(&parsed.header_len.to_be_bytes());
    let file_len_at = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&parsed.header_extra);
    for (index, tag) in parsed.tags.iter().enumerate() {
        if let Some(Some(raw)) = plan.replacements.get(index) {
            out.extend_from_slice(raw);
        } else {
            out.extend_from_slice(&tag.raw);
        }
        if Some(index) == plan.last_list_slot {
            for tag in &plan.inserted {
                out.extend_from_slice(tag);
            }
        }
    }
    let length = u32::try_from(out.len()).unwrap_or(u32::MAX);
    if let Some(slot) = out.get_mut(file_len_at..file_len_at + 4) {
        slot.copy_from_slice(&length.to_be_bytes());
    }
    out
}

/// The merged entries for one flavour: the base's own entries, the overlay's
/// replacements and removals, and the deck's fresh entries minted.
///
/// Entries ride out as the bytes they will land in the file as. A base entry
/// the overlay does not name keeps its own bytes when the base already has
/// the shape it will land in; entries migrating from a `PCOB` twin into a
/// `PCO2` list re-encode, for there is nothing in a basic entry to lose.
///
/// The pairs ride a sort key — a hot list's slot, a memory list's position —
/// so the merged list comes out in the order the deck's own files carry.
fn merged_entries(
    base: &[RawEntry],
    overlay: &TrackOverlay,
    source: FourCc,
    target: FourCc,
    flavour: CueListType,
    grid: &[u8],
) -> Vec<(u32, Vec<u8>)> {
    let mut out: Vec<(u32, Vec<u8>)> = Vec::with_capacity(base.len());
    let mut base_slots: Vec<u32> = Vec::new();
    let mut base_positions: Vec<u32> = Vec::new();
    for entry in base {
        if entry.hot_cue != 0 {
            let Ok(slot) = u16::try_from(entry.hot_cue) else {
                out.push((entry.hot_cue, entry.bytes.clone()));
                continue;
            };
            base_slots.push(entry.hot_cue);
            if overlay.removed.contains(&slot) {
                continue;
            }
            if let Some(record) = overlay.upserts.get(&slot) {
                let values = record_values(record);
                out.push((
                    entry.hot_cue,
                    entry_for(
                        (source == target).then_some(entry.bytes.as_slice()),
                        values,
                        target,
                        grid,
                    ),
                ));
                continue;
            }
            out.push((entry.hot_cue, kept(entry, source, target)));
        } else {
            base_positions.push(entry.time);
            if overlay.memory_cleared.contains(&entry.time) {
                continue;
            }
            if let Some(record) = overlay.memories.get(&entry.time) {
                let values = record_values(record);
                out.push((
                    entry.time,
                    entry_for(
                        (source == target).then_some(entry.bytes.as_slice()),
                        values,
                        target,
                        grid,
                    ),
                ));
                continue;
            }
            out.push((entry.time, kept(entry, source, target)));
        }
    }
    // The deck's fresh entries, minted into the flavour they belong to: a
    // hot list takes the hot upserts, a memory list the memories.
    match flavour {
        f if f == CueListType::HOT => {
            for (slot, record) in &overlay.upserts {
                if base_slots.contains(&u32::from(*slot)) {
                    continue;
                }
                let values = record_values(record);
                out.push((u32::from(*slot), entry_for(None, values, target, grid)));
            }
        }
        _ => {
            for (position, record) in &overlay.memories {
                if base_positions.contains(position) || overlay.memory_cleared.contains(position) {
                    continue;
                }
                let values = record_values(record);
                out.push((*position, entry_for(None, values, target, grid)));
            }
        }
    }
    // A hot list rides in slot order, a memory list in time order — the
    // orders the deck's own files carry.
    match flavour {
        f if f == CueListType::HOT => out.sort_by_key(|(slot, _)| *slot),
        _ => out.sort_by_key(|(time, _)| *time),
    }
    out
}

/// The four numbers a deck's write record carries, as an entry names them.
fn record_values(record: &CueWriteRecord) -> (u32, u8, u32, u32) {
    (
        u32::from(record.hot_cue()),
        u8::try_from(record.cue_type()).map_or(1, |ty| if ty == 2 { 2 } else { 1 }),
        record.time_ms(),
        record.loop_ms(),
    )
}

/// An entry's bytes: the base's, patched in place, or a fresh mint.
fn entry_for(
    base: Option<&[u8]>,
    (hot_cue, cue_type, time, loop_time): (u32, u8, u32, u32),
    kind: FourCc,
    grid: &[u8],
) -> Vec<u8> {
    if let Some(bytes) = base
        && let Some(patched) = patched_entry(bytes, kind, cue_type, time, loop_time, grid)
    {
        return patched;
    }
    match kind {
        f if f == FourCc::PCOB => write::pcpt_entry(hot_cue, cue_type, time, loop_time),
        _ => write::pcp2_entry(hot_cue, cue_type, time, loop_time),
    }
}

/// An entry kept as it was: its own bytes when it already has the target's
/// shape, a re-encoding of its numbers when it migrates between list kinds.
fn kept(entry: &RawEntry, source: FourCc, target: FourCc) -> Vec<u8> {
    if source == target {
        entry.bytes.clone()
    } else {
        entry_for(None, entry.values(), target, &[])
    }
}

/// Patch an entry's own bytes: type, position and loop end in place, and for
/// an extended entry the loop's length in beats.
///
/// `None` when the bytes are too short to carry the fields — the caller
/// mints a fresh entry instead.
fn patched_entry(
    bytes: &[u8],
    kind: FourCc,
    cue_type: u8,
    time: u32,
    loop_time: u32,
    grid: &[u8],
) -> Option<Vec<u8>> {
    let (at_type, at_time, at_loop, at_length) = match kind {
        f if f == FourCc::PCOB => (28, 32, 36, None),
        f if f == FourCc::PCO2 => (16, 20, 24, Some(36)),
        _ => return None,
    };
    if bytes.len() < at_loop + 4 {
        return None;
    }
    let mut out = bytes.to_vec();
    let mut write_word = |at: usize, word: &[u8]| -> Option<()> {
        let slot = out.get_mut(at..at + word.len())?;
        slot.copy_from_slice(word);
        Some(())
    };
    write_word(at_type, &[cue_type])?;
    write_word(at_time, &time.to_be_bytes())?;
    write_word(at_loop, &loop_time.to_be_bytes())?;
    // An extended entry carries the loop's length in beats; a point carries
    // none, and a loop without a grid to count from carries the unset pair.
    if let Some(at_length) = at_length {
        let beats = (cue_type == 2 && loop_time != u32::MAX)
            .then(|| loop_beats(grid, time, loop_time))
            .flatten();
        let (numerator, denominator): (u16, u16) = match beats {
            Some(beats) => (beats, 1),
            None => (0, 0),
        };
        write_word(at_length, &numerator.to_be_bytes())?;
        write_word(at_length + 2, &denominator.to_be_bytes())?;
    }
    Some(out)
}

/// Write bytes over a file: a sibling temporary file, synced, renamed.
///
/// A crash mid-write leaves the original in place; the rename is the one
/// step a reader can see half of, and a rename is atomic.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("prolink-splice");
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::File::open(&tmp).and_then(|file| file.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    // -- the F67 ground truth: the deck's own cue tags, hex for hex ----------
    //
    // Captured off the stick the XDJ-AZ wrote directly (2026-10-28): a track
    // whose cue state went from cratify's eight hot cues to "hot cue A as a
    // point at 22 ms, hot cue B as a loop 6025→9032, a memory cue at
    // 3018" — the deck rewrote only these two files and left every database
    // untouched. These constants are the whole of what the splicer must be
    // able to reproduce.

    /// The `.DAT`'s hot `PCOB`: two `PCPT` entries, the deck's compact shape.
    const DECK_DAT_HOT_PCOB: &str = concat!(
        "50434f420000001800000088",
        "0000000100000002ffffffff",
        "504350540000001c00000038000000010000000000000000ffffffff010003e8",
        "00000016ffffffffffffffffffffffffffffffffffffffff",
        "504350540000001c00000038000000020000000000000000ffffffff020003e8",
        "0000178900002348ffffffffffffffffffffffffffffffff",
    );

    /// The `.DAT`'s memory `PCOB`: one `PCPT` entry, position 3018.
    const DECK_DAT_MEMORY_PCOB: &str = concat!(
        "50434f420000001800000050",
        "000000000000000100000000",
        "504350540000001c00000038000000000000000000000000ffffffff010003e8",
        "00000bcaffffffffffffffffffffffffffffffffffffffff",
    );

    /// The `.EXT`'s emptied `PCOB` lists, memory and hot: tags kept, no
    /// entries.
    const DECK_EXT_PCOB_MEMORY_EMPTY: &str = "50434f4200000018000000180000000000000000ffffffff";
    const DECK_EXT_PCOB_HOT_EMPTY: &str = "50434f4200000018000000180000000100000000ffffffff";

    /// The `.EXT`'s hot `PCO2`: two 48-byte `PCP2` entries, colour tails on
    /// (red for A, the loop's orange for B), the loop's eight beats at 36.
    const DECK_EXT_HOT_PCO2: &str = concat!(
        "50434f3200000014000000740000000100020000",
        "50435032000000100000003000000001010003e800000016ffffffff00000205",
        "000000000000000000000000ff0017",
        "50435032000000100000003000000002020003e800001789000023480000000b",
        "00120000000800010000000000ff8c00",
    );

    /// The `.EXT`'s memory `PCO2`: one 44-byte `PCP2` entry, no colour.
    const DECK_EXT_MEMORY_PCO2: &str = concat!(
        "50434f3200000014000000400000000000010000",
        "50435032000000100000002c00000000010003e800000bcaffffffff",
        "00000005000000000000000000000000",
    );

    fn hex_bytes(text: &str) -> Vec<u8> {
        (0..text.len() / 2)
            .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("valid hex"))
            .collect()
    }

    /// A cue write record, F65's live-session shape with the position
    /// swapped for the test's.
    fn cue_write_record(time_ms: u32) -> CueWriteRecord {
        const SET: &str = concat!(
            "54000000010001000000e8033e080000",
            "ffffffff000000000000000000000000",
            "00000000ffffffffffffffffffffffff",
            "ffffffff1a0000000000000000000000",
            "000000000000000000000400000000ff",
            "00170000"
        );
        let mut raw = hex_bytes(SET);
        raw[0x0c..0x10].copy_from_slice(&time_ms.to_le_bytes());
        CueWriteRecord::parse(&raw).expect("the set record")
    }

    /// The set record as a loop: type 2, the loop's end at `0x10`, and the
    /// slot the test's session holds it in.
    fn loop_write_record(slot: u16, time_ms: u32, loop_ms: u32) -> CueWriteRecord {
        let mut raw = cue_write_record(time_ms).raw().to_vec();
        raw[0x04..0x06].copy_from_slice(&slot.to_le_bytes());
        raw[0x06..0x08].copy_from_slice(&2u16.to_le_bytes());
        raw[0x10..0x14].copy_from_slice(&loop_ms.to_le_bytes());
        CueWriteRecord::parse(&raw).expect("the loop record")
    }

    /// The memory-cue record, F66's 76-byte shape.
    fn memory_write_record(time_ms: u32) -> CueWriteRecord {
        const SET: &str = concat!(
            "4c000000000001000000e803e4070000",
            "ffffffff000000000000000000000000",
            "00000000ffffffffffffffffffffffff",
            "ffffffff120000000000000000000000",
            "00000000000000000000000000000000",
            "0000"
        );
        let mut raw = hex_bytes(SET);
        raw[0x0c..0x10].copy_from_slice(&time_ms.to_le_bytes());
        CueWriteRecord::parse(&raw).expect("the memory record")
    }

    /// The session the stick's own files record: hot cue A at 22, hot cue B
    /// as a loop to 9032, a memory cue at 3018.
    fn session_overlay() -> TrackOverlay {
        TrackOverlay {
            upserts: [
                (1u16, cue_write_record(22)),
                (2, loop_write_record(2, 6025, 9032)),
            ]
            .into(),
            memories: [(3018, memory_write_record(3018))].into(),
            ..TrackOverlay::default()
        }
    }

    // -- container builders, cratify's own conventions ------------------------

    /// One tag: four-cc, declared header, payload — everything big-endian.
    fn tag_bytes(fourcc: [u8; 4], header_extra: &[u8], payload: &[u8]) -> Vec<u8> {
        let header_len = 12 + header_extra.len();
        let mut out = fourcc.to_vec();
        out.extend_from_slice(&u32::try_from(header_len).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(
            &u32::try_from(header_len + payload.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        out.extend_from_slice(header_extra);
        out.extend_from_slice(payload);
        out
    }

    /// A container the way cratify's own writer builds one: a 28-byte header
    /// holding a version and two `0x10000` words, with the header's last four
    /// bytes left zero as rekordbox's are.
    fn container(tags: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = tags.concat();
        let mut out = b"PMAI".to_vec();
        out.extend_from_slice(&28u32.to_be_bytes());
        out.extend_from_slice(
            &u32::try_from(28 + body.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&0x10000u32.to_be_bytes());
        out.extend_from_slice(&0x10000u32.to_be_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&body);
        out
    }

    /// A `PPTH` for a stick path.
    fn path_tag(path: &str) -> Vec<u8> {
        let mut text: Vec<u8> = path.encode_utf16().flat_map(u16::to_be_bytes).collect();
        text.extend_from_slice(&[0, 0]);
        let mut extra = u32::try_from(text.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes()
            .to_vec();
        extra.extend_from_slice(&0u32.to_be_bytes());
        tag_bytes(*b"PPTH", &extra, &text)
    }

    /// An empty `PCOB` list tag: flavour, no entries, `0xffff_ffff` past.
    fn empty_pcob(flavour: u32) -> Vec<u8> {
        let mut extra = flavour.to_be_bytes().to_vec();
        extra.extend_from_slice(&0u16.to_be_bytes());
        extra.extend_from_slice(&0u16.to_be_bytes());
        extra.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
        tag_bytes(*b"PCOB", &extra, &[])
    }

    /// An empty `PCO2` list tag.
    fn empty_pco2(flavour: u32) -> Vec<u8> {
        let mut extra = flavour.to_be_bytes().to_vec();
        extra.extend_from_slice(&0u16.to_be_bytes());
        extra.extend_from_slice(&0u16.to_be_bytes());
        tag_bytes(*b"PCO2", &extra, &[])
    }

    /// A `PQTZ` grid at 160 BPM from zero, so a loop's beats are countable.
    fn grid_tag(beats: usize) -> Vec<u8> {
        let mut extra = vec![0u8; 8];
        extra.extend_from_slice(&u32::try_from(beats).unwrap_or(u32::MAX).to_be_bytes());
        let mut payload = Vec::with_capacity(beats * 8);
        for beat in 0..beats {
            payload.extend_from_slice(&u16::try_from(beat + 1).unwrap_or(u16::MAX).to_be_bytes());
            payload.extend_from_slice(&16_000u16.to_be_bytes());
            payload.extend_from_slice(
                &u32::try_from(beat)
                    .unwrap_or(0)
                    .saturating_mul(375)
                    .to_be_bytes(),
            );
        }
        tag_bytes(*b"PQTZ", &extra, &payload)
    }

    /// A scratch directory for analysis files.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prolink-splice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        dir
    }

    fn write_scratch(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("a scratch file");
        path
    }

    /// The cue entries a written file carries, parsed back the way a player
    /// would: `(hot cue, type, time, loop end, beats)`.
    fn entries_of(bytes: &[u8], fourcc: FourCc, flavour: CueListType) -> Vec<(u32, u8, u32, u32)> {
        let parsed = AnlzFile::parse(bytes).expect("the written file parses");
        let lists = match fourcc {
            f if f == FourCc::PCOB => parsed
                .tags(FourCc::PCOB)
                .filter(|tag| write::list_flavour(tag) == Some(flavour))
                .flat_map(|tag| write::parse_pcob_entries(tag.payload()))
                .collect::<Vec<_>>(),
            _ => parsed
                .tags(FourCc::PCO2)
                .filter(|tag| write::list_flavour(tag) == Some(flavour))
                .flat_map(|tag| write::parse_pco2_entries(tag.payload()))
                .collect::<Vec<_>>(),
        };
        lists.iter().map(RawEntry::values).collect()
    }

    // -- the tests -----------------------------------------------------------

    #[test]
    fn fresh_entries_land_exactly_as_the_deck_writes_them() {
        // A cratify-style stick: empty cue lists everywhere, a grid to count
        // a loop's beats from.
        let dat = container(&[
            path_tag("/Contents/track.mp3"),
            grid_tag(64),
            empty_pcob(0),
            empty_pcob(1),
        ]);
        let ext = container(&[
            path_tag("/Contents/track.mp3"),
            grid_tag(64),
            empty_pcob(0),
            empty_pcob(1),
            empty_pco2(1),
            empty_pco2(0),
        ]);
        let dir = scratch("fresh");
        let dat_path = write_scratch(&dir, "ANLZ0000.DAT", &dat);
        let ext_path = write_scratch(&dir, "ANLZ0000.EXT", &ext);
        let overlay = session_overlay();

        assert!(
            splice_file(&dat_path, &overlay, File::Dat),
            "the .DAT changes"
        );
        assert!(
            splice_file(&ext_path, &overlay, File::Ext),
            "the .EXT changes"
        );

        let spliced_dat = std::fs::read(&dat_path).expect("the spliced .DAT");
        // The basic lists are byte for byte what the deck itself wrote.
        let parsed = AnlzFile::parse(&spliced_dat).expect("the spliced .DAT parses");
        let hot = parsed
            .tags(FourCc::PCOB)
            .find(|tag| write::list_flavour(tag) == Some(CueListType::HOT))
            .expect("the hot list");
        assert_eq!(
            hot.raw,
            hex_bytes(DECK_DAT_HOT_PCOB),
            "the hot PCOB is the deck's own bytes"
        );
        let memory = parsed
            .tags(FourCc::PCOB)
            .find(|tag| write::list_flavour(tag) == Some(CueListType::MEMORY))
            .expect("the memory list");
        // The list header's trailing word is one no reader consults — the
        // deck itself writes `0` here on a memory list and `0xffff_ffff` on
        // a hot one, cratify `0xffff_ffff` on an empty one — so it rides
        // through from the base rather than being invented. Everything else
        // is the deck's own bytes.
        let mask_num2 = |raw: &[u8]| -> Vec<u8> {
            let mut out = raw.to_vec();
            if let Some(slot) = out.get_mut(20..24) {
                slot.fill(0);
            }
            out
        };
        assert_eq!(
            mask_num2(&memory.raw),
            mask_num2(&hex_bytes(DECK_DAT_MEMORY_PCOB)),
            "the memory PCOB is the deck's own bytes, num2 aside"
        );

        let spliced_ext = std::fs::read(&ext_path).expect("the spliced .EXT");
        let parsed = AnlzFile::parse(&spliced_ext).expect("the spliced .EXT parses");
        // The .EXT's basic lists sit empty, as the deck leaves them.
        for flavour in [CueListType::MEMORY, CueListType::HOT] {
            let empty = parsed
                .tags(FourCc::PCOB)
                .find(|tag| write::list_flavour(tag) == Some(flavour))
                .expect("the .EXT's PCOB list");
            assert_eq!(
                empty.payload(),
                &[] as &[u8],
                "the {flavour:?} PCOB sits empty"
            );
        }
        assert_eq!(
            empty_pcob(0)[12..],
            hex_bytes(DECK_EXT_PCOB_MEMORY_EMPTY)[12..],
            "the emptied memory list is the deck's own tag shape"
        );
        assert_eq!(
            empty_pcob(1)[12..],
            hex_bytes(DECK_EXT_PCOB_HOT_EMPTY)[12..],
            "the emptied hot list is the deck's own tag shape"
        );
        // The extended lists carry the same numbers the deck's own do — our
        // entries are the compact shape, so equality is by value.
        assert_eq!(
            entries_of(&spliced_ext, FourCc::PCO2, CueListType::HOT),
            vec![(1, 1, 22, u32::MAX), (2, 2, 6025, 9032)],
            "the hot PCO2 carries the session's two cues"
        );
        assert_eq!(
            entries_of(&spliced_ext, FourCc::PCO2, CueListType::MEMORY),
            vec![(0, 1, 3018, u32::MAX)],
            "the memory PCO2 carries the session's one cue"
        );
        // And the untouched tags rode through whole.
        assert_eq!(
            parsed.payload(FourCc::PQTZ).expect("the grid survives"),
            grid_tag(64).get(24..).expect("the grid payload"),
            "the beat grid is untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_decks_own_bytes_splice_to_nothing() {
        // The stick's state after the session, built from the captured tags:
        // the same overlay that produced it must produce nothing.
        let deck_dat = container(&[
            path_tag("/Contents/track.mp3"),
            grid_tag(64),
            hex_bytes(DECK_DAT_MEMORY_PCOB),
            hex_bytes(DECK_DAT_HOT_PCOB),
        ]);
        let deck_ext = container(&[
            path_tag("/Contents/track.mp3"),
            grid_tag(64),
            hex_bytes(DECK_EXT_PCOB_MEMORY_EMPTY),
            hex_bytes(DECK_EXT_PCOB_HOT_EMPTY),
            hex_bytes(DECK_EXT_HOT_PCO2),
            hex_bytes(DECK_EXT_MEMORY_PCO2),
        ]);
        let dir = scratch("deck-idempotent");
        let dat_path = write_scratch(&dir, "ANLZ0000.DAT", &deck_dat);
        let ext_path = write_scratch(&dir, "ANLZ0000.EXT", &deck_ext);
        let overlay = session_overlay();

        assert!(
            !splice_file(&dat_path, &overlay, File::Dat),
            "the deck's own .DAT is already the splice's output"
        );
        assert!(
            !splice_file(&ext_path, &overlay, File::Ext),
            "the deck's own .EXT is already the splice's output"
        );
        assert_eq!(
            std::fs::read(&dat_path).expect("the .DAT"),
            deck_dat,
            "the file was not touched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_removal_drops_its_slot_and_a_memory_clear_drops_its_position() {
        let deck_dat = container(&[
            path_tag("/Contents/track.mp3"),
            grid_tag(64),
            hex_bytes(DECK_DAT_MEMORY_PCOB),
            hex_bytes(DECK_DAT_HOT_PCOB),
        ]);
        let dir = scratch("removal");
        let dat_path = write_scratch(&dir, "ANLZ0000.DAT", &deck_dat);
        // Hot cue A's slot, dropped.
        let overlay = TrackOverlay {
            removed: [1].into(),
            ..TrackOverlay::default()
        };
        assert!(splice_file(&dat_path, &overlay, File::Dat));
        let spliced = std::fs::read(&dat_path).expect("the spliced .DAT");
        assert_eq!(
            entries_of(&spliced, FourCc::PCOB, CueListType::HOT),
            vec![(2, 2, 6025, 9032)],
            "only the removed slot left the hot list"
        );
        assert_eq!(
            entries_of(&spliced, FourCc::PCOB, CueListType::MEMORY),
            vec![(0, 1, 3018, u32::MAX)],
            "the memory list is untouched by a hot removal"
        );
        // A memory position, cleared.
        let overlay = TrackOverlay {
            memory_cleared: [3018].into(),
            ..TrackOverlay::default()
        };
        assert!(splice_file(&dat_path, &overlay, File::Dat));
        let spliced = std::fs::read(&dat_path).expect("the spliced .DAT");
        assert_eq!(
            entries_of(&spliced, FourCc::PCOB, CueListType::MEMORY),
            Vec::<(u32, u8, u32, u32)>::new(),
            "the cleared position left the memory list"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replaced_entry_keeps_its_comment_and_its_colour() {
        // A rekordbox-written hot cue: comment, colour tail, grown shape.
        let mut comment: Vec<u8> = "intro".encode_utf16().flat_map(u16::to_be_bytes).collect();
        comment.extend_from_slice(&[0, 0]);
        let mut entry = vec![0u8; 44 + comment.len() + 4];
        let entry_len = u32::try_from(entry.len()).unwrap_or(u32::MAX);
        entry[..4].copy_from_slice(b"PCP2");
        entry[4..8].copy_from_slice(&20u32.to_be_bytes());
        entry[8..12].copy_from_slice(&entry_len.to_be_bytes());
        entry[12..16].copy_from_slice(&1u32.to_be_bytes());
        entry[16] = 1;
        entry[17..20].copy_from_slice(&[0, 3, 0xe8]);
        entry[20..24].copy_from_slice(&220u32.to_be_bytes());
        entry[24..28].copy_from_slice(&u32::MAX.to_be_bytes());
        entry[28] = 0;
        entry[40..44].copy_from_slice(
            &u32::try_from(comment.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        entry[44..44 + comment.len()].copy_from_slice(&comment);
        entry[44 + comment.len()..].copy_from_slice(&[0x2a, 0xff, 0x00, 0x00]);
        let mut header = 1u32.to_be_bytes().to_vec();
        header.extend_from_slice(&1u16.to_be_bytes());
        header.extend_from_slice(&0u16.to_be_bytes());
        let list = write::pco2_tag(&header, &entry).expect("the list tag");
        let base = container(&[path_tag("/Contents/track.mp3"), list]);
        let dir = scratch("preserve");
        let ext_path = write_scratch(&dir, "ANLZ0000.EXT", &base);
        // The deck re-asserts the slot at a new position: the comment and
        // the colour tail must survive, the position must move.
        let overlay = TrackOverlay {
            upserts: [(1, cue_write_record(2110))].into(),
            ..TrackOverlay::default()
        };
        assert!(splice_file(&ext_path, &overlay, File::Ext));
        let spliced = std::fs::read(&ext_path).expect("the spliced .EXT");
        let parsed = AnlzFile::parse(&spliced).expect("the spliced .EXT parses");
        let list = parsed.tag(FourCc::PCO2).expect("the list tag");
        let entries = write::parse_pco2_entries(list.payload());
        assert_eq!(entries.len(), 1);
        let out = &entries[0];
        assert_eq!((out.hot_cue, out.cue_type, out.time), (1, 1, 2110));
        assert_eq!(
            out.bytes.get(44 + comment.len()..48 + comment.len()),
            Some(&[0x2a, 0xff, 0x00, 0x00][..]),
            "the colour tail rode through the patch"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_whole_is_left_alone() {
        let dat = container(&[
            path_tag("/Contents/track.mp3"),
            empty_pcob(0),
            empty_pcob(1),
        ]);
        let mut truncated = dat.clone();
        truncated.truncate(truncated.len() - 8);
        let dir = scratch("whole");
        let dat_path = write_scratch(&dir, "ANLZ0000.DAT", &truncated);
        assert!(
            !splice_file(&dat_path, &overlay_removed(), File::Dat),
            "a truncated file is not spliced"
        );
        assert_eq!(std::fs::read(&dat_path).expect("the file"), truncated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn overlay_removed() -> TrackOverlay {
        TrackOverlay {
            removed: [1].into(),
            ..TrackOverlay::default()
        }
    }

    #[test]
    fn an_ext_without_pco2_tags_gains_them_after_its_pcob_lists() {
        // The cratify convention: cues in the .EXT's PCOB lists, PCO2 absent.
        // The splice moves the cues into fresh PCO2 tags and empties the
        // PCOB lists, as the deck's own files carry them.
        let mut header = 1u32.to_be_bytes().to_vec();
        header.extend_from_slice(&0u16.to_be_bytes());
        header.extend_from_slice(&1u16.to_be_bytes());
        header.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
        let hot = write::pcob_tag(&header, &write::pcpt_entry(1, 1, 220, u32::MAX))
            .expect("the PCOB tag");
        let base = container(&[path_tag("/Contents/track.mp3"), hot]);
        let dir = scratch("migrate");
        let ext_path = write_scratch(&dir, "ANLZ0000.EXT", &base);
        assert!(splice_file(&ext_path, &TrackOverlay::default(), File::Ext));
        let spliced = std::fs::read(&ext_path).expect("the spliced .EXT");
        let parsed = AnlzFile::parse(&spliced).expect("the spliced .EXT parses");
        // The PCOB list sits empty; the PCO2 list carries the cue.
        let pcob = parsed.tag(FourCc::PCOB).expect("the PCOB tag");
        assert_eq!(pcob.payload(), &[] as &[u8]);
        let pco2 = parsed.tag(FourCc::PCO2).expect("the fresh PCO2 tag");
        let entries = write::parse_pco2_entries(pco2.payload());
        assert_eq!(
            entries.iter().map(RawEntry::values).collect::<Vec<_>>(),
            vec![(1, 1, 220, u32::MAX)],
            "the migrated cue carries its numbers whole"
        );
        // And it sits after the last PCOB tag, as the deck's own files do.
        let pcob_at = spliced
            .windows(4)
            .position(|w| w == b"PCOB")
            .expect("the PCOB");
        let pco2_at = spliced
            .windows(4)
            .position(|w| w == b"PCO2")
            .expect("the PCO2");
        assert!(
            pco2_at > pcob_at,
            "the fresh PCO2 sits after the PCOB lists"
        );
        // A second splice changes nothing further.
        assert!(
            !splice_file(&ext_path, &TrackOverlay::default(), File::Ext),
            "the migrated file is already the splice's output"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
