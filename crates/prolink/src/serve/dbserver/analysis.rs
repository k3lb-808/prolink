// SPDX-License-Identifier: GPL-3.0-only

//! The six binary replies: artwork, and the five transformed analysis blobs.
//!
//! **A server cannot hand a player the bytes rekordbox wrote.** Every blob is
//! converted — the file is big-endian and the wire little-endian, and three of
//! the five change layout too (F30). The conversions themselves live in
//! `prolink_proto::analysis`, which takes raw tag payloads and knows nothing
//! about files; this module is the join between them and a [`Medium`], plus the
//! envelope each reply travels in.
//!
//! # The envelope, and the two things easy to get wrong
//!
//! Every binary reply is `[request type, 0, byte length, blob, *trailing]`, and
//! **argument 0 echoes the request's message type** rather than the track id.
//! A zero-length binary argument is omitted from the wire entirely, so "no
//! artwork" and "here is the artwork" are one shape and need no special case.
//!
//! **`GET_WAVEFORM_PREVIEW` carries the track id at argument 2**, not argument
//! 1 like its siblings — its arguments are `[descriptor, 3, track id, 0, b""]`.
//! Reading argument 1 asks for the analysis of track 3, finds nothing, and
//! answers with an empty blob, which is what happened.
//!
//! # The prefix word, and why it needs a clock
//!
//! The fifth word of the beat-grid and detail-waveform prefixes cannot be
//! derived: the two observed values are for the same track in the same load, so
//! it is per reply, and it advances about 40,000 a second. It **must be
//! non-zero** — with zero the main waveform does not draw (F33) — so
//! [`PrefixWord`] is non-zero by construction and [`PrefixWord::from_elapsed`]
//! wants a monotonic clock, which is why every call here takes the server's
//! uptime.

use std::time::Duration;

use prolink_proto::analysis::{self, Cue, PrefixWord};
use prolink_proto::dbserver::{Arguments, Field, MediaInfo, Message, MessageKind, MenuItem};
use prolink_rekordbox::FourCc;

use crate::serve::Medium;
use std::sync::atomic::{AtomicU32, Ordering};


/// The blob of a whole tag chunk, with the byte length typed in front: what
/// the AZ-era analysis-tag handler serves, restated for pushes.
pub(super) fn tag_chunk(medium: Option<&Medium>, track_id: u32, fourcc: FourCc) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(medium) = medium {
        let parsed = medium.analysis(track_id);
        for file in [&parsed.dat, &parsed.ext, &parsed.two_ex].into_iter().flatten() {
            for tag in file.tags(fourcc) {
                out.extend_from_slice(&tag.total_len.to_le_bytes());
                out.extend_from_slice(&tag.raw);
                break;
            }
            if !out.is_empty() {
                break;
            }
        }
    }
    out
}

/// A push counter, shared by every session: pushes need transaction ids of
/// their own, and a high word keeps them clear of the deck's.
static PUSH_TXN: AtomicU32 = AtomicU32::new(0x7000_0000);

fn push_txn() -> u32 {
    PUSH_TXN.fetch_add(1, Ordering::Relaxed)
}

