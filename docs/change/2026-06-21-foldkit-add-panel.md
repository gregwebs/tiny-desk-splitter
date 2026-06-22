# Foldkit port #2: the add-to-playlist panel

Second component ported to [Foldkit](https://foldkit.dev) (Effect-TS MVU), after the
splitter (`docs/change/2026-06-19-foldkit-eval.md`). Replaces the most async/DOM-sync-fragile
piece of the old imperative `playlists.ts` — the add-to-playlist sidebar panel — with a Foldkit
MVU widget mounted via `Runtime.embed`. The `/playlists` list page, the `/playlists/:id` detail
page, and the HTML5 drag-reorder stay imperative.

## What moved where

`src/playlists.ts` (809 lines) became a directory:

- `src/playlists/pages.ts` — the imperative list/detail/drag-reorder handlers, moved verbatim
  (own commit, reviewable before the bundle minified).
- `src/playlists/core.ts` — **new** pure, DOM-free logic: row ordering (`buildRows`), the
  typing auto-highlight (`autoHighlight`), arrow-key movement (`nextRow`/`prevRow`),
  `targetLabel`/`addItemBody`, `isMember`/`itemIdFor`. Unit-tested directly
  (`js-tests/playlists-core.test.ts`, 16 cases).
- `src/playlists/widget/` — the MVU widget (`model`, `message`, `update`, `view`, `command`,
  `port`, `subscription`, `widget`, `index`), mirroring `splitter/widget/`.
- `src/playlists/index.ts` — host glue: assembles `window.Playlists`, mounts the widget,
  bridges its Ports to `window.Player` / `#player-sidebar` / `prompt()`.

## The three MVU wins (as predicted in the feasibility report)

- The `addPanelToken` monotonic counter (stale-fetch guard) → the **staleness rule**: every
  `Completed*`/`Failed*` Message carries the `forTarget` it was dispatched for and is ignored
  unless it still equals the current `Loaded.target`.
- The `MutationObserver` on `document.body`'s class list (external sidebar-close detection) →
  an **inbound `closed` Port** the host pushes. (A small host-side observer remains — see
  Ports below — because `player.ts`, which owns `sidebar-open`, is out of scope.)
- The manual `applyActiveHighlight()` DOM walk (syncing `aria-selected`/`aria-activedescendant`)
  → a pure `view` over the Model. The highlight is `buildRows` + `activeId`, not a post-render
  pass.

## Mount lifecycle (differs from the splitter)

The splitter mounts/disposes a fresh widget per toggle. The add panel lives in the
page-lifetime `#player-sidebar` (a sibling of `#content`, so it survives hx-boost swaps), so its
widget is mounted **once** on the first `openAdd` into a dedicated `<div id="add-pl-widget-root">`
(template) and kept for the page lifetime. Open/close flows over Ports:

- inbound `opened: AddTarget` (host's `openAdd`), `closed: void` (external sidebar close),
  `newName: string` (empty-state `prompt()` result).
- outbound `requestClose: void` (the widget's "×" / Enter-on-empty-filter → host tears down the
  sidebar chrome), `requestNewName: void` (empty-state → host `prompt()` → inbound `newName`).

`openAdd` re-captures `sidebarWasOpen` (host-owned) **before** opening the sidebar, every call,
so closing restores the prior state. The host keeps a small `MutationObserver` that fires
`closed` only when `sidebar-open` is removed while `showing-add` is present (covers
`Player.toggleSidebar()`).

## Two findings worth recording

**1. Don't encode per-row behavior in the click handler's identity — decide in `update`.**
The biggest bug found during the spike: clicking the "Create '<name>'" row fired the
*empty-state* row's `prompt()` instead of creating the playlist. Root cause: the empty-state row
and the create row are different row *kinds* rendered at the same list position, and the vdom
reused the `<li>` across the kind change but did **not** refresh its click handler — leaving a
stale `ClickedEmptyCreate` (→ `prompt()` → `null` in headless → nothing). Distinct snabbdom
keys did **not** fix it. The robust fix is idiomatic MVU: every row dispatches one
`ClickedRow({ id })` Message, and `update` interprets it against the row's *current* kind
(`buildRows`). A reused element can't act on a stale decision because the decision lives in
`update`, keyed only by the stable row id. (Rows are additionally keyed by `kind-id` so
reorders stay clean.)

**2. The MVU render is asynchronous; some e2e assertions had to wait for it.**
The old imperative code re-rendered synchronously inside the input/keydown handler; Foldkit
defers renders (rAF / MessageChannel). A handful of `add-to-playlist*` assertions read the DOM
immediately after a `fill`/`press` and so raced the render. They were changed from one-shot
reads to Playwright auto-retrying assertions (`toHaveText`, `toHaveCount`, `toHaveClass`) — same
behavior asserted, just tolerant of the async render. No widget behavior changed.

## Build

`playlists` moved from the reviewable es2020 bundle to the minified es2022 Foldkit bundle
(`build.mjs`), joining `splitter`; `player.js` is now the lone unminified, `ts-verify`-guarded
bundle (`justfile`). The still-imperative `pages.ts` rides along in the minified `playlists.js`
— the same tradeoff the splitter's host glue already made. No Rust changes (`include_str!` and
the route are unchanged).

## Verification

- `just lint` clean (fmt, clippy, tsc ×2, `ts-verify`); `node --test js-tests/*` — 32 unit
  tests (12 splitter + 20 add-panel core).
- Playwright: `add-to-playlist.spec.js`, `add-to-playlist-ordering.spec.js`,
  `sidebar-close-resize.spec.js`, `sidebar.spec.js`, `playlists.spec.js` all green (membership
  ✓/trash, click-to-add, filter, arrow-key nav, exact-name + Enter clears filter, create-and-add,
  empty-state `prompt()` bridge, external sidebar-close reset, `sidebarWasOpen` restore).
- The two MVU-specific behaviours added explicit specs (`add-to-playlist-ordering.spec.js`): the
  **staleness rule** — a deliberately delayed membership fetch for a superseded target is dropped
  rather than clobbering the current target (the scenario the old `addPanelToken` counter
  existed for) — and the **Enter-toggles-member / click-is-no-op asymmetry** on a member row.
- The full e2e suite has 8 failures that are **pre-existing on `origin/main`** (verified on a
  clean worktree): `openapi` (page-navigation), `automate-splitting`, `concert-reconstruction`,
  `interlude-tracks`, `sync-player-persistence` — none touch the add-to-playlist panel.
