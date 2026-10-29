// SPDX-License-Identifier: GPL-3.0-only

//! Writing ANLZ cue tags, and rendering a parsed container back to bytes.
//!
//! The deck's own ANLZ writer is the ground truth for every shape here (F67:
//! the "Pon da WTF" session, captured as the stick's own files between mount
//! and remount). A `PCPT` entry is 56 bytes with a 16-byte `0xff` tail; a
//! fresh `PCP2` entry is the deck's compact 44 bytes with no comment and no
//! colour. Both carry the same four numbers: the hot-cue number, the type,
//! the position and the loop end.
//!
//! Rendering a container back ([`AnlzFile::render_with`]) keeps every tag the
//! caller does not name byte for byte — waveforms, grids, paths — and
//! recomputes only the two declared lengths a change touches: the replaced
//! tag's own and the container's.

use crate::anlz::{AnlzFile, CueListType, FourCc, Tag};
use crate::error::{Error, Result};

/// A `PCPT` entry's total length, fixed.
pub const PCPT_LEN: usize = 56;

/// A minimal deck-style `PCP2` entry's length, without comment or colour.
pub const PCP2_MIN_LEN: usize = 44;

/// `PCPT` entries carry sixteen bytes of `0xff` past the fixed fields: the
/// deck-written records of F67 carry exactly this tail.
const PCPT_TRAILING: [u8; 16] = [0xff; 16];

/// The three bytes after an entry's type byte, `0x00 0x03 0xe8` — a thousand,
/// in whatever unit the field counts — on every entry seen, deck-written and
/// rekordbox-written alike.
const UNKNOWN_1000: [u8; 3] = [0x00, 0x03, 0xe8];

/// The words of a `PCPT` entry header past the common twelve: `28` declared,
/// the hot-cue number, status and an unknown left zero, and the two sort
/// words the deck leaves unset.
const PCPT_ENTRY_HEADER: u32 = 28;

/// The same for a minimal `PCP2` entry: `16` declared, the hot-cue number.
const PCP2_ENTRY_HEADER: u32 = 16;