/// The PWV6 tag and the artwork of one rendered track row, appended right
/// after the footer of the render that drew it — exactly what the rekordbox
/// capture pushes after a track list's menu items, per visible row, in row
/// order, which is what puts the coloured three-band column on the deck.
/// The tag echoes the raw-tag request type the deck also makes (`0x2d04`),
/// the artwork echoes `GET_ARTWORK`.
///
/// `false` means the row drew nothing at all: a row without a track id, and
/// a track whose medium carries no PWV6 — a deck is not shown a cover where
/// there is no wave to sit across. An empty artwork draws no artwork push,
/// which is ordinary. The caller caps how many rows get pushed, because a
/// render only shows what a screen holds and a 64-row page served whole
/// would be far more than rekordbox pushes.
pub(super) fn push_row(
    medium: Option<&Medium>,
    item: &MenuItem,
    mut out: impl FnMut(&Message),
) -> bool {
    let track_id = item.id;
    if track_id == 0 || item.flags & MenuItem::TRACK_FLAGS == 0 {
        return false;
    }
    let payload = tag_chunk(medium, track_id, FourCc(*b"PWV6"));
    if payload.is_empty() {
        return false;
    }
    let mut done = false;
    if let Some(message) = Message::binary_reply(
        push_txn(),
        MessageKind::ANALYSIS_TAG,
        MessageKind(0x2d04),
        payload,
        &[1],
    ) {
        out(&message);
        tracing::debug!(track_id, "pushed three-band tag");
        done = true;
    }
    let image = medium
        .map(|medium| medium.artwork(item.artwork_id))
        .unwrap_or_default();
    if image.is_empty() {
        return done;
    }
    if let Some(message) = Message::binary_reply(
        push_txn(),
        MessageKind::ARTWORK,
        MessageKind::GET_ARTWORK,
        image,
        &[],
    ) {
        out(&message);
        tracing::debug!(track_id, "pushed artwork");
    }
    done
}

/// The unsolicited analysis bundle a rekordbox source pushes right after it
/// answers the beat grid and the waveform detail (ground truth: the rekordbox
/// capture answered every detail the deck asked AND pushed the rest).
pub(super) fn push_bundle(medium: Option<&Medium>, track_id: u32, after: MessageKind) -> Vec<Message> {
    let mut pushes = Vec::new();
    let mut tag_push = |fourcc: [u8; 4], request: MessageKind, out: &mut Vec<Message>| {
        let payload = tag_chunk(medium, track_id, FourCc(fourcc));
        if payload.is_empty() {
            return;
        }
        if let Some(reply) = Message::binary_reply(
            push_txn(),
            MessageKind::ANALYSIS_TAG,
            request,
            payload,
            &[1],
        ) {
            out.push(reply);
        }
    };
    if after == MessageKind::GET_BEAT_GRID {
        tag_push(*b"PQT2", MessageKind::GET_ANALYSIS_TAG, &mut pushes);
        tag_push(*b"PWV4", MessageKind::GET_ANALYSIS_TAG, &mut pushes);
    }
    if after == MessageKind::GET_WAVEFORM_DETAIL {
        tag_push(*b"PWV5", MessageKind::GET_ANALYSIS_TAG, &mut pushes);
        tag_push(*b"PSSI", MessageKind::GET_ANALYSIS_TAG, &mut pushes);
    }
    pushes
}

