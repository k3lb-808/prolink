# 044 — loops and memory cues (F66)

**2026-10-27/28. The deck edits cues on our serve and they survive reloads,
restarts and power cycles. The last blockers were the ack's request kind and
an orange pad.**

This session finished the cue write-back (TODO item 6). It began with
memory cues that would not render, went through a stalled fresh boot, a
rekordbox ground-truth capture that rewrote the ack contract, and ended on
a colour byte that hid every loop we had served correctly all along.

## The memory-cue model

- A memory cue travels with cue number `0` and **no identity but its
  position**. Each set at a fresh position adds another cue; a remove
  clears exactly its own position. The earlier "one moved slot" reading
  came from the bare-ack sessions, where the deck's re-assertion loop only
  ever followed the latest placement — its unconfirmed-write loop, not a
  move.
- The sidecar's `R` lines now carry the position (older lines replay with
  the clear simply unrepresented; the deck re-asserts nothing for a cue it
  dropped, so nothing resurfaces that matters).

## The write ack is the list

The rekordbox capture (`captures/rekordbox_memory_cue.pcapng`) settled
what a write-back source actually replies to `0x2705`: **the refreshed
`0x4e02` list on the write's own transaction — no bare success at all**,
for hot and memory writes alike, including the 15 s re-assertions. Our
first attempts (a bare ack; an unsolicited push on our own transaction)
left the deck holding the cue but never drawing it.

Two more subtleties:

- **The reply's request kind is the write's own (`0x2705`), not the
  fetch's (`0x2b04`)**. With the fetch kind the write confirmed — the
  re-assertions stopped — but the placed cue never reached the deck's
  per-track cache, so *every reload drew the stale list*. This was the
  loop that would not survive a reload.
- **The deck never re-fetches a track's cue list within a session** (one
  fetch per track per power cycle; the AZ's two players share the device
  cache). Reloads ride the cache, so the cache is everything. Also:
  a *fresh boot* with two rekordbox-class sources present (us + live
  rekordbox) stalls the deck's browser for minutes — the deck withholds
  its own announcements and never asks for the dbserver port. Boot with
  one source; adding the second afterwards is fine.

## The loop record

rekordbox's long loop entries carry, where a point carries zero: `0x04`
at `0x18`, `0x06` at `0x3a`, the loop's length in beats at `0x42`
(numerator, little-endian) with denominator `1` at `0x44`. The beats we
compute from the track's beat grid (`loop_beats`). The `0x20`/`0x22`
pair on a hot entry is its **linked memory cue's index and id** — correct
to leave zero for our unlinked section cues.

**And the punchline: the loops rendered all along — in the wrong
colour.** A loop's pad is orange, always: rekordbox serves its loops
`[0x24][255][140][0]`, and our section/palette colours on a loop read as
a plain cue. Every "not a loop" report of this session was an orange
marker hiding among coloured cue tags.

## The fresh-boot quirk

The deck's boot-time source evaluation runs once per power cycle. With
two rekordbox-class sources announcing, the deck claims its numbers and
then sits: no announcements of its own, no dbserver-port portmap ask, a
"loading" browser for minutes (it does eventually recover). Boot with one
source present.

## Verification

The loop round-trips: set on deck 1 → the write-ack serves it → the cache
carries it → reload and deck 2 show it. Sidecar replays across server
restarts and deck power cycles (the overlay of the memory cues survived
every one today). Hot-cue deletes, moves and the delete-all all replay
correctly.

Residuals: the cratify sidecar adoption (the next re-export should carry
the deck's cues into the export itself), and the comment-editing gap (an
edited cue comment reverts on reload — the overlay keeps the base
comment).