/// Build a `PCPT` entry: a basic cue, extended with nothing.
///
/// The shape is the deck's own (F67): status 0, no sort keys, the `0xff`
/// tail. `loop_time` is `u32::MAX` for a point.
#[rustfmt::skip]
pub fn pcpt_entry(hot_cue: u32, cue_type: u8, time: u32, loop_time: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(PCPT_LEN);
    out.extend_from_slice(b"PCPT");
    out.extend_from_slice(&PCPT_ENTRY_HEADER.to_be_bytes());
    out.extend_from_slice(&(u32::try_from(PCPT_LEN).unwrap_or(u32::MAX)).to_be_bytes());
    out.extend_from_slice(&hot_cue.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // status: unset
    out.extend_from_slice(&0u32.to_be_bytes()); // unknown
    out.extend_from_slice(&0xffffu16.to_be_bytes()); // sort first
    out.extend_from_slice(&0xffffu16.to_be_bytes()); // sort last
    out.push(cue_type);
    out.extend_from_slice(&UNKNOWN_1000);
    out.extend_from_slice(&time.to_be_bytes());
    out.extend_from_slice(&loop_time.to_be_bytes());
    out.extend_from_slice(&PCPT_TRAILING);
    out
}

/// Build a minimal `PCP2` entry: hot cue, type, position, loop end — and no
/// comment, no colour, which is the deck's own fresh-cue shape (F67).
///
/// `loop_time` is `u32::MAX` for a point. The loop-length fields a rekordbox
/// entry carries sit past this entry's end and stay unwritten: the wire
/// computes a loop's beats from the grid at reply time, so the file need not
/// carry them.
#[rustfmt::skip]
pub fn pcp2_entry(hot_cue: u32, cue_type: u8, time: u32, loop_time: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(PCP2_MIN_LEN);
    out.extend_from_slice(b"PCP2");
    out.extend_from_slice(&PCP2_ENTRY_HEADER.to_be_bytes());
    out.extend_from_slice(&(u32::try_from(PCP2_MIN_LEN).unwrap_or(u32::MAX)).to_be_bytes());
    out.extend_from_slice(&hot_cue.to_be_bytes());
    out.push(cue_type);
    out.extend_from_slice(&UNKNOWN_1000);
    out.extend_from_slice(&time.to_be_bytes());
    out.extend_from_slice(&loop_time.to_be_bytes());
    out.extend_from_slice(&[0; 16]); // colour id, pad, loop length, comment length: unset
    out
}

/// The coloured `PCP2` shape's length: the compact entry plus the deck's
/// four `(code, r, g, b)` bytes at offset 44 — the 48-byte entries the
/// deck's own stick writes carry (F67: hot cue A red `00 ff 00 17`, a loop
/// orange `00 ff 8c 00`).
pub const PCP2_COLORED_LEN: usize = 48;

/// Build a `PCP2` entry with the deck's colour tail: the compact shape's
/// fields, the declared length grown to cover the four bytes.
#[rustfmt::skip]
pub fn pcp2_entry_colored(
    hot_cue: u32,
    cue_type: u8,
    time: u32,
    loop_time: u32,
    tail: [u8; 4],
) -> Vec<u8> {
    let mut out = pcp2_entry(hot_cue, cue_type, time, loop_time);
    let len = u32::try_from(PCP2_COLORED_LEN).unwrap_or(u32::MAX);
    if let Some(slot) = out.get_mut(8..12) {
        slot.copy_from_slice(&len.to_be_bytes());
    }
    out.extend_from_slice(&tail);
    out
}

/// Build a whole `PCOB` tag from a list header's bytes and an entries span.
///
/// The list header rides through verbatim — its flavour word and the `num2`
/// word whose meaning no reader is known to depend on — with only the entry
/// count patched to match the span. A `PCOB` list header is twelve bytes:
/// flavour, pad, count, `num2`.
///
/// # Errors
///
/// When `list_header` is not the twelve bytes a `PCOB` list header is.
pub fn pcob_tag(list_header: &[u8], entries: &[u8]) -> Result<Vec<u8>> {
    list_tag(FourCc::PCOB, 12, list_header, entries)
}

/// Build a whole `PCO2` tag from a list header's bytes and an entries span.
///
/// An `PCO2` list header is eight bytes: flavour, count, pad. See
/// [`pcob_tag`] for the rest.
///
/// # Errors
///
/// When `list_header` is not the eight bytes a `PCO2` list header is.
pub fn pco2_tag(list_header: &[u8], entries: &[u8]) -> Result<Vec<u8>> {
    list_tag(FourCc::PCO2, 8, list_header, entries)
}

fn list_tag(
    fourcc: FourCc,
    header_extra_len: usize,
    list_header: &[u8],
    entries: &[u8],
) -> Result<Vec<u8>> {
    if list_header.len() != header_extra_len {
        return Err(Error::malformed(
            0,
            format!("a {fourcc:?} list header is not {header_extra_len} bytes"),
        ));
    }
    let common = usize::try_from(crate::anlz::TAG_HEADER_LEN).unwrap_or(0);
    let header_len = u32::try_from(common + list_header.len()).unwrap_or(u32::MAX);
    let total = header_len.saturating_add(u32::try_from(entries.len()).unwrap_or(u32::MAX));
    let mut out = Vec::with_capacity(
        usize::try_from(total).unwrap_or(entries.len() + common + list_header.len()),
    );
    out.extend_from_slice(&fourcc.0);
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(list_header);
    out.extend_from_slice(entries);
    Ok(out)
}

/// The entry count a list tag declares, patched into a list header's bytes.
///
/// A `PCOB` header counts at its third word, a `PCO2` at its second; the
/// trailing `num2` word rides through untouched. `None` for a header shorter
/// than the count's own two bytes, or a four-cc that is no list tag at all.
pub fn with_entry_count(fourcc: FourCc, list_header: &[u8], count: u16) -> Option<Vec<u8>> {
    let at = match fourcc {
        f if f == FourCc::PCOB => 6,
        f if f == FourCc::PCO2 => 4,
        _ => return None,
    };
    let mut out = list_header.to_vec();
    let slot = out.get_mut(at..at + 2)?;
    slot.copy_from_slice(&count.to_be_bytes());
    Some(out)
}

/// A list header for a tag a file does not carry yet: the flavour and the
/// count, with the words a fresh tag leaves unset filled the way rekordbox's
/// own empty lists are — a `PCOB`'s `num2` at `0xffff_ffff`, a `PCO2`'s pad
/// at zero.
pub fn fresh_list_header(fourcc: FourCc, flavour: CueListType, count: u16) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match fourcc {
        f if f == FourCc::PCOB => {
            out.extend_from_slice(&flavour.0.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
            Some(out)
        }
        f if f == FourCc::PCO2 => {
            out.extend_from_slice(&flavour.0.to_be_bytes());
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes());
            Some(out)
        }
        _ => None,
    }
}

