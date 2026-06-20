# Foldkit feasibility evaluation

## Summary

[foldkit](https://foldkit.dev) is a pre-1.0 TypeScript framework that implements The Elm
Architecture (MVU) **entirely on top of Effect-TS**. It is not a library that can be added
incrementally to the existing imperative `player.ts`/`playlists.ts`/`splitter/` code — it
replaces that style outright. Adopting it is realistically one of two moves:

1. **Embed** a single foldkit widget (via `Runtime.embed`) inside the current htmx/askama
   shell for one isolated piece of UI, or
2. **Rewrite** the frontend as a full client-rendered SPA, which also requires building JSON
   endpoints for the many pages that today are htmx-swapped HTML fragments with no JSON
   equivalent.

**Recommendation: not now.** The architectural win is real for the genuinely stateful pieces
(player queue, split-timeline editor) — MVU eliminates exactly the class of bug the playlists
add-panel is built to avoid (stale-fetch races, manual aria/DOM sync, ad hoc module-global
state). But the cost is an Effect-TS learning curve, a second build toolchain (Vite) running
alongside the current esbuild→`include_str!` pipeline, loss of the project's
committed-unminified-reviewable-JS invariant, and pre-1.0 churn — for a payoff that, on the
current evidence, is concentrated in two of five interactive areas. If we revisit this, the
right first move is a `Runtime.embed` spike of the **splitter** (see [Suggested next step](#suggested-next-step)),
not playlists or the player.

## Where we are today

`docs/change/2026-06-19-frontend-typescript.md` describes the current setup: most pages
(list, detail, jobs, settings, playlist pages) are **askama-rendered HTML swapped by htmx**
(`hx-boost`, `hx-get`/`hx-post` on `concert_card.html`, `jobs.html`, `layout.html`, etc. —
see `concert-tracker/templates/`). Only three areas carry real client-side TypeScript, each
an IIFE bundle attached to a `window.*` global so they can talk to each other across
`hx-boost` swaps without ES module imports:

- `concert-tracker/frontend/src/player.ts` (2124 lines) — `window.Player`: playback queue,
  auto-advance, prepare/download polling, the single shared `<video>` element, persists
  across navigation.
- `concert-tracker/frontend/src/playlists.ts` (809 lines) — `window.Playlists`: playlist
  CRUD, drag/drop reorder, and the cross-cutting "add to playlist" sidebar panel.
- `concert-tracker/frontend/src/splitter/{core.ts,index.ts}` (221 + 588 lines) —
  `window.Splitter`: the split-timeline drag editor. `core.ts` is notably **pure and
  DOM-free**.

These are typed against a generated OpenAPI client
(`concert-tracker/frontend/src/generated/openapi.d.ts` +
`concert-tracker/frontend/src/api/client.ts`), built by esbuild into committed, unminified
IIFEs in
`concert-tracker/static/*.js`, which the Rust binary embeds via `include_str!`
(`concert-tracker/src/web/handlers.rs`) and serves from disk only in `--dev`. `just ts-verify`
rebuilds and diffs those committed files in CI/pre-push — deliberately making the shipped JS
reviewable and reproducible without a Node runtime at `cargo build` time
(`concert-tracker/frontend/build.mjs`, `justfile`).

## What foldkit is

Confirmed against real source (`github.com/foldkit/foldkit`, examples `counter` and
`embedding` — not just the marketing site, which is light on code):

- One immutable `Model` defined as an Effect `Schema.Struct`.
- `Message`s built with `m('Tag')` / `m('Tag', { field: S.Type })`, combined into a
  `Schema.Union`.
- A single `update(model, message)` using `Match.tagsExhaustive`, returning
  `[Model, Command[]]`. Immutable field updates go through `evo(model, { field: () => v })`.
- Side effects are `Command.define('Name', paramsSchema, ResultMessage)(params => Effect...)`
  — Commands are *returned*, not invoked imperatively; the runtime executes the Effect and
  feeds the result back in as a Message.
- Views are hyperscript, not JSX: `const h = html<Message>()`, then
  `h.div([h.Class('...')], [...children])`, `h.button([h.OnClick(Msg())], ['+'])`. A small
  `@foldkit/ui` package wraps common patterns (e.g. `Button.view`).
- `Port.inbound`/`Port.outbound` + `Subscription.make` are the typed boundary for talking to
  a host page or external streams (ticks, WebSockets).
- `Runtime.makeApplication` + `Runtime.run` mounts a full top-level app;
  `Runtime.makeElement` + `Runtime.embed` mounts a self-contained widget into an existing
  (non-foldkit) page, returning a handle with `.ports`, `.send`, `.dispose`.

Real example (`examples/counter/src/main.ts`, trimmed):

```typescript
export const Model = S.Struct({ count: S.Number })
export const ClickedIncrement = m('ClickedIncrement')
export const Message = S.Union([ClickedIncrement /* , ... */])

export const update = (model: Model, message: Message) =>
  M.value(message).pipe(
    M.withReturnType<readonly [Model, ReadonlyArray<Command.Command<Message>>]>(),
    M.tagsExhaustive({
      ClickedIncrement: () => [{ count: model.count + 1 }, []],
      // ...
    }),
  )

export const view = (model: Model): Document => {
  const h = html<Message>()
  return {
    title: `Counter: ${model.count}`,
    body: h.div([h.Class('...')], [
      h.div([h.Class('text-6xl')], [model.count.toString()]),
      Button.view<Message>({ onClick: ClickedIncrement(), toView: a => h.button([...a.button], ['+']) }),
    ]),
  }
}
```

Pre-1.0 (v0.114, ~293 releases), MIT licensed, scaffolded via
`npx create-foldkit-app@latest` (Vite + Tailwind + ESLint + Prettier, state-preserving HMR).
"No escape hatch from Effect — you're all in or you're not" is foldkit's own framing, and it
held up under inspection: there is no partial-adoption API for sprinkling foldkit reactivity
onto an existing DOM tree the way, say, a signal library would.

## Worked example: the add-to-playlist panel

This is the most state-heavy, most async-fragile piece of `playlists.ts`, so it's the
clearest before/after.

### State

Today the panel's state is **8 mutable module-level globals** plus a `MutationObserver` that
watches `document.body`'s class list to detect an external sidebar close
(`concert-tracker/frontend/src/playlists.ts:233-280`):

```typescript
let currentAddTarget: AddTarget | null = null;
let allPlaylists: PlaylistRef[] = [];
let memberMap = new Map<number, number>();
let addPanelToken = 0;
let activeFromTyping = false;
let sidebarWasOpen = false;
let activeId: number | "new" | null = null;
let actionableRows: ActionableRow[] = [];

new MutationObserver(() => {
  if (!document.body.classList.contains("sidebar-open") && currentAddTarget) {
    // ... resetAddState()
  }
}).observe(document.body, { attributes: true, attributeFilter: ["class"] });
```

In foldkit this collapses to one `Model`:

```typescript
const AddPanelModel = S.Struct({
  target: S.NullOr(AddTarget),
  allPlaylists: S.Array(PlaylistRef),
  memberMap: S.Record(S.Number, S.Number), // playlist id -> item_id
  activeId: S.NullOr(S.Union([S.Number, S.Literal("new")])),
  activeFromTyping: S.Boolean,
  filterText: S.String,
})
```

The `MutationObserver` workaround disappears entirely — sidebar-open/closed becomes a Model
field the parent owns and pushes down via a `Port`, instead of something the child has to
detect by watching the DOM from outside.

### Rendering

`renderAddList` (`playlists.ts:400-596`, ~200 lines) manually builds two row groups
(`memberEntries`/`nonMemberEntries`), interleaves a synthetic "Create" row, and separately
calls `applyActiveHighlight()` (`playlists.ts:600-617`) to sync `aria-selected` /
`aria-activedescendant` against `activeId` by walking the DOM:

```typescript
function applyActiveHighlight(): void {
  for (const row of actionableRows) {
    const isActive = row.id === activeId;
    row.el.classList.toggle("add-pl-row-active", isActive);
    row.el.setAttribute("aria-selected", isActive ? "true" : "false");
    if (isActive) { /* set aria-activedescendant, scrollIntoView */ }
  }
}
```

In foldkit this is one declarative `view` over the Model — no separate "now go sync the DOM
to match the variable" pass, because the DOM *is* a pure function of the Model:

```typescript
const row = (p: PlaylistRef, isActive: boolean) =>
  h.li(
    [
      h.Class(isActive ? "add-pl-row add-pl-row-active" : "add-pl-row"),
      h.Attribute("aria-selected", isActive ? "true" : "false"),
      h.OnClick(memberMap[p.id] != null ? RemoveFromPlaylist({ id: p.id }) : AddToPlaylist({ id: p.id })),
    ],
    [h.span([h.Class("add-pl-check")], [memberMap[p.id] != null ? "✓" : ""]), h.span([], [p.name])],
  )
```

### Async / staleness

The hand-rolled `addPanelToken` monotonic counter exists purely to discard a fetch that
resolved after a newer `openAdd()` superseded it (`playlists.ts:239`, `320-331`, `383`):

```typescript
let addPanelToken = 0;
// ...
const token = ++addPanelToken;
// ... later, after an await:
if (token !== addPanelToken) return; // superseded
```

In foldkit, side effects are `Command`s the runtime owns; the equivalent
(`fetchMembership`/`addPlaylistItem` wrapped as `Command.define`) returns a result `Message`
that the `update` function can simply ignore if a newer `target` has since replaced it in the
Model — no manual counter bookkeeping, because "is this still relevant" is answered by
comparing against current Model state rather than a side-channel token:

```typescript
const ReloadMembership = Command.define(
  'ReloadMembership', { target: AddTarget }, CompletedMembership,
)(({ target }) => Effect.tryPromise(() => fetchMembership(target)).pipe(Effect.map(CompletedMembership)))

// update:
CompletedMembership: ({ data, forTarget }) =>
  Equal.equals(forTarget, model.target) // ignore if target moved on
    ? [evo(model, { memberMap: () => toMap(data) }), []]
    : [model, []],
```

### Keyboard listbox

`filterKeydown`/`dispatchEnter` (`playlists.ts:619-695`) thread `activeFromTyping` through
arrow-key vs. typing-originated highlight state by hand. This maps cleanly onto Messages
(`ArrowDown`, `ArrowUp`, `PressedEnter`) each with a small, exhaustive `update` branch — the
logic doesn't get simpler, but it becomes table-driven instead of imperative, and the
typing-vs-arrow distinction is just two branches setting `activeFromTyping` instead of being
threaded through call sites by convention.

### Drag-and-drop reorder — the honest friction point

`playlists.ts:144-216` wires HTML5 DnD directly: `dragstart`/`dragover`/`drop` listeners on
`document`, with `dragover` **mutating the live DOM** (`list.insertBefore`/`appendChild`) to
show the in-progress reorder, then `persistOrder()` reads the final DOM order back out to
build the request body. This is fundamentally at odds with MVU, where the DOM is *derived
from* the Model, not mutated and then read back. Doing this properly in foldkit means
tracking drag state (dragged item id, current hover position) in the Model and re-deriving
list order from it on every `dragover` Message — workable, but it's the one place where the
imperative DOM-API-driven code doesn't have a clean 1:1 MVU translation; it would need a
genuine redesign, not a port.

## Reuse matrix

| Asset | Verdict |
|---|---|
| `concert-tracker/frontend/src/generated/openapi.d.ts` | Reuse as-is, or regenerate as Effect `Schema` for end-to-end Schema validation |
| `concert-tracker/frontend/src/api/client.ts` fetch wrappers | Partial reuse — wrap each call in `Command.define`/`Effect.tryPromise`, or replace with Effect's `HttpClient` |
| `concert-tracker/frontend/src/splitter/core.ts` (pure) | Reuse near-verbatim — it's already DOM-free, unit-tested logic; the cleanest port in the codebase |
| `player.ts` / `playlists.ts` / `splitter/index.ts` (DOM/interaction layers) | Rewritten as Model/Message/update/view — not portable as-is |
| `concert-tracker/frontend/src/shared/dom.ts` (`byId`/`byIdOrNull`) | Obsolete — MVU has no manual DOM lookup |
| askama templates (`concert-tracker/templates/*.html`) | Rewritten as foldkit views, **only** if going full-SPA |
| Playwright e2e (`e2e/*.spec.js`) | Mostly still valid — DOM/ARIA-based assertions don't care how the DOM was produced; foldkit additionally offers its own `Story`/`Scene` test primitives |

## Integration paths

**Embed (`Runtime.embed`).** Keep the htmx shell; mount one foldkit widget into a slot, e.g.
`#player-sidebar`. Verified shape from `examples/embedding/src/{main,host}.ts`: the widget
exposes typed `Port`s, the host calls `Runtime.embed(element)` to get a handle with
`.ports.<name>.send`/`.subscribe` and `.dispose()`. The complication specific to this
codebase: the add-to-playlist panel is **not self-contained** — it's opened from `player.ts`
(`window.Player.openSidebar()`, the `showing-add` CSS class, the sidebar-close
`MutationObserver`). That shared ownership of `#player-sidebar` would need to become an
explicit Port contract between the (still-imperative) `player.ts` host and the foldkit
widget, and the page would carry both an Effect-TS runtime and the existing esbuild IIFEs
side by side.

**Full SPA.** foldkit owns routing and every view; htmx and askama are retired. This is a
materially larger project than "rewrite three TS files" — `list`, `detail`, `jobs`, and
`settings` are *only* HTML routes today (`concert-tracker/src/web/mod.rs`), with no JSON
endpoint at all, so a full SPA first requires designing and shipping that JSON surface.
Cleanest long-term architecture if pursued, but it's a frontend-and-backend project, not a
frontend-only one.

**Build.** foldkit's own tooling defaults to Vite. The current pipeline
(esbuild → committed unminified IIFE → `include_str!` → `just ts-verify` diff guard) is a
deliberate project invariant: shipped JS is reviewable in a plain diff and `cargo build`
never needs Node. An Effect-TS/foldkit bundle is heavier than the current handful of small
IIFEs and harder to keep human-reviewable unminified; this tradeoff needs a conscious
decision, not an assumption that Vite output slots into the same place.

## Risks / costs vs. wins

**Costs:** Effect-TS is its own paradigm (Schema, Effect, Match) on top of learning MVU;
pre-1.0 means breaking changes between minors; bundle weight; loses the
committed-reviewable-JS invariant; adoption is all-or-nothing per widget, not incremental
within a file.

**Wins:** the player's queue/auto-advance/prepare-polling and the splitter's timeline editor
are real state machines that MVU models well. For playlists specifically, the
win is concentrated in the add-panel: the `addPanelToken` staleness guard, the
`MutationObserver` sidebar-close detection, and the manual aria-sync pass are exactly the bug
class MVU is designed to make structurally impossible — not "less code" so much as "a category
of bug that can't be expressed." Plus: type-safe routing, time-travel devtools, and
`Story`/`Scene` test primitives as a bonus if adopted.

## Suggested next step

If this is pursued further, spike the **splitter**, not playlists, as the first
`Runtime.embed` proof: `splitter/core.ts` is already pure and DOM-free (the "cleanest port"
row above), it has a dedicated, already-JSON-only API
(`GET/POST/POST .../reset` on `/concerts/:id/split-timestamps`), and — unlike the add-panel
— it doesn't need any coordination with `player.ts` just to *open*. It isn't fully
coupling-free, though: `splitter/index.ts:437` calls `window.Player.playAlbumAt()` and `:448`
reads `window.Player.nowPlaying()` to drive the playhead during audition, so an embedded
widget would still need one small **outbound** Port to player for preview/playhead sync — a
narrower version of the same coordination problem flagged for the add-panel, not an absence
of it. Still the lowest-risk place to learn whether `Runtime.embed` feels good in this
codebase before committing playlists or the player to the same move.

## Spike results (2026-06-20)

The splitter spike above was carried out: `concert-tracker/frontend/src/splitter/index.ts`
now mounts a Foldkit `Runtime.embed` widget (`splitter/widget/`) in place of the original
588-line imperative module, reusing `core.ts` unchanged. `just lint` is clean and the full
`e2e/splitter.spec.js` suite (9 tests, including a real pointer-drag interaction) passes
unmodified against the rewrite. `static/splitter.js` is a minified ~628KB Effect-TS bundle,
excluded from the `ts-verify` drift guard as planned (`player.js`/`playlists.js` unaffected).

**`Runtime.embed` has a real, non-obvious trap:** it takes ownership of the container
element's own attributes (patches them against the widget's root view on every render) and
requires that container to have a non-empty `id` for HMR model preservation — without one it
`Effect.die`s *asynchronously, inside a forked fiber*, invisible to a synchronous try/catch
around the mount call and, in this sandboxed environment, invisible to `console`/`pageerror`
capture too. The fix (mount into a dedicated child `<div>` with its own id, rather than handing
`Runtime.embed` the host's own `#splitter` div that `toggle()`/CSS also manage) is one line
once found, but finding it cost real debugging time and would surprise anyone embedding into
an existing element a host already owns — worth flagging prominently if this pattern is reused
for playlists or the player.

**The clone-then-mutate bridge to `core.ts` works but is a structural seam, not a clean
boundary.** Effect Schema's `.Type` defaults to `ReadonlyArray` for array fields; `core.ts`'s
editor functions mutate arrays in place. `S.mutable(...)` on the affected fields plus a
`structuredClone`-before-mutate helper (`update.ts`'s `withClonedEditor`) resolves the type
mismatch, but nothing stops a future edit from calling `core.ts`'s mutators on `model`'s own
editor directly — the invariant is enforced by one helper function and a comment, not by the
type system. This is exactly the kind of impedance mismatch worth weighing against the
"category of bug that can't be expressed" argument made above for playlists: adopting Foldkit
for code that *wraps* an existing mutable library trades one bug class for a narrower one,
it doesn't eliminate mutation-related bugs outright.

Net: the architecture holds up under real implementation, and the e2e suite gives good
confidence the rewrite is behaviorally equivalent. The two findings above are the concrete
"what would bite us" data this spike was meant to produce, not reasons to abandon the
recommendation above.
