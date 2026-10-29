# prolink TODO — self-contained stick reconciliation (sidecar removal)

prolink must not require cratify: when a deck writes cues on a served stick,
the stick itself becomes self-contained. The deck's edits are absorbed into
the tree's own PIONEER ANLZ files, so a stick pulled after a session can be
plugged directly into a player and carries the edited cues. No sidecar, no
python, no second project in the loop.

## Status

- **Done (2026-10-28).** The splicer, the dedup, the triggers and the
  sidecar removal are implemented and green (841 tests, clippy silent):
  `serve/splice.rs` merges the overlay into the tree's ANLZ files and the
  sidecar no longer exists anywhere in the code.
- The splice **target was settled by hardware** (F67): a stick plugged
  directly into the XDJ-AZ gets its cue edits written back into that track's
  `ANLZ0000.DAT`/`.EXT` — and *only* those files. `export.pdb` and
  `exportLibrary.db` keep their bytes across a session of cue edits, and a
  deck displays cues read from the ANLZ even when the databases carry none.
  The ANLZ is the cue surface in both directions; see
  `docs/PROTOCOL.md` §7.2 and `work_log/045-the-splice.md`.
- Two corrections to the original plan, learned on the way:
  - The write-ack's refreshed cue sections are the dbserver **wire** format
    (little-endian, 136-byte entries) and cannot be pasted into ANLZ files
    (big-endian, 56-byte `PCPT`/44-byte `PCP2`). The splicer needed a real
    ANLZ encoder, which `prolink-rekordbox::anlz::write` now is.
  - No restore-from-ANLZ is needed at all: once the files are spliced they
    *are* the truth, so the overlay starts empty every restart and the
    in-memory `TrackOverlay` stays a pure session delta. (And the planned
    manual `reconcile` command is pointless for the same reason: there is no
    persisted state for it to reconcile — a crash loses exactly what the
    deck re-asserts.)
- Live serving of deck edits is done and hardware-verified (F65/F66): the
  write-back (`0x2705`) is decoded, the runtime overlay serves refreshed
  cue lists on the write's own transaction.
- The small hard-kill window (process killed mid-set before the splice
  lands) is accepted — the deck re-asserts each state about every 15 s and
  recaptures anything lost.

## Remaining

- **F67 hardware acceptance**: serve a physical stick, edit cues on the
  deck, stop prolink, plug the stick into the XDJ's USB port directly, boot:
  hot cues, memory cues, loops and colours all present. The capture evidence
  (the deck reads its own spliced-style output) says it will be; run it.
- Optional: phrase-marker conventions for deck-written cues that sit on
  phrase starts (rekordbox writes `comment` + colour there; the deck does
  not).
- Optional hardening for the cratify round trip: cratify's export settles
  each bundle against the sha256 it ingested before overwriting it, and
  leaves a still-changing bundle alone — so a deck edit landing mid-export
  survives (and the deck's 15 s re-assertion covers the residual sliver
  while the track stays loaded). To close even that sliver provably, both
  sides could take an advisory `flock` on a lock file beside the tree:
  prolink around each splice, cratify around settle+render. Contract item
  for PROTOCOL.md if it ever matters; the re-assertion makes it theoretical.

## Landed since

- **The colour tail is decoded, and the mints carry it.** The write
  record's `0x4e..0x50` holds `(code, r, g, b)` — the byte-identical tuple
  the deck's own stick write (F67) puts at a `PCP2` entry's offset 44: hot
  cue A red `00 ff 00 17`, a loop orange `00 ff 8c 00` (the deck's pad
  defaults). `CueWriteRecord::color_tail` reads it (`None` on the memory
  record's shorter shape and on all-zero tails), and the splicer's fresh
  mints grow to the deck's own 48-byte shape with the tail at 44 — and the
  patch takes the record's colour too: a deck write over an existing entry
  lands in the colour the deck just assigned (its pad sequence), an entry
  whose record carries no colour keeps its own. cratify's ingest
  (its PCO2 parser now reads tails off short entries too) takes the colour
  into the DB and the re-export renders it verbatim: a colour the deck
  assigned survives the whole round trip. The served *reply* still uses the
  verified shapes (loops `0x24 ff 8c 00`, the palette cycle for colourless
  hot cues) — whether a deck renders a code-`0x00` tail from a reply is
  part of the open F67 acceptance question.