/// One cue entry, read off a list tag's entries span.
///
/// The entry's own bytes ride beside the four numbers the splicer merges on,
/// so an entry the overlay does not touch is re-emitted exactly as the file
/// carried it — a rekordbox comment, a colour tail, a longer header, all of
/// it survives a splice that never names the entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RawEntry {
    /// The whole entry, from its magic to its end.
    pub bytes: Vec<u8>,
    /// The hot-cue number: 1–8 for a hot slot, 0 for a memory cue.
    pub hot_cue: u32,
    /// The type byte: `1` a point, `2` a loop.
    pub cue_type: u8,
    /// Where the cue sits, in milliseconds.
    pub time: u32,
    /// Where a loop jumps back from; `u32::MAX` for a point.
    pub loop_time: u32,
}

impl RawEntry {
    /// The four numbers an entry carries, as a fresh entry would name them.
    pub fn values(&self) -> (u32, u8, u32, u32) {
        (self.hot_cue, self.cue_type, self.time, self.loop_time)
    }
}

/// Walk a `PCOB` tag's entries span: fixed 56-byte `PCPT` records.
///
/// A span that ends mid-entry or carries a foreign magic stops the walk
/// there, keeping what came before — the same posture the container's own
/// parse takes.
pub fn parse_pcob_entries(span: &[u8]) -> Vec<RawEntry> {
    span.chunks_exact(PCPT_LEN)
        .map_while(|chunk| entry_at(chunk, 28, 32, 36))
        .collect()
}

/// Walk a `PCO2` tag's entries span: length-prefixed `PCP2` records, the
/// deck's compact 44 and rekordbox's grown shapes alike.
pub fn parse_pco2_entries(span: &[u8]) -> Vec<RawEntry> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(word) = span.get(at + 8..at + 12) {
        let total = u32::from_be_bytes(word.try_into().unwrap_or([0; 4]));
        let total = usize::try_from(total).unwrap_or(usize::MAX);
        if total < usize::try_from(crate::anlz::TAG_HEADER_LEN).unwrap_or(0)
            || at.saturating_add(total) > span.len()
        {
            break;
        }
        let Some(chunk) = span.get(at..at + total) else {
            break;
        };
        let Some(entry) = entry_at(chunk, 16, 20, 24) else {
            break;
        };
        out.push(entry);
        at += total;
    }
    out
}

/// The four fields an entry carries, from the offsets its flavour fixes.
fn entry_at(chunk: &[u8], at_type: usize, at_time: usize, at_loop: usize) -> Option<RawEntry> {
    let word = |at: usize| -> Option<u32> {
        <[u8; 4]>::try_from(chunk.get(at..at + 4)?)
            .ok()
            .map(u32::from_be_bytes)
    };
    let magic = chunk.get(..4)?;
    if magic != b"PCPT" && magic != b"PCP2" {
        return None;
    }
    Some(RawEntry {
        bytes: chunk.to_vec(),
        hot_cue: word(12)?,
        cue_type: *chunk.get(at_type)?,
        time: word(at_time)?,
        loop_time: word(at_loop)?,
    })
}

impl AnlzFile {
    /// Whether the parse walked the declared file whole: every tag read, no
    /// trailing slack, the file on disk exactly as long as it declares.
    ///
    /// A splicer renders only whole files — re-rendering one whose walk
    /// stopped early would silently drop everything past the stop.
    pub fn walked_whole(&self, file_len_on_disk: usize) -> bool {
        let walked = usize::try_from(self.header_len)
            .unwrap_or(usize::MAX)
            .saturating_add(
                self.tags
                    .iter()
                    .map(|tag| usize::try_from(tag.total_len).unwrap_or(usize::MAX))
                    .sum(),
            );
        u64::from(self.file_len) == u64::try_from(file_len_on_disk).unwrap_or(u64::MAX)
            && walked == file_len_on_disk
    }
}

