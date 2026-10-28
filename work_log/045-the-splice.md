// SPDX-License-Identifier: GPL-3.0-only

# 045 — the splice: cue writes land in the tree's own files

## The experiment that settled the target (F67, hardware)

The plan for making a served stick self-contained had an open question: when a
deck edits cues on a stick it holds *directly* — no network, no prolink — which
files does its firmware write? Community lore said the databases; nothing here
had ever pinned it, and the whole splice design depended on the answer.

So: a cratify-exported stick (the "k3lb" volume, `property` row `CRATIFY`),
mounted, hashes taken, plugged into the XDJ-AZ's USB port, cues deleted, then
remounted. Then again with cues set: one hot cue, one beat loop, one memory
cue.

The result, from file mtimes and sha256 sums:

- The deck rewrote **only** the edited track's `ANLZ0000.DAT` and
  `ANLZ0000.EXT` (mtime moved to the session's minute; every other ANLZ bundle
  on the 1,148-bundle stick kept its export mtime; the `.2EX` untouched).
- `export.pdb`, `exportLibrary.db`, `exportExt.pdb`: byte-identical across
  both sessions. The XDJ-AZ is a OneLibrary device — its own library browsing
  reads `exportLibrary.db` — and it still recorded cue edits in the ANLZ
  files alone.
- The display evidence closed the loop: the track's databases carried **zero
  cue rows** the whole time, yet the deck showed cues — read from the ANLZ
  `PCOB` lists cratify had written.

So the ANLZ is the cue surface in *both* directions on this firmware, and the
splice target was never in doubt. The deck's byte shapes were captured whole
(they are the F67 test vectors in `serve/splice.rs`):

- `PCPT` entries, 56 bytes: hot number, status 0, sort words `0xffff`, type,
  `0x00 0x03 0xe8`, position, loop end, a 16-byte `0xff` tail.
- `PCP2` entries, the compact 44 bytes: no comment, no colour. A hot loop
  grows to 48 with a colour tail — orange `00 ff 8c 00`, the same orange the
  wire rules serve (F66) — and carries its length in beats, 8/1 for the
  session's loop, counted off the grid.
- The `.EXT`'s `PCOB` lists sit empty after an edit: tags kept, emptied. The
  deck normalises; so does the splicer.
- Every list header's trailing word varies (`0` on the memory list,
  `0xffff_ffff` on the hot one); nothing reads it; the splicer preserves
  whatever the base carried rather than inventing a value.
- The writes landed at one flush moment — one mtime for all three edits —
  so the deck batches its own writes, as the splice's quiesce does.

## What was built

- **`prolink-rekordbox::anlz::write`** — the workspace's first ANLZ *writer*:
  `pcpt_entry`/`pcp2_entry` mint the deck's compact shapes;
  `pcob_tag`/`pco2_tag` build whole list tags from a preserved list header
  plus an entries span, patching only the declared count;
  `fresh_list_header` for tags a file does not carry;
  `AnlzFile::walked_whole` to refuse rendering a file whose parse stopped
  early. Every offset pinned against the F67 captures.
- **`prolink::serve::splice`** — the merge. Per track, per file: parse fresh
  from disk (never the served parse), merge the `TrackOverlay`
  (slot-keyed hot upserts and removals, position-keyed memory sets and
  clears) into the cue tags, re-render the container with a recomputed file
  length, re-parse the render as a guard, then temp-and-rename. The deck's
  conventions mirrored: `.DAT` `PCOB` lists carry the cues; an `.EXT`'s
  `PCOB` lists empty out and its `PCO2` lists carry everything, cues from
  the `PCOB` twin migrating as the compact entries they can be expressed as.
  A base entry the overlay replaces is **patched in place** — type, position,
  loop end only — so a rekordbox comment or colour tail survives every
  re-assertion; fresh entries are the compact colourless shape, which is all
  the write record carries.
- **Triggers** — a spawned task polls once a second (a quarter of the
  quiesce, floored and capped) and splices whatever has been quiet for five
  seconds; `DbServer`'s `Drop` splices everything still dirty before its
  tasks die. A splice that fails loses only its dirty mark: the deck
  re-asserts every held state about every 15 s, so the next pass captures
  it again. `DbServerConfig::splice_quiesce` exists for tests and nothing
  else.
- **Dedup** — `CueWrites::apply` returns whether anything changed, and only
  a change marks a medium dirty. The deck's re-assertion loop — a dozen
  writes a minute per loaded track — now costs nothing but a boolean.
- **The sidecar is gone.** No append path, no replay, no `.prolink-cues`.
  The overlay is deliberately memory-only: the spliced files are the
  persistence, and what a hard kill loses is what the deck will re-assert.
  An older prolink's sidecar files are simply ignored.
- **Cache invalidation** — `Medium::invalidate_analysis` drops a track's
  parse after its splice, so wire replies are built from the spliced bytes
  with the overlay re-applied over them (the same keys, so it is idempotent).
  NFS file reads were already disk reads (`Node::Disk`), so the served tree
  shows spliced bytes with no work at all — the "asymmetry" the TODO planned
  to document turned out not to exist.

## Verification

- 841 workspace tests, clippy silent, fmt clean.
- The splice tests pin the deck's own bytes: a fresh splice of an empty
  cratify-style tree produces the captured `PCOB` tags byte for byte; the
  captured files themselves splice to *nothing* under the session's own
  overlay (idempotence by construction); removals, memory clears, comment
  and colour preservation, the not-whole refusal and the `PCO2`-migration
  path all have their own tests.
- The trigger tests run a real server over a temp volume with a 50 ms
  quiesce: a write, the quiesce, the file rewritten, the parse cache
  invalidated; and a write no quiesce would catch, landed by the drop.
- Still to come: the hardware acceptance run — serve a physical stick, edit,
  stop prolink, plug the stick into the XDJ's USB port directly: everything
  present. The capture evidence says it will be; the stick that produced
  F67's vectors is the proof the deck reads what it writes.