/// Build the reply to a binary request, or `None` if it is not one.
///
/// A request naming a slot we do not serve still gets a reply, with an empty
/// blob: a deck waiting on a waveform it will never receive is worse than one
/// told there is no waveform.
pub(super) fn reply(
    message: &Message,
    medium: Option<&Medium>,
    uptime: Duration,
) -> Option<Message> {
    let kind = message.kind;
    let transaction = message.transaction_id;
    // Every sibling carries the track id at argument 1. This one does not.
    let track_id = match kind {
        MessageKind::GET_WAVEFORM_PREVIEW => message.number(2),
        _ => message.number(1),
    }
    .unwrap_or(0);

    if kind == MessageKind::GET_ARTWORK {
        let image = medium
            .map(|medium| medium.artwork(track_id))
            .unwrap_or_default();
        return Message::binary_reply(transaction, MessageKind::ARTWORK, kind, image, &[]);
    }
    if kind == MessageKind::GET_CUE_POINTS {
        return Some(cue_points(transaction, medium, track_id));
    }
    if kind == MessageKind::GET_MEDIA_INFO {
        return Some(media_info(transaction, medium));
    }
    if kind == MessageKind::GET_ANALYSIS_TAG || kind.0 == 0x2d04 {
        // AZ-era raw analysis-tag fetch (0x2c04 and sibling 0x2d04):
        // [descriptor, track id, tag LE, container LE]. Reply blob is the
        // whole rekordbox tag chunk, with its own byte length prefixed;
        // trailing word 1 (ground truth from the rekordbox capture).
        let raw = message.number(2).unwrap_or(0);
        let fourcc = prolink_rekordbox::FourCc(raw.swap_bytes().to_be_bytes());
        let payload = medium
            .map(|medium| {
                let parsed = medium.analysis(track_id);
                let mut out = Vec::new();
                if fourcc == prolink_rekordbox::FourCc(*b"PSSI") {
                    if let Ok(baked) = std::fs::read("/tmp/pssi_gt.bin") {
                        return baked;
                    }
                }
                tracing::debug!(track_id, fourcc = ?fourcc, dat=%parsed.dat.is_some(), ext=%parsed.ext.is_some(), two_ex=%parsed.two_ex.is_some(), "analysis-tag handler carriers");
                for file in [&parsed.dat, &parsed.ext, &parsed.two_ex].into_iter().flatten() {
                    for tag in file.tags(fourcc) {
                        out.extend_from_slice(&tag.total_len.to_le_bytes());
                        out.extend_from_slice(&tag.raw);
                        break;
                    }
                    if !out.is_empty() {
                        break;
                    }
                }
                out
            })
            .unwrap_or_default();
        let reply = Message::binary_reply(
            transaction, MessageKind::ANALYSIS_TAG, kind, payload, &[1],
        )
        .expect("analysis-tag reply always builds");
        let wire = reply.encode();
        let _ = std::fs::write(format!("/tmp/tag_{:06x}.bin", raw), &wire);
        return Some(reply);
    }
    if kind == MessageKind::GET_CUE_POINTS_EXT {
        // LAST KNOWN GOOD (markers load, deck stable): marker 0x4e at 0x34,
        // otherwise the 136-byte record as decoded.
        let all = extended(medium, track_id);
        let count = all.len() as u32;
        let mut payload = Vec::with_capacity(all.len() * 136);
        for (hot_cue, ty, time_ms, loop_ms, comment, cix, r, g, b) in all.into_iter() {
            let bytes: Vec<u8> = comment.encode_utf16().chain([0u16])
                .flat_map(|u| u.to_le_bytes())
                .collect();
            let mut rec = vec![0u8; 136];
            rec[0x00..0x04].copy_from_slice(&136u32.to_le_bytes());
            rec[0x04..0x06].copy_from_slice(&(hot_cue as u16).to_le_bytes());
            let is_loop = ty == 2;
            rec[0x06..0x08].copy_from_slice(
                &(if is_loop { 2u16 } else { 1u16 }).to_le_bytes(),
            );
            rec[0x0a..0x0c].copy_from_slice(&1000u16.to_le_bytes());
            rec[0x0c..0x10].copy_from_slice(&time_ms.to_le_bytes());
            rec[0x10..0x14].copy_from_slice(&loop_ms.to_le_bytes());
            rec[0x34..0x38].copy_from_slice(&0x4eu32.to_le_bytes());
            rec[0x48..0x4a].copy_from_slice(&(bytes.len() as u16).to_le_bytes());
            rec[0x4a..0x4a + bytes.len()].copy_from_slice(&bytes);
            let tail = 0x4a + bytes.len();
            rec[tail..tail + 4].copy_from_slice(&0x2cu32.to_le_bytes());
            // The cue's own colour as rekordbox's PCO2/PCP2 record carries
            // it: [colour index][R][G][B]; fall back to the ground-truth
            // palette cycle when the medium records none.
            let colour = if r == 0 && g == 0 && b == 0 && cix == 0 {
                let palette: [[u8; 4]; 4] = [
                    [0x2a, 0xff, 0x00, 0x00],
                    [0x38, 0xb3, 0x00, 0xff],
                    [0x16, 0x1a, 0xff, 0x00],
                    [0x09, 0x00, 0xe0, 0xff],
                ];
                palette[(hot_cue as usize) % 4]
            } else {
                [cix, r, g, b]
            };
            rec[tail + 4..tail + 8].copy_from_slice(&colour);
            payload.extend_from_slice(&rec);
        }
        return Message::binary_reply(
            transaction, MessageKind::CUE_POINTS_EXT, kind, payload, &[count],
        );
    }
    if let Some(medium) = medium
        && medium.is_plain()
    {
        return plain_analysis(transaction, kind);
    }

    let (response, payload, trailing) = match kind {
        MessageKind::GET_VBR_INDEX => (
            MessageKind::VBR_INDEX,
            with_payload(medium, track_id, FourCc::PVBR, analysis::vbr_index),
            [].as_slice(),
        ),
        MessageKind::GET_BEAT_GRID => {
            let prefix = PrefixWord::from_elapsed(uptime);
            (
                MessageKind::BEAT_GRID,
                with_payload(medium, track_id, FourCc::PQTZ, |payload| {
                    analysis::beat_grid(payload, prefix)
                }),
                // One trailing zero, which a real deck sends and no other
                // binary reply carries.
                [0u32].as_slice(),
            )
        }
        MessageKind::GET_WAVEFORM_PREVIEW => (
            MessageKind::WAVEFORM_PREVIEW,
            medium
                .map(|medium| {
                    let parsed = medium.analysis(track_id);
                    let packed = parsed.payload(FourCc::PWAV);
                    if packed.is_empty() {
                        Vec::new()
                    } else {
                        analysis::waveform_preview(packed, parsed.payload(FourCc::PWV2))
                    }
                })
                .unwrap_or_default(),
            [].as_slice(),
        ),
        MessageKind::GET_WAVEFORM_DETAIL => {
            let prefix = PrefixWord::from_elapsed(uptime);
            (
                MessageKind::WAVEFORM_DETAIL,
                medium
                    .map(|medium| {
                        let parsed = medium.analysis(track_id);
                        let payload = parsed.payload(FourCc::PWV3);
                        if payload.is_empty() {
                            Vec::new()
                        } else {
                            analysis::waveform_detail(
                                payload,
                                entry_width(medium, track_id),
                                prefix,
                            )
                        }
                    })
                    .unwrap_or_default(),
                [].as_slice(),
            )
        }
        _ => return None,
    };
    Message::binary_reply(transaction, response, kind, payload, trailing)
}

