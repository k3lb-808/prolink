# 38 — The waveform colour: one byte on the wire, and where it is not

The deck drew 3-band waveforms from our server without ever sending a `0x46`,
so the settings answer looked like dead code — and TODO item 1 asked to pin
which packet really carries the preference before making it configurable.
Answered with a re-diff of the ground truths; the answer is that the `0x47`
answer *is* the carrier, and everything else that looked like a carrier
falsified itself in the data.

## Method

`tools/capture-analysis/colour_diff.py` (in the rekordbox repo) re-diffed the
three differential captures — `rekordbox_3band/blue/rgb.pcapng.gz`, same rig,
same library, one flipped rekordbox preference — across **everything**: every
UDP datagram fingerprinted by (port, kind, length), deduped, then
byte-position-diffed with a within-capture noise mask; and every dbserver TCP
message reassembled and classed by kind. The original one-byte finding was
UDP-only; the TCP waveform replies had never been diffed.

`az_wave_mode.py` then read our own serving captures for the deck's side:
whether it ever asks `0x46`, and what its own status says about the mode it is
in.

## Findings

- **F59 — `0x47` byte `0x32` is the only wire carrier of the waveform
  preference.** The three colour captures differ in exactly that byte (`0x01`
  blue, `0x03` RGB, `0x04` 3Band). Everything else is byte-identical.
- **F60 — the dbserver's waveform payloads do not depend on the preference.**
  The TCP classes are identical across all four captures, lengths included:
  the waveform detail (`0x4a02`) is 29,608 bytes in every one. So there is no
  "colour variant" of the data to serve — blue and RGB render from the same
  three-band data the deck already gets. The byte is a rendering instruction,
  not a data format.
- **F61 — the `0x29` source status is not the channel.** It is absent from
  all three colour captures entirely, and byte-constant (778 identical
  datagrams) in the long ground truth. Its body carries no settings block.
- **F62 — the status bytes that track the preference do so only
  coincidentally.** The deck's `0x0a` byte `0x87` read `04/03/03` across the
  three captures — a correlation — but the long ground truth (3Band) reads
  `0x03` there, and it flips `01/02` with load state. Same for the old
  CDJ-2000's settings-block byte at `0xda` in our own 284-byte status and a
  neighbouring player's `0x06` on 50000: it is a settings echo of *what the
  device holds*, state-dependent, not a readout of the preference. The
  template's `0x04` (907b94f) is the right echo for a source serving 3Band.
- **F63 — the deck asks `0x46` about once per rekordbox session, at boot
  evaluation.** Across all our serving captures it asked exactly once
  (`az_wire_new2`), and never in the others. So the configured colour takes
  effect when the deck happens to ask — which is not reliably — unless the
  answer is pushed unasked.

## What was built

- `WaveformColor` (`prolink-proto::status`): blue / RGB / 3Band with the
  byte mapping; `WaveformSettingsResponse::build` takes it.
- `--waveform-color blue|rgb|3band` (default `3band`) on `prolink serve`,
  threaded `VirtualPlayerConfig` → `VirtualCdjConfig`, driving the `0x47`
  answer and the settings byte of our own player status.
- `--push-waveform-color`: an experiment, off by default. Unicasts the `0x47`
  answer to each player heard from, once, shaped exactly like a reply (the
  deck's queries leave from ephemeral ports; answers go to `:50002`). If the
  deck accepts it, the colour takes effect without a fresh boot on its side.
  Unproven — F63 says the deck otherwise only reads the colour when it asks.

## Verification state

Golden test: the 3Band answer matches the ground-truth `0x47` byte for byte.
Unit tests: per-colour byte, no other byte differs, the status echo.

**Verified on the deck, 2026-10-26:**

- With `--waveform-color blue`, the deck asked `0x46` during boot evaluation
  (08:01:39, 0.2 s after the media query) and drew non-3Band waveforms — the
  byte takes effect through the natural path (F63's once-per-session ask).
- With `--waveform-color rgb --push-waveform-color`, a plain restart of the
  server (no deck reboot): the deck did **not** re-ask, and the unasked push
  reached both players 9 s after our claims — and **the deck accepted it**:
  the rendering changed to the deck's RGB look. So F63's open question is
  answered: a deck takes an unasked `0x47`, and the colour can be changed
  live. (Whether a production server *should* push — rekordbox never does —
  is a taste decision; off by default.)
- The RGB look itself: "mostly red in both windows". The data is not at
  fault — our served three-band detail is band-balanced (dominance 46/42/42
  vs ground truth's 44/35/34) and every preference-dependent carrier was
  re-checked byte-wise: the analysis-tag replies (PWV4/PWV5 colour data) are
  md5-identical across all three ground-truth preferences. The RGB rendering
  semantics are entirely deck-side, and the ground-truth sessions left no
  visual reference of what real rekordbox's RGB looked like on this deck.
  So: the byte is correct end-to-end; whether the deck's RGB look is
  *wanted* is the operator's eye, not a protocol question.

## The push, afterwards: removed as redundant

The push shipped as an experiment and earned its keep once: it answered
whether a deck takes an unasked `0x47` (it does — see the verification
above). But the boot-time ask turned out to be the rule, not the exception
(F63 refined: the deck asks `0x46` at **every** boot, 0.2 s after the media
query), so the natural path carries the colour and the push was the one
packet in the serve path that real rekordbox never sends. Removed
(`--push-waveform-color` gone from the CLI, the configs and the responder);
the finding stays here and in PROTOCOL.md. A colour change takes effect on
the deck's next boot.
