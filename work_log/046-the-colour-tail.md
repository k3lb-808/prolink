# 046 — the colour tail, decoded and minted

**2026-10-29. The write record's colour was "the one part nobody has
decoded" (043); the F67 capture decodes it by correlation, and the splicer
mints what the deck assigns.**

## The decode

Two captures, one tuple:

- The live `0x2705` write (043's capture, hot cue A set on deck 2): bytes
  `0x4e..0x50` carry `00 ff 00 17`, two zero bytes of padding after.
- The F67 stick capture (the deck's own `PCP2` entries, 48 bytes): the four
  bytes at offset 44 carry `00 ff 00 17` on hot cue A, `00 ff 8c 00` on the
  loop.

The record's tail and the entry's tail are the **same `(code, r, g, b)`
tuple** — the deck assigns a pad colour at cue creation (A red, loops
orange — the deck's own defaults) and writes it in both places. The code
byte rides `0x00` on the deck's own file entries; rekordbox's *served*
replies use palette codes (`0x24` for loops, `0x2a`/`0x38`/`0x16`/`0x09`
for the hot cycle) — a different convention, and the reply-build keeps the
verified one.

## The change

- `CueWriteRecord::color_tail` (`prolink-proto`): the four bytes at
  `0x4e`, `None` on the memory record's shorter shape (it ends at `0x4c`)
  and on an all-zero tail.
- `write::pcp2_entry_colored` (`prolink-rekordbox`): the compact entry with
  the declared length grown to 48 and the tail at 44 — the deck's own F67
  shape, byte for byte.
- The splicer's `record_values` carry the tail; a fresh mint grows to the
  coloured shape when the record has one. Patched entries keep the base's
  bytes — the deck itself does (F67: only type, position and loop end are
  patched in place).

## Verification

- 706 workspace tests green, clippy silent, fmt clean. The writer test pins
  the coloured shape's lengths and fields; the splice test asserts the
  mints carry the records' tails and the memory mint stays colourless at
  44.
- The cratify side already rides along: its `PCO2` parser now reads tails
  off short entries (its own writer pads to 88+comment — the deck's 48-byte
  shape used to slip through), ingests the colour into the DB, and the
  re-export renders it verbatim.
- Open: whether a deck renders a code-`0x00` tail from a *served reply*
  (the F67 acceptance run answers it; the file side is the deck's own
  shape and needs no guess).