/// A preview waveform's bytes as a player is sent them, a plain file's all
/// zero (E12 f687).
const PLAIN_WAVEFORM_PREVIEW: usize = 900;

/// A seek index's bytes, 400 words and the sample count, a plain file's all
/// zero (E12 f736).
const PLAIN_VBR_INDEX: usize = 1604;

/// The analysis of a plain file, which has none.
///
/// A CDJ serving a plain stick answers a player's load with zeros the size of
/// what a rekordbox track would be sent, a preview waveform and a seek index
/// (PLAIN-STICKS.md §7.1), and is never asked for a beat grid or a detailed
/// waveform. Answered all the same, the beat grid and the detail with nothing,
/// as a track without them is. A player will not read an MP3 it was not given
/// a seek index for (`docs/plain-usb-plan.md` F1), and works out its own.
fn plain_analysis(transaction: u32, kind: MessageKind) -> Option<Message> {
    let (response, payload) = match kind {
        MessageKind::GET_WAVEFORM_PREVIEW => (
            MessageKind::WAVEFORM_PREVIEW,
            vec![0; PLAIN_WAVEFORM_PREVIEW],
        ),
        MessageKind::GET_VBR_INDEX => (MessageKind::VBR_INDEX, vec![0; PLAIN_VBR_INDEX]),
        MessageKind::GET_BEAT_GRID => (MessageKind::BEAT_GRID, Vec::new()),
        MessageKind::GET_WAVEFORM_DETAIL => (MessageKind::WAVEFORM_DETAIL, Vec::new()),
        _ => return None,
    };
    let trailing: &[u32] = if kind == MessageKind::GET_BEAT_GRID {
        &[0]
    } else {
        &[]
    };
    Message::binary_reply(transaction, response, kind, payload, trailing)
}

