# 43 — Cue write-back: the deck persists its hot cues into us

TODO item 6, unlocked by one live capture (`/tmp/cue-write2.pcap`,
2026-10-26): a track loaded on deck 2, hot cue A deleted, a new one set.

## The shape (F65)

`0x2705` on the dbserver stream: four arguments — `[descriptor, track id,
op, record length]` — and an **84-byte record**:

- `0x04` the hot-cue number, `1` = A
- `0x06` the cue type, `1` = a point, `2` = a loop
- `0x0a` the standing `1000` the extended-cue records carry
- `0x0c` the position, **milliseconds** — the deck echoed our served
  record's own value back, so the units are the file's
- `0x10` the loop position, `0xffff_ffff` = none
- the rest is the deck's own tail (colour-ish bytes and padding whose
  meaning is not settled), carried verbatim

The operation is the third argument: `1` **sets**, `0` **removes**. The one
live session: the set went out once; the remove went out as **six identical
records of the cue's pre-delete state, ~15 s apart** — a periodic
re-assertion, not a retry loop (15.0/15.0/20.0/15.0/15.0 s gaps, our bare
acknowledgement `SUCCESS[0x2705, 0]` after each, which the deck has been
accepting all along and which stays byte-identical).

## The implementation

A deck writes while *playing* our tree, and a cratify re-export regenerates
that tree, so the writes cannot be folded into the export. They live in a
sidecar beside the medium, `.prolink-cues`, a line log replayed in order
(`S <track> <hot cue> <record hex>` / `R <track> <hot cue>`), written
through on each apply (write-and-rename). The store (`serve::cue_writes`)
loads lazily per medium and survives a restart; a corrupt line is skipped,
because the deck will simply rewrite what it still holds.

The overlay is applied at reply-build time, in both cue replies the deck
reads — `0x4702` memory points and `0x4e02` extended cues — so a reload
shows the deck's cue at its position without touching the analysis files:

- a removed hot cue's record is dropped;
- a set one replaces its base record, keeping the base's comment and
  colour; a brand-new cue lands uncoloured with an empty comment, because
  the write record's colour tail is the one part nobody has decoded (the
  colour polish follows that read, not a guess).

`cratify` does not know the sidecar yet — adopting it into the export
(writing the deck's cues into the next generation) is the follow-up, on the
same pattern as the waveform cache.

## Verification state

Unit tests: the two captured records parse to the decoded fields with the
raw travelling verbatim; the sidecar replays whole into a fresh store; both
overlays drop, replace-in-place (comment and colour kept) and append.
Clippy silent, 590 tests green. **Not yet verified on the deck**: set a cue,
reload the track, and the cue should still be there — and after a server
restart, too. The journal logs each write ("a deck wrote a cue").
