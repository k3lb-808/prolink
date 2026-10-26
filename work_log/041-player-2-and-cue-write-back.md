# 41 — Player 2, and a cue write-back that has nothing to decode

TODO items 5 and 6. One closed by audit plus a hands-on session; the other
turned out to have no evidence to build from.

## Player 2 (item 5)

The AZ is one unit with two players; only player 1's flow had ever been
exercised end-to-end. Audit first: every path that could have baked in
sender 1 is in fact keyed by the peer's own device number —
`observe_loaded`/`track_on` (the row marking, F55), the browser-open
announce trigger (per-player map), the waveform push (fired for player 2 in
its first live session), and the source-monitor logging. The marking
mechanism already had a unit test with device 2 as the peer. No sender-1
assumptions existed; nothing to fix.

The wire agreed that the path was untested: across all our captures, only
sender 1 ever reported a track loaded from us (`az_wire_new2`: 228 load
packets, all sender 1; zero sender 2). A hands-on session then confirmed
player 2 browses, loads and plays from us.

New observability to make loads visible at all: `observe_loaded` now
returns the load when a status packet states a (player, slot, track) triple
that is new, and the responder logs it — "a player loaded a track from us
player=N". Until now a load left no journal trace; the row marking it feeds
happened silently.

Residual: one load on player 2 with the new binary, to read the line.

## Cue write-back (item 6): no ground truth exists

`0x2705` (the cue write pdl.py knows the name of) appears in **zero** of the
thirty-three corpus captures and zero of the archived AZ/rekordbox
captures — neither deck ever wrote a cue in anything we hold, and prolink
has no code for it at all. Implementing would mean inventing bytes, which
this project does not do. The item is evidence-blocked: it needs one
capture of the deck writing a cue to us (track from our source loaded, a
hot cue or memory cue set on the deck, tcpdump running), and then the
decode-plus-implementation follows the usual path.