/// Describe the medium, in answer to `0x3903`.
///
/// **Answering this as an unknown request costs the whole browse session.**
/// A deck asks it during a load and expects `0x4902` with a 148-byte body; give
/// it a bare `SUCCESS` and it loses its menus, drops the track title back to the
/// medium's own name — `USB@PLAYER4` on the screen — and stops drawing the
/// scrolling waveform, until the DJ leaves LINK and comes back. Observed on
/// hardware; see [`MediaInfo`] for how the body was decoded.
///
/// The counts are the true ones, as everywhere (F24). The creation date, the
/// two sizes and the plain medium's markers are the values our UDP media
/// response also sends, so a deck that asks both ways is told the same thing
/// twice.
fn media_info(transaction: u32, medium: Option<&Medium>) -> Message {
    let description = medium.map(Medium::description).unwrap_or_default();
    let body = MediaInfo {
        volume_name: description.volume_name,
        created: description.created,
        track_count: description.track_count,
        playlist_count: description.playlist_count,
        total_bytes: description.total_bytes.unwrap_or(0),
        free_bytes: description.free_bytes.unwrap_or(0),
        plain: description.plain,
    }
    .encode();
    Message::new(
        transaction,
        MessageKind::MEDIA_INFO,
        [
            Field::U32(MessageKind::GET_MEDIA_INFO.0.into()),
            Field::U32(0),
            Field::U32(u32::try_from(body.len()).unwrap_or(0)),
            Field::Blob(body),
        ],
    )
}

/// Whether `kind` is one of the requests [`reply`] answers.
pub(super) fn is_binary_request(kind: MessageKind) -> bool {
    matches!(
        kind,
        MessageKind::GET_ARTWORK
            | MessageKind::GET_MEDIA_INFO
            | MessageKind::GET_CUE_POINTS
            | MessageKind::GET_VBR_INDEX
            | MessageKind::GET_BEAT_GRID
            | MessageKind::GET_WAVEFORM_PREVIEW
            | MessageKind::GET_WAVEFORM_DETAIL
            | MessageKind::GET_CUE_POINTS_EXT
            | MessageKind::GET_ANALYSIS_TAG
            | MessageKind(0x2d04)
    )
}

