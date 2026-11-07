# Playback, listen, and like coverage

Issue [#162](https://github.com/gregwebs/tiny-desk-splitter/issues/162) (move
playback resolution, track navigation, explicit listen recording, and
track-like changes behind `Concerts`) was implemented with the rest of the
Concert application interface in
[the #177 contraction](2026-07-25-concert-catalog-and-status.md), and recorded in
[Playback, listen, and like behaviour behind Concerts](2026-07-25-playback-listen-and-like.md).
`docs/concerts.md` documents the resolved media descriptions, the typed
`ConcertPlaybackError`, and the split between resolving media and recording an
event.

Auditing the ticket's acceptance criteria against `main` after that contraction
left two gaps in *"Interface-level tests cover source, reconstruction, missing
media, unsupported formats, navigation, listens, and like retries"* and in
*"Previous/next track resolution and browser-playability facts come through the
same interface"*:

- **Unsupported formats were only tested inside `playback.rs`.** Nothing at the
  interface level pinned what an unplayable container means. The distinction the
  product depends on — a browser cannot decode it, a system player can, so
  navigation and reconstruction step over it — was asserted only against that
  module's private helpers.
- **"Returns resulting Concert State" was asserted loosely.** The listen, watch,
  and like tests read one field of the returned state instead of checking that it
  is the state an adapter re-renders without a second query.

Adapter coverage had the same shape of hole the #160 and #161 audits found:

- No successful `POST /concerts/:id/listen` or
  `POST /concerts/:id/tracks/:idx/listen` existed over HTTP — only the absence of
  a listen link — so the button's own pass/fail state, the player call it still
  delivers, and the events each route records were unwatched.
- `/watch` was covered for its status code but not for the `watch` event it
  records, which is exactly the recording #162 moved out of the handler.
- The two error mappings this behaviour depends on had no test: playback
  resolution's three situations onto 404/500, and a rejected listen/watch onto a
  button state rather than an HTTP error.

## Changes

Only tests and documentation. No route, markup, JSON shape, event name or
payload, or status code changed.

- `concerts.rs` gained a fixture that names its container
  (`seeded_media_in_format`), because the extension a fixture writes is what
  decides whether playback is browser-playable, plus three interface tests:
  - an unsupported **source** format (`.mkv`) is described rather than reported
    as absent or as an integrity failure: `source_media` and `playback` carry
    `playable: false`, and `media_file` still resolves the path a system player
    would open;
  - an unsupported **track** format among playable ones stays a present,
    available track that `track_media` and `media_file` describe, and that
    `next_track_media`, `prev_track_media`, and the reconstruction sequence step
    over, so the browser is never handed a file it cannot decode;
  - a concert whose **only split media** is unsupported has nothing to
    reconstruct (`Lookup(NotPlayable)`), the plan-level consequence of the same
    rule. A source in an unsupported container is not that case: whole-album
    playback still resolves it, with `playable: false` telling the adapter to use
    the system player.
- The listen, watch, and like tests now compare the returned Concert State with a
  fresh observation (`assert_is_current_state`): like flags, per-track rows, both
  media observations, active work, whether archiving is configured, and permitted
  actions. Every state-returning call in those tests is checked, including a
  retried like and the unlike, so "no second event" cannot hide a stale state
  being returned.
- `web/handlers.rs` pins the two mappings: `NotFound` and `NotPlayable` are 404s
  while `MarkedDownloadedButMissing`, `InvalidFilename`, and an operational query
  failure are 500s; and `media_command_state` answers `success`, `error` for an
  expected rejection (missing media), 404 for an absent concert, and 500 for a
  fault.
- `hurl/media_info_navigation.hurl` (22 → 28 requests) pins the black-box facts
  the player bar branches on: an mkv source and an mkv track both answer 200 with
  `playable: false` and a `.mkv` URL, and `next`/`prev` answer 404 rather than
  offering an undecodable neighbour.
- `hurl/concert_playback.hurl` (11 → 14 requests) pins that a concert whose
  tracks exist and are listed `available` still has no whole-concert plan when no
  browser can decode them.
- `hurl/media_files_lifecycle.hurl` (28 → 39 requests, plus one more seed and
  two more assertions than the first draft, which shared one concert) pins the recording
  adapters: resolving the playback plan and track info records nothing; the
  listen buttons answer `btn-listen-success` while still handing the click to
  `Player.playAlbum` / `Player.playTrack`; a whole-concert listen on one concert
  and a track listen on another each record a `listen` event (two concerts,
  because `/test/assert/concert_events` reports which event names exist rather
  than how many, so one concert could not prove the track route records
  anything); and `/watch` records a `watch` event.

The browser-playability paragraph in `docs/concerts.md` now states the rule the
new tests pin: an unsupported container is described and openable, is stepped
over by navigation and reconstruction, and — when it is the only *split* media —
leaves a concert with nothing to reconstruct rather than producing a
missing-media rejection.

## Tests

- `concerts::tests::an_unplayable_source_format_is_described_and_openable`,
  `an_unplayable_track_format_is_never_navigated_to_or_reconstructed`,
  `a_concert_with_only_unplayable_media_has_nothing_to_play`.
- `concerts::tests::resolving_playback_records_no_listen_and_the_command_records_one`,
  `record_watch_records_a_watch_event_for_the_opened_file`,
  `set_track_liked_stores_the_desired_state_and_is_idempotent` — now also assert
  the returned state is current.
- `web::handlers::tests::playback_errors_map_absence_to_404_and_integrity_to_500`,
  `media_command_rejections_are_button_states_not_http_errors`,
  `listen_without_media_is_a_button_state_not_an_http_error`.
- `hurl/media_files_lifecycle.hurl`, plus the appended cases in
  `hurl/media_info_navigation.hurl` and `hurl/concert_playback.hurl`.

## Mutation checks

Each was applied, the named test observed failing, then the change reverted:

- Removing the browser-playability filter from the songs-only reconstruction path
  fails `a_concert_with_only_unplayable_media_has_nothing_to_play` and
  `an_unplayable_track_format_is_never_navigated_to_or_reconstructed`.
- Making `find_playable_track` stop skipping unplayable extensions fails
  `an_unplayable_track_format_is_never_navigated_to_or_reconstructed` (`next`
  returns the mkv track instead of the third song).
- Reporting `playable: true` for every source fails
  `an_unplayable_source_format_is_described_and_openable`.
- Returning the Concert State loaded *before* the like is written fails
  `set_track_liked_stores_the_desired_state_and_is_idempotent`.

The adapter mappings are pinned by construction rather than by mutating the
product: answering `MarkedDownloadedButMissing` with 404 fails
`playback_errors_map_absence_to_404_and_integrity_to_500`, and turning a
`MissingMedia` rejection into an HTTP error fails
`media_command_rejections_are_button_states_not_http_errors`.

## The one case the hurl suite cannot carry

The rejected-listen button state — `POST /concerts/:id/listen` for a concert with
no media, which answers 200 with `btn-listen-error` — has no hurl case, and the
Change Record says so rather than dropping it. Adding one (a
`lifecycle_concert` seed plus that POST) reproducibly wedged the shared hurl
server: in the next file `POST /concerts/:id/tracks/0/like` never got a response
(`Operation timed out after 300004 milliseconds with 0 bytes received`, with no
server-side log line for the request at all) and the file after that failed on
its first request. Removing just that case and re-running the suite gave 14
files and 100%, repeatedly. The wedge reproduces in both
`hurl/media_state_errors.hurl` and `hurl/media_files_lifecycle.hurl`, so it is
about the requests rather than the file; and since the web binary is unchanged by
this slice, it is a latent issue in the shared-server harness that needs its own
investigation instead of a workaround here.

The adapter behaviour is covered in process instead:
`web::handlers::tests::listen_without_media_is_a_button_state_not_an_http_error`
calls `listen` and `listen_track` for a concert with no media and asserts the
response is 200, that the returned markup carries `btn-listen-error`, and that no
`listen` event was recorded. With
`web::handlers::tests::media_command_rejections_are_button_states_not_http_errors`
pinning the mapper's own branches, criterion 5's listen behaviour is demonstrated
without the hurl route.

## Verification gates

Run in this checkout with artifacts written to a scratch target directory on
`/tmp`: the checkout's own volume holds 19 G of `videos/` plus 2.5 G of
`concerts/` out of 29 G, so an in-repo debug build cannot fit and linking fails
with `final link failed: No space left on device`. `Dockerfile.dev` documents the
scratch-directory recipe.

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
- `cargo nextest run --tests` — 922 passed, 0 skipped (916 before this slice; the
  six new tests are three interface tests, two adapter mapping tests, and one
  handler test).
- `node scripts/hurl-test.js` — 14 files, 298 requests, 100%.
- `npx playwright test` — 171 passed, 2 failed. Both failures are in
  `e2e/openapi.spec.js` (the Swagger UI never renders
  `section.swagger-ui.swagger-container`), and both reproduce on unmodified
  `main` in this sandbox, so they are not attributable to this change.
- `./scripts/shellcheck.sh`, `./scripts/ts-check.sh`, `./scripts/ts-lint.sh` —
  exit 0 (no shell or TypeScript changed; run to confirm the standard lint suite
  still passes).
