# Adopt the Foldkit oxlint plugin for concert-tracker's frontend

## Summary

`concert-tracker/frontend` had no JS/TS linter — only `tsc --noEmit` via `just ts-check`.
Foldkit ships an [oxlint plugin](https://foldkit.dev/tooling/oxlint-plugin) that enforces
its Elm Architecture conventions (Message/Command naming and shape) alongside a strict
TypeScript baseline. This change adopts the plugin's standard config, fixes every finding,
and wires `oxlint` into the existing lint gates (`just lint`, the pre-push hook, CI)
alongside `ts-check`.

## Config

`concert-tracker/frontend/.oxlintrc.json` — copied from Foldkit's `create-foldkit-app`
scaffold template unchanged. Enables:

- `no-unused-vars` (with `^_` ignore patterns)
- `typescript/no-explicit-any`
- `typescript/consistent-type-assertions: "never"` — no `as`/`<Type>` casts anywhere
- All 7 `foldkit/*` rules: `no-noop-message`, `got-submodel-message-name`,
  `message-binding-matches-tag`, `got-prefix-requires-submodel-payload`,
  `no-empty-object-tagged-call`, `prefer-callable-message-constructor`,
  `command-binding-matches-name`

`concert-tracker/frontend/package.json` gained `oxlint` (1.72.0) and
`@foldkit/oxlint-plugin` (0.1.0) devDependencies and a `"lint": "oxlint"` script.

## Findings and fixes

**Dead code.** `src/player.ts` (the pre-Foldkit imperative player, ~2000 lines) was still
present despite `docs/change/2026-06-25-foldkit-player.md` stating it had been "replaced by
a Foldkit widget" — it was never actually deleted after that port. It wasn't a build entry
point and nothing imported it; only its own comments referenced it. Deleted outright; its
findings (stale `as` casts) went with it.

**`Command.define` no-args form.** Several Commands (`PauseAudio`, `HideVideoPanel`,
`ScrollQueueToBottom`, etc., plus three in the playlists widget) were defined with an
explicit empty-fields record: `Command.define("Name", {}, ResultMessage)`. Foldkit's
`Command.define` treats a plain-object second argument as the *args* form — even when
empty — so call sites were required to pass `Name({})`, tripping
`foldkit/no-empty-object-tagged-call`. Fixed by switching these to the genuine no-args
overload (`Command.define("Name", ResultMessage)`, effect passed as a value rather than a
`() =>` builder), matching the pattern Foldkit's own examples use for commands with no
arguments. Call sites became `Name()`.

**`got-submodel-message-name` false positives.** Three Messages (`FailedFetchInfo`,
`FailedConcertPlayback` in the player widget; `FailedMutation` in playlists) carried a
`message: S.String` field for user-facing error text. The rule's heuristic for detecting
Submodel wrapper Messages keys on a field literally named `message`, so these tripped it
despite not being wrappers. Renamed the field to `errorMessage` at the definition, every
construction call, and every destructuring site (including tests) — a more precise name
regardless of the lint rule.

**`command-binding-matches-name`.** `SyncNowPlayingMirrorCmd`'s binding didn't match its
`Command.define` name `"SyncNowPlayingMirror"`. Renamed the binding (and all references,
including the Story test suite) to match.

**Type assertions — DOM narrowing.** `e.target as HTMLElement`, `e as MouseEvent`, and a
`(evt as CustomEvent<...>).detail` cast in drag/drop handlers, keyboard/outside-click
subscriptions, and an htmx-swap subscription were replaced with `instanceof` narrowing
(`e.target instanceof HTMLElement ? ... : null`, `evt instanceof CustomEvent ? evt.detail :
undefined`). One `row.id as number` in the playlists view (documented as "never `'new'`
for member rows") became a runtime `typeof row.id !== "number"` check that throws loudly
instead — consistent with `shared/dom.ts`'s existing "throw on a violated invariant" style.

**Type assertions — DOM lookup helpers (`shared/dom.ts`).** `byId`/`byIdOrNull` took a
generic `T extends Element` and cast the `getElementById` result to it with no runtime
check (`el as unknown as T`). Replaced the generic with two new functions that verify the
type instead of asserting it:

- `byId`/`byIdOrNull` now return plain `HTMLElement`/`HTMLElement | null` — no cast needed,
  since that's already `getElementById`'s return type.
- `byIdOf(id, ctor)` / `byIdOfOrNull(id, ctor)` take the element's constructor
  (e.g. `HTMLMediaElement`, `HTMLInputElement`) and verify with `instanceof`, throwing if
  the element exists but is the wrong type. All 9 call sites that previously supplied a
  type parameter (`byId<HTMLMediaElement>(...)`, a raw
  `document.getElementById(...) as HTMLMediaElement | null` in `splitter/index.ts`, etc.)
  now pass the constructor value instead.

**Type assertions — fetch boundary (`api/client.ts`).** `getJson`/`getJsonOrNull` cast
`response.json()` (genuinely `Promise<unknown>`) to the caller's generic type parameter,
and two inline call sites (`playlists/pages.ts`, `playlists/widget/command.ts`) repeated
the same cast by hand for `{ id: number }`. There's no runtime Schema to decode against —
the wire types come from `openapi-typescript`, not Effect Schema — so introducing one was
out of scope for this change. Instead, extracted the one unavoidable assertion into a
single `readJson<T>(r: Response): Promise<T>` helper with one justified
`// oxlint-disable-next-line typescript/consistent-type-assertions` comment; `getJson` and
`getJsonOrNull` now call it, and the two inline call sites were routed through it using the
existing generated `CreatedPlaylistJson` type instead of a local `{ id: number }` shape.

## Gate wiring

- `scripts/ts-lint.sh` (new, mirrors `scripts/ts-check.sh`): `npm run lint` in
  `concert-tracker/frontend`.
- `justfile`: new `ts-lint` recipe; added to the `lint` recipe
  (`lint: fmt-check clippy shellcheck ts-check ts-lint`).
- `.githooks/pre-push`: runs `just ts-lint` after `just ts-check`.
- `.github/workflows/ci.yml`: **not** updated in this change — the GitHub App used to push
  this branch lacks the `workflows` permission needed to touch that file. Adding an
  "oxlint" step (mirroring the TypeScript type-check step, `run: ./scripts/ts-lint.sh`) to
  the `frontend` job is a follow-up.
- `README.md`'s "Linting" section: added `ts-lint` alongside `ts-check`. In passing,
  corrected two stale references to `just ts-verify` in `just lint`'s description and the
  pre-push bullet — that recipe was removed from the justfile and pre-push hook in
  `docs/change/2026-06-25-foldkit-player.md` (the standalone `scripts/ts-verify.sh` still
  runs, but only as a direct CI step, not via `just`).

## Verification

- `npm run lint` (`concert-tracker/frontend`) — 0 findings.
- `npx tsc --noEmit` — clean.
- `npm run test:story` — 157 Story/Scene tests pass.
- `./scripts/ts-check.sh` and `./scripts/ts-test.sh` — clean (68 + 157 tests).
- `just ts-build` then `cargo build` — regenerated `concert-tracker/static/*.js` compiles
  through the `include_str!` embeds.
- Sanity-checked the gate: temporarily reintroduced a stray `as` cast and an `m("NoOp")`
  Message, confirmed `npm run lint` / `just ts-lint` fail on each, then reverted.
