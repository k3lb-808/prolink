# 42 — Behaviour tests for the responder's decisions

TODO item 7. The packet shapes were already golden (`0x16` byte-for-byte,
the `0x47` answer against ground truth); what had no tests was what the
responder *does*. Two of its three stateful decisions are now extracted as
pure functions and tested directly; the third was already covered.

- **The browser-open announce trigger** → `announce_due(browsing, player,
  ui, now)`. Tested: the idle home screen earns nothing; the edge into
  `0x05` fires; a browser held open does not re-fire inside the retry
  window; leaving and re-entering is a fresh edge; a browser held open
  re-fires after five seconds counted from the last edge; and a second
  player's browser state is independent.
- **The short source query's answer** → `short_source_answers(name, ours)`.
  Tested: three identical announces then the short response, in the boot
  capture's order, both carrying our identity as the sender. The responder's
  send loop now walks this list, so the order lives in the tested function
  rather than in the loop.
- **The claim gating** (`browsable_number` → `BrowsableDeviceNumber`) was
  already tested at the proto level (device.rs: 4 and below browsable, 5 is
  not, browsable numbers enumerate highest first); the `VirtualCdj` wrapper
  is a passthrough.

A full-stack loopback harness (spawn the `VirtualCdj`, send datagrams, read
the replies) was considered and rejected: the sockets pin themselves with
`SO_BINDTODEVICE`, which needs privileges a test does not have. The
functions are the honest seam.

Housekeeping along the way: the load log from 041, the source-monitor arm,
and the waveform push each had a lint the crate's own settings catch
(`expect_used`, `semicolon_if_nothing_returned`, `map_or` → `is_none_or`);
fixed. The one remaining warning in `virtual_cdj.rs` is the responder's
`too_many_lines`, which predates this session (159 lines at 959f943).
