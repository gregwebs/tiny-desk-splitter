# Availability-first Concert Split publication with a durable recovery journal

Status: Accepted

Concert Split publication (issue
[#139](https://github.com/gregwebs/tiny-desk-splitter/issues/139), tickets
[#142](https://github.com/gregwebs/tiny-desk-splitter/issues/142)–[#144](https://github.com/gregwebs/tiny-desk-splitter/issues/144))
copies staged split output to canonical filenames rather than making that
publish step filesystem-atomic. We accept a small window where a crash can
leave canonical files in a mixed state, in exchange for one retained backup
generation, salvage of playable songs from a failed first attempt
(Recoverable Partial Split), and a durable on-disk journal that makes any
interrupted publication explicit and recoverable before `concert-web` ever
serves a request. See
[`docs/concert-split.md`](../concert-split.md#published-and-recoverable-partial-output)
for the full publish/rollback mechanism and state diagrams, and
[`CONTEXT.md`](../../CONTEXT.md#language) for the canonical definitions of
Concert Split, Published Concert Split, and Recoverable Partial Split.

## Alternatives considered

- **Filesystem-atomic directory swap** (stage a whole new concert directory,
  then `rename()` it over the canonical one). This would remove the mixed-state
  window entirely, but canonical output lives in `workdir/concerts/<concert>`
  alongside files the splitter does not own — source media, `concert.json`,
  preview images, notes. A directory-level swap cannot exclude those without
  reintroducing per-file logic, and it conflicts with the on-disk data model
  (`docs/data.md`) that expects one stable, long-lived concert directory rather
  than periodic full replacement. Cross-filesystem or NAS-mounted `workdir`s
  also cannot guarantee `rename()` atomicity across the swap.
- **Accumulating backup generations.** Simpler to implement, but concert
  directories already carry per-track media files; unbounded backups turn a
  disk-space concern into an operational one for no corresponding benefit —
  one prior known-good generation is enough to survive a failed resplit or
  recovery rollback.
- **Best-effort publish with no journal.** The pre-existing design (before
  #142–#144) copied files into place without recording *what it intended to
  do*. A crash mid-copy left no record of which files were the intended
  replacement set, which were obsolete, or what the prior state was — an
  operator could not distinguish "still publishing" from "silently broken."

## Decision

Publication stages complete output beside the canonical directory, validates
it, and — under an advisory exclusive lock — copies the current canonical set
to one retained backup, atomically installs a journal describing the exact
intended replacement, then renames staged files into place and installs an
exact manifest (`.concert-split-published.json`) naming the files that belong
to the Published Concert Split. The journal, not directory scanning, is the
source of truth for both what to publish and what to roll back — obsolete
cleanup and recovery only ever touch files the journal names, so unrelated
concert files are never at risk.

If a process or host crash interrupts publication, `concert_split::run`
resolves any pending journal for its concert directory *before* validating
media for a new split, and `concert-web` calls
`recover_split_publications_before_startup` before opening the database,
constructing the Job Registry, or binding — so a pending recovery blocks
startup rather than serving inconsistent output. Recovery retries finishing
the interrupted publication up to three times across invocations (each attempt
persists its count before retrying, so attempts survive further crashes);
after the third failure it restores the exact prior canonical state — Empty,
Published-from-backup, or Recoverable-Partial-from-snapshot — from the
journal. If restoration itself fails, the journal is retained and an explicit
unrecoverable error is raised rather than silently discarding evidence.

## Consequences

- A crash between the canonical rename and journal cleanup can briefly expose
  a mix of old and new canonical files on disk. This is accepted: the journal
  makes that window explicit, bounded (at most three recovery attempts before
  fallback), and self-healing on the next `concert_split::run` or
  `concert-web` startup, rather than requiring filesystem-atomic guarantees
  the target filesystems don't reliably provide.
- Recovery is fail-closed: `concert-web` will not bind or serve while a
  pending or unrecoverable publication journal exists for any concert
  directory. This trades availability of the whole application for guaranteed
  consistency of what it does serve.
- Only one backup generation and one Recoverable Partial Split generation are
  retained per concert; older generations are not recoverable once superseded
  by a later successful publish.
- This decision does not change the CLI vs. library adapter cancellation
  behavior (see `docs/concert-split.md#cancellation-semantics`) — publication
  and recovery are shared by both adapters because both call the same
  `publication` module.
