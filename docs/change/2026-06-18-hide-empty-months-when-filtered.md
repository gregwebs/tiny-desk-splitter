# Hide empty months on the index page when a filter is active

## Motivation

The index page lists concerts grouped under month dividers. `build_month_items`
walks from the current month down to the earliest concert's month and emitted a
divider for *every* month in that range, appending rows only when a month had
matching concerts. This is intentional for the default (unfiltered) view — it
renders a continuous timeline with per-month "Sync" buttons so the user can
fetch months that haven't been scraped yet.

But when a filter pill (Wanted, Archived, etc.) was active, every empty month
still rendered its header, producing a long list of bare month labels with no
cards underneath.

## What changed

`build_month_items` (`concert-tracker/src/month_walk.rs`) gained a
`hide_empty_months: bool` parameter. In the walk loop, a month's rows are now
removed from the map first; the divider (and Sync button) is only pushed when
`!hide_empty_months || has_rows`. The "Unknown date" section was already gated
on a non-empty `no_date_rows`, so it needed no change.

The `list` handler (`concert-tracker/src/web/handlers.rs`) passes
`!filter.is_empty()` — so the default view (`filter == ""`) is unaffected and
keeps the full continuous timeline with Sync buttons; any filter pill hides
months with zero matches, including the current month if it has none.

A filter that matches nothing now yields an empty `rows` list. `list.html`
gained an explicit empty state (`<p class="empty-state">No matching
concerts.</p>`) inside `#concert-list` so this renders as a deliberate message
rather than a blank area; a matching `.empty-state` rule was added to
`layout.html`.

## Verification

- 5 new unit tests in `month_walk.rs` covering the filtered path: empty months
  skipped, non-empty months kept, current month omitted when empty, "Unknown
  date" still appended. Existing tests updated to pass `hide_empty_months:
  false`, preserving the original always-emit behavior for the default view.
- Manual: ran `concert-web` against a scratch copy of `concerts.db` (separate
  `--db`/`--workdir`, real db never touched). Confirmed via direct HTML
  inspection and Playwright screenshots that the default view still shows the
  full month timeline, the Archived filter jumps straight from a month with
  archived concerts to the next one that has any (skipping empty months in
  between), and a filter with zero matches shows the "No matching concerts"
  message instead of a blank list.

## Files changed

- `concert-tracker/src/month_walk.rs` — `build_month_items` gains
  `hide_empty_months`; 5 new tests, existing tests updated
- `concert-tracker/src/web/handlers.rs` — `list` handler passes
  `!filter.is_empty()`
- `concert-tracker/templates/list.html` — empty-state message when `rows` is
  empty
- `concert-tracker/templates/layout.html` — `.empty-state` style