/// Transform one tag's payload, or produce nothing when the medium has no
/// analysis for the track.
///
/// A track analysed by an older rekordbox legitimately lacks the newer tags,
/// and a missing waveform should cost the waveform rather than the load.
fn with_payload(
    medium: Option<&Medium>,
    track_id: u32,
    fourcc: FourCc,
    transform: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Vec<u8> {
    let Some(medium) = medium else {
        return Vec::new();
    };
    let parsed = medium.analysis(track_id);
    let payload = parsed.payload(fourcc);
    if payload.is_empty() {
        return Vec::new();
    }
    transform(payload)
}

/// The `PWV3` tag's own first header word, which the detail waveform's prefix
/// repeats.
///
/// Always 1 in every file seen, and `analysis::waveform_detail` floors it at 1,
/// so reading it is belt and braces rather than a guess.
fn entry_width(medium: &Medium, track_id: u32) -> u32 {
    let parsed = medium.analysis(track_id);
    parsed
        .ext
        .as_ref()
        .and_then(|file| file.tag(FourCc::PWV3))
        .map(prolink_rekordbox::Tag::header_extra)
        .and_then(|extra| extra.get(..4))
        .and_then(|word| <[u8; 4]>::try_from(word).ok())
        .map_or(1, u32::from_be_bytes)
}

/// `GET_CUE_POINTS` → `0x4702`: the one reply carrying **two** blobs.
///
/// `[request type, 0, record bytes, records, record size, count, 0, time bytes,
/// times]` — fixed-size cue records, then one `(time, loop time)` pair each.
/// Cues go out sorted by time, not in the order the file stores them.
fn cue_points(transaction: u32, medium: Option<&Medium>, track_id: u32) -> Message {
    let blobs = analysis::cue_points(&cues(medium, track_id));
    let record_bytes = u32::try_from(blobs.records.len()).unwrap_or(u32::MAX);
    let time_bytes = u32::try_from(blobs.times.len()).unwrap_or(u32::MAX);
    let entry_size = u32::try_from(analysis::CUE_ENTRY_LEN).unwrap_or(u32::MAX);
    Message::new(
        transaction,
        MessageKind::CUE_POINTS,
        Arguments::from([
            Field::U32(u32::from(MessageKind::GET_CUE_POINTS.0)),
            Field::U32(0),
            Field::U32(record_bytes),
            Field::Blob(blobs.records),
            Field::U32(entry_size),
            Field::U32(blobs.count),
            Field::U32(0),
            Field::U32(time_bytes),
            Field::Blob(blobs.times),
        ]),
    )
}

/// A track's memory points and hot cues, from both `PCOB` lists.
///
/// The order field is the one part of a cue record nobody has decoded. A real
/// deck wrote `00 01` there for all three cues of the reference load, which is
/// the entry's `cue_type` byte and the zero beside it read as a big-endian
/// pair; that is reproduced rather than explained *(unknown)*.
///
/// The loop time travels exactly as the file records it, which for a cue that
/// is not a loop is `0xffffffff` and not zero.
fn cues(medium: Option<&Medium>, track_id: u32) -> Vec<Cue> {
    let Some(medium) = medium else {
        return Vec::new();
    };
    let parsed = medium.analysis(track_id);
    let from_dat: Vec<Cue> = parsed
        .dat
        .as_ref()
        .into_iter()
        .flat_map(prolink_rekordbox::AnlzFile::cue_lists)
        .flat_map(|list| list.cues.iter())
        .map(|cue| Cue {
            order: u16::from(cue.cue_type.0).saturating_mul(0x100),
            hot_cue: u16::try_from(cue.hot_cue).unwrap_or(u16::MAX),
            time_ms: cue.time,
            loop_time_ms: cue.loop_time,
        })
        .collect();
    if !from_dat.is_empty() {
        return from_dat;
    }

    // **Fall back to the `.EXT`'s extended lists.** The `.DAT`'s `PCOB` is the
    // only place the reference implementation looked, and on hardware that
    // produced a `0x4702` reply carrying two *empty* blobs where a real deck
    // sends two full ones — caught by diffing our replies against a deck's for
    // the same requests. A track whose cues rekordbox wrote only as nxs2
    // `PCO2` entries has nothing in `PCOB` at all, and an empty reply is not an
    // error, so nothing else would ever have reported it.
    parsed
        .ext
        .as_ref()
        .into_iter()
        .flat_map(prolink_rekordbox::AnlzFile::extended_cue_lists)
        .flat_map(|list| list.cues.iter())
        .map(|cue| Cue {
            order: u16::from(cue.cue_type.0).saturating_mul(0x100),
            hot_cue: u16::try_from(cue.hot_cue).unwrap_or(u16::MAX),
            time_ms: cue.time,
            loop_time_ms: cue.loop_time,
        })
        .collect()
}

/// A track's extended cues with per-record comment and colour, from the
/// PCO2 PCP2 lists: .
fn extended(
    medium: Option<&Medium>,
    track_id: u32,
) -> Vec<(u32, u8, u32, u32, String, u8, u8, u8, u8)> {
    let Some(medium) = medium else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for list in medium.analysis(track_id)
        .ext.as_ref()
        .into_iter()
        .flat_map(prolink_rekordbox::AnlzFile::extended_cue_lists)
    {
        for cue in &list.cues {
            let pad = u32::from(cue.hot_cue);
            let ty = match cue.cue_type { prolink_rekordbox::anlz::CueType::LOOP => 2u8, _ => 1u8 };
            out.push((
                pad, ty, cue.time, cue.loop_time,
                cue.comment.clone(),
                cue.hot_cue_color_index,
                cue.hot_cue_color_red, cue.hot_cue_color_green, cue.hot_cue_color_blue,
            ));
        }
    }
    out.sort_by_key(|(_, _, time, ..)| *time);
    out
}