/// The flavour a cue-list tag declares, off its header's first word.
///
/// Both list tags open their own header with the flavour: `0` memory, `1`
/// hot. `None` for a tag that is no list tag, or one whose flavour word is
/// neither.
pub fn list_flavour(tag: &Tag) -> Option<CueListType> {
    let word: [u8; 4] = tag.header_extra().get(..4)?.try_into().ok()?;
    match u32::from_be_bytes(word) {
        n if n == CueListType::MEMORY.0 => Some(CueListType::MEMORY),
        n if n == CueListType::HOT.0 => Some(CueListType::HOT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deck's own memory-cue `PCPT` (F67): position 3018, a point, the
    /// `0xff` tail — captured off the stick the XDJ-AZ wrote itself.
    const DECK_PCPT_MEMORY: &str = concat!(
        "504350540000001c00000038",
        "000000000000000000000000ffffffff",
        "010003e800000bcaffffffffffffffffffffffffffffffffffffffff",
    );

    /// The deck's own memory-cue `PCP2`, the compact 44 bytes: a point at
    /// 3018, no comment, no colour.
    const DECK_PCP2_MEMORY: &str = concat!(
        "50435032000000100000002c",
        "00000000010003e800000bcaffffffff00000005",
        "000000000000000000000000",
    );

    fn hex_bytes(text: &str) -> Vec<u8> {
        (0..text.len() / 2)
            .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("valid hex"))
            .collect()
    }

    #[test]
    fn a_pcpt_entry_is_the_deck_s_own_bytes() {
        let entry = pcpt_entry(0, 1, 3018, u32::MAX);
        assert_eq!(entry, hex_bytes(DECK_PCPT_MEMORY));
        assert_eq!(entry.len(), PCPT_LEN);
    }

    #[test]
    fn a_pcp2_entry_is_the_deck_s_own_compact_shape() {
        // The deck's memory entry: 44 bytes, colour id 0, comment length 0.
        let entry = pcp2_entry(0, 1, 3018, u32::MAX);
        assert_eq!(entry.len(), PCP2_MIN_LEN);
        let deck = hex_bytes(DECK_PCP2_MEMORY);
        // Everything up to the seven unknown bytes after the colour id is
        // the deck's own; those seven ride whatever the writer had, and the
        // fields past them — loop length, comment length — are ours again.
        assert_eq!(&entry[..29], &deck[..29], "the fixed fields");
        assert_eq!(&entry[36..], &deck[36..], "loop length and comment length");
    }

    #[test]
    fn a_colored_pcp2_entry_is_the_deck_s_own_f67_shape() {
        // Hot cue A red: the deck's own stick-write put the four colour
        // bytes at 44, the entry grown to 48 with the length field saying
        // so (F67).
        let entry = pcp2_entry_colored(1, 1, 22, u32::MAX, [0x00, 0xff, 0x00, 0x17]);
        assert_eq!(entry.len(), PCP2_COLORED_LEN);
        assert_eq!(
            u32::from_be_bytes(entry[8..12].try_into().expect("the length")),
            48,
            "the entry length covers the tail"
        );
        assert_eq!(&entry[44..48], &[0x00, 0xff, 0x00, 0x17], "the tail");
        // The compact shape's fields ride unchanged, the length aside: the
        // coloured entry's own length word says 48.
        let compact = pcp2_entry(1, 1, 22, u32::MAX);
        assert_eq!(&entry[..8], &compact[..8], "the tag and header length");
        assert_eq!(&entry[12..44], &compact[12..44], "the fields");
    }

    #[test]
    fn a_list_tag_declares_its_lengths_whole() {
        let header = fresh_list_header(FourCc::PCOB, CueListType::HOT, 1).expect("a header");
        let entry = pcpt_entry(1, 1, 22, u32::MAX);
        let raw = pcob_tag(&header, &entry).expect("the tag");
        let parsed = AnlzFile::parse(&AnlzTestFile::wrap(&raw)).expect("the tag parses");
        let tag = parsed.tag(FourCc::PCOB).expect("the PCOB tag");
        assert_eq!(tag.header_len, 24);
        assert_eq!(tag.total_len, 24 + u32::try_from(PCPT_LEN).unwrap_or(0));
        assert_eq!(list_flavour(tag), Some(CueListType::HOT));
        assert_eq!(
            with_entry_count(FourCc::PCOB, tag.header_extra(), 0).expect("the patched header"),
            fresh_list_header(FourCc::PCOB, CueListType::HOT, 0).expect("the fresh header"),
            "a count of zero round-trips to the fresh header"
        );
    }

    #[test]
    fn a_whole_file_is_whole_and_a_short_one_is_not() {
        let header = fresh_list_header(FourCc::PCO2, CueListType::MEMORY, 0).expect("a header");
        let raw = pco2_tag(&header, &[]).expect("the tag");
        let file = AnlzTestFile::wrap(&raw);
        let parsed = AnlzFile::parse(&file).expect("the file parses");
        assert!(parsed.walked_whole(file.len()));
        assert!(!parsed.walked_whole(file.len() - 1), "short on disk");
        assert!(!parsed.walked_whole(file.len() + 1), "long on disk");
    }

    /// The `PMAI` wrapper around one tag, as a real file carries it.
    struct AnlzTestFile;

    impl AnlzTestFile {
        fn wrap(tag: &[u8]) -> Vec<u8> {
            let mut out = b"PMAI".to_vec();
            out.extend_from_slice(&28u32.to_be_bytes());
            out.extend_from_slice(
                &u32::try_from(28 + tag.len())
                    .unwrap_or(u32::MAX)
                    .to_be_bytes(),
            );
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&0x10000u32.to_be_bytes());
            out.extend_from_slice(&0x10000u32.to_be_bytes());
            out.extend_from_slice(&[0u8; 4]);
            out.extend_from_slice(tag);
            out
        }
    }
}
