# 39 — Kind-aware discovery, and presenting the source at startup

Two join-fidelity gaps from TODO items 2 and 3, both visible on every
deployment: the startup logs claimed no player could ever find us while the
deck was busy listing us, and we presented ourselves to players only when
they opened a browser.

## The portmapper port is generation-dependent

F46 said a deck retries `GETPORT` on 111 for ever and never falls back —
true of the old decks it was measured on, and worthless as a statement about
the AZ: its decks probe a rekordbox-class source's portmapper on **50111**,
where we have been serving privilege-free since 907b94f, and list us. The
startup warnings (`NfsServer::start`, `VirtualPlayer::start`, the CLI) all
asserted 111 unconditionally, so every rekordbox-kind deployment opened with
two false alarms.

Now: `portmap::AZ_PORT` (50111) beside `portmap::PORT` (111),
`Ports::is_discoverable_for(kind)` decides per generation, and
`VirtualPlayer::is_discoverable()` consults the kind it was started with —
which the FFI status and the CLI warning inherit for free. The `NfsServer`
warn demoted to a `debug!` statement of fact: the server does not know the
kind, and the kind-aware callers warn where the knowledge is.

## The startup announce

rekordbox presents itself: ~0.8 s after its own start, a three-copy `0x16`
announce to each player, and the deck's media query follows the announce
(the join ground truth: batch, then query 0.2 s later). We relied on the
deck re-evaluating our claims — which it does within ~60 ms — and on the
responder's browser-open trigger; both work, neither is what rekordbox does
first.

`VirtualCdj` now spawns the announce for rekordbox-kind servers: 800 ms
after the tasks start, three copies to every peer the discovery table holds
(10 ms apart, the responder's burst shape), logged as "announced the source
at startup". Peers heard later are still covered by the browser-open
trigger. Timing note: our claim chain itself takes seconds, so the announce
lands later relative to our claims than rekordbox's does relative to its
own — but the announce is fidelity, not the listing gate, and the deck has
long since re-evaluated by then.

## Verification

584 lib tests pass, including the new `is_discoverable_for` cases.

**On the deck, 2026-10-26:** restart with the deck live. The journal shows
the whole ground-truth chain: claim settled 08:21:15.8, the deck's source
query answered within milliseconds, the push to both players, the startup
announce at +0.9 s ("announced the source at startup peers=3"), the deck's
media query 0.7 s after the announce, a boot-time `0x46` answered
`color=Some(Rgb)`, and the dbserver connection. No F46 warnings anywhere.
Also now confirmed: the deck asks `0x46` at **every** boot — the natural
path carries the colour; the push is only for mid-session changes.
