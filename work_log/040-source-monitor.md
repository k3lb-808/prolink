# 40 — The deck's source-monitor report, and an open question that dissolved

TODO item 4: decode the `0x80` and the "ephemeral-port-bound `0x16`s". One
decode landed; the other open question turned out not to exist in the data.

## 0x80 — the source-monitor report (F64)

The old note said "bursts of 14 at 250 ms when it evaluates a source,
`[00][01][41][zeros]`, meaning unknown". Re-measured across all thirteen
captures (`tools/capture-analysis/decode_0x80.py`):

- **The burst shape is confirmed**: fourteen packets, 250.0 ms apart, 3.25 s
  a burst, one burst per source evaluation. A long session re-bursts on each
  re-evaluation — `az_ours.pcap` holds four, 120-200 s apart. Every other
  capture holds exactly one (each capture starts at an evaluation).
- **The body is decoded**: `[00][01][source][zeros]` — byte `0x26` names the
  device number of the source being monitored. `0x29` (41) against
  rekordbox and our server in twelve of thirteen captures; `0x04` in
  `az-load2`, where the server held device 4. So the packet is the deck
  saying *which* source it is watching, not what it found.
- **The burst rides the evaluation window**: the deck's short source query
  (`0x30`), the source's announce batch, and — in the RGB ground truth — the
  media query and the `0x46` waveform-settings ask all land within half a
  second of the burst's packets.
- **Nothing answers it.** rekordbox never did, in any capture.

Implemented: `StatusKind::SOURCE_MONITOR` (0x80), `Packet::SourceMonitor`
(`sender`, `source`), decoded in the responder with one log line per burst —
"the deck is monitoring a source" — instead of fourteen. The corpus test
target gained the check and, with it, its first compile in a while (the
stale match was pre-existing breakage; this also closes that half of TODO
item 8).

## The ephemeral-port `0x16`s — not in the data

The open question read: "the ephemeral-port-bound `0x16`s rekordbox sends
during active browsing (~1.4/s) — replies to something we have not
identified". Re-measured: every `0x16` in both long ground truths goes
`:50002` → `:50002` (rekordbox's source port to the deck's status port), six
of them to the server's own listener during the original capture. No announce in
the archived data is bound to an ephemeral port, and rekordbox announces
nine times in hours, not 1.4/s. The `~1.4/s` figure matches *our* announce
trigger under active browsing (687 announces in `az_wire_new2` over 8.2
minutes). The question is dissolved, not answered.

## Byte 0x87 — mapped the shape, not the meaning

The deck's per-player status byte: player 1 holds `0x02` in every capture;
player 2 holds `0x03` mostly, `0x04` throughout the 3Band ground truth, and
rare `0x01` blips. It does not track the browser-open byte at `0x113` (both
`0x05` and `0x07` screens carry the same values), nor the waveform
preference. Per-player state, meaning still open; documented in
PROTOCOL.md rather than guessed at.

Second pass (`tools/capture-analysis/decode_0x87.py`), no new captures:

- **The blips are evaluation transients.** In `az_wire_new`, both streams
  drop to `0x01` for 0.3-0.9 s and restore — and the drop coincides with a
  burst of fourteen source-monitor reports (first packet 40 ms before, then
  one every 250 ms), with our heartbeat unbroken. So the byte resets while
  the deck re-evaluates a source and is restored per player after.
- **The steady values are per-player constants**: `0x02` (player 1) and
  `0x03` (player 2) in five of six captures with a burst at the start;
  the 3Band ground truth alone holds `0x04` for player 2 the whole capture.
  Ruled out against it: the deck's claimed device numbers (identical across
  the three colour captures), load state, play state, the UI byte, the
  waveform preference.
- The **1152-byte status variant** is the 292-byte layout plus a
  low-band tail (67 non-zero bytes in `az-cue`, no ASCII or UTF-16 strings);
  the same-moment sender-1/sender-2 diff shows per-player regions (a
  `01`-marked bitfield at `0xd9..` where player 1 has marks, per-player
  tempo fields at `0x92`/`0x96`) — player-side state, not shared.
- Our responder now logs each `0x87` move at debug ("the deck's player
  state byte moved"), so the next capture session can correlate transitions
  with deck-side actions directly in the journal.
