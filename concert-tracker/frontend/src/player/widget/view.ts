import { Option } from "effect";
import { type Html, html } from "foldkit/html";

import { buildQueueRows, nextEnabled, prevEnabled } from "../core";
import { CommandReceived, type Message } from "./message";
import type { ConcertPlaybackState, Model, PlaybackItem, SidebarTrack, SidebarTrackList } from "./model";
import { PlayerCommandValue } from "./port";

// VIEW — player bar + sidebar (queue + concert sections).
// Host embed wiring + layout.html restructure land in commit 7.
//
// NOTE: hand-rolling buttons/inputs here rather than reaching for Ui.Button /
// Ui.Input — @foldkit/ui is not a dependency of this project (see
// package.json). Adopting it is a separate decision from this a11y pass;
// these elements are instead given explicit AriaLabel/Role/AriaPressed by
// hand where the interaction isn't self-describing from visible text.

/** Enter activation for an element carrying `Role("button")` — native
 *  `<button>` gets this for free; a `<span role="button">` needs it wired
 *  explicitly or it's a keyboard trap. Deliberately Enter-only, not the usual
 *  Enter+Space ARIA button convention: these spans live inside #player-bar,
 *  where Space is already claimed by the global playback shortcut (pause,
 *  not row-activation) — see isPlayerPlaybackShortcutTarget in ../core. */
const onEnterKey = (message: Message) => (key: string) =>
  key === "Enter" ? Option.some(message) : Option.none();

// Inline adapter: widget's Option-based Playback → core.PlaybackState nulls.
// Kept local since only nextEnabled/prevEnabled need the core shape.
const toCorePb = (p: Model["playback"]) => ({
  concertId: p.concertId,
  trackIdx: p.trackIdx,
  isVideo: p.isVideo,
  watchUrl: p.watchUrl,
  hasNext: p.hasNext,
  hasPrev: p.hasPrev,
  liked: p.liked,
  concert: Option.getOrNull(p.concert),
});

// ── Shared sidebar-row button builders ──────────────────────────────────
//
// reconstructionList's track row and wholeAlbumList's available-track row
// render the same like/delete/add-to-playlist trio against the same
// SidebarLikeTrack/SidebarDeleteTrack/SidebarAddToPlaylist commands — shared
// here instead of duplicated per list.

// Shared by every rendering of the like star (bar + sidebar rows) so the
// `liked` class can't drift out of sync with one call site again.
const likeButtonClass = (liked: boolean): string => (liked ? "btn-like liked" : "btn-like");

const likeButton = (concertId: number, trackIdx: number, liked: boolean): Html => {
  const h = html<Message>();
  return h.button(
    [
      h.Class(likeButtonClass(liked)),
      h.Title("Like"),
      h.AriaLabel("Like"),
      h.AriaPressed(liked ? "true" : "false"),
      h.OnClick(CommandReceived({ command: PlayerCommandValue.SidebarLikeTrack({ concertId, trackIdx }) })),
    ],
    [liked ? "★" : "☆"],
  );
};

const deleteTrackButton = (concertId: number, trackIdx: number, title: string): Html => {
  const h = html<Message>();
  return h.button(
    [
      h.Class("btn-delete"),
      h.Title("Delete track files"),
      h.AriaLabel(`Delete files for ${title}`),
      h.OnClick(CommandReceived({ command: PlayerCommandValue.SidebarDeleteTrack({ concertId, trackIdx }) })),
    ],
    [h.span([h.Class("icon-trash")], [])],
  );
};

const addToPlaylistButton = (concertId: number, trackIdx: number, title: string): Html => {
  const h = html<Message>();
  return h.button(
    [
      h.Class("btn-add-pl"),
      h.Title("Add to playlist"),
      h.AriaLabel(`Add ${title} to playlist`),
      h.OnClick(
        CommandReceived({ command: PlayerCommandValue.SidebarAddToPlaylist({ concertId, trackIdx, label: title }) }),
      ),
    ],
    ["+"],
  );
};

// ── Concert section (reconstruction mode) ────────────────────────────────

const concertTrackRowView = (
  item: PlaybackItem,
  pos: number,
  trackIdx: number,
  concertId: number,
  isPlaying: boolean,
): Html => {
  const h = html<Message>();
  return h.keyed("li")(
    `track-${trackIdx}`,
    [h.Class(isPlaying ? "concert-item concert-item-playing" : "concert-item")],
    [
      likeButton(concertId, trackIdx, item.liked),
      h.button(
        [
          h.Class(isPlaying ? "btn-track-listen playing" : "btn-track-listen"),
          h.Attribute("data-concert-id", String(concertId)),
          h.Attribute("data-track-idx", String(trackIdx)),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.PlayConcertFrom({ concertId, pos }) })),
        ],
        [item.title],
      ),
      deleteTrackButton(concertId, trackIdx, item.title),
      addToPlaylistButton(concertId, trackIdx, item.title),
    ],
  );
};

const concertInterludeRowView = (item: PlaybackItem, pos: number, concertId: number, isPlaying: boolean): Html => {
  const h = html<Message>();
  const interludeIdx = item.interlude_index ?? 0;
  return h.keyed("li")(
    `interlude-${interludeIdx}`,
    [h.Class(isPlaying ? "concert-item concert-item-interlude concert-item-playing" : "concert-item concert-item-interlude")],
    [
      h.button(
        [
          h.Class(isPlaying ? "btn-track-listen btn-interlude playing" : "btn-track-listen btn-interlude"),
          h.Attribute("data-concert-id", String(concertId)),
          h.Attribute("data-interlude-idx", String(interludeIdx)),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.PlayConcertFrom({ concertId, pos }) })),
        ],
        [item.title],
      ),
      h.button(
        [
          h.Class("btn-delete"),
          h.Title("Delete interlude file"),
          h.AriaLabel(`Delete interlude file for ${item.title}`),
          h.OnClick(
            CommandReceived({ command: PlayerCommandValue.SidebarDeleteInterlude({ concertId, interludeIdx }) }),
          ),
        ],
        [h.span([h.Class("icon-trash")], [])],
      ),
    ],
  );
};

function reconstructionList(concert: ConcertPlaybackState, concertId: number): Html {
  const h = html<Message>();
  return h.ol(
    [h.Class("track-list track-list-concert-playback")],
    concert.items.map((item, pos) => {
      const isPlaying = pos === concert.pos;
      const isInterlude = item.kind === "interlude";
      const trackIdx = item.track_index ?? null;

      return !isInterlude && trackIdx !== null
        ? concertTrackRowView(item, pos, trackIdx, concertId, isPlaying)
        : concertInterludeRowView(item, pos, concertId, isPlaying);
    }),
  );
}

// ── Concert section (whole-album mode) ───────────────────────────────────

const availableTrackRowView = (
  track: SidebarTrack,
  concertId: number,
  isPlaying: boolean,
  tracksBusy: boolean,
): Html => {
  const h = html<Message>();
  return h.keyed("li")(
    `avail-${track.index}`,
    [h.Class(isPlaying ? "concert-item concert-item-playing" : "concert-item")],
    [
      likeButton(concertId, track.index, track.liked),
      h.button(
        [
          h.Class(isPlaying ? "btn-track-listen playing" : "btn-track-listen"),
          h.Attribute("data-concert-id", String(concertId)),
          h.Attribute("data-track-idx", String(track.index)),
          h.Disabled(tracksBusy),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.PlayTrack({ concertId, trackIdx: track.index }) })),
        ],
        [track.title],
      ),
      ...(track.is_video
        ? [
            h.button(
              [
                h.Class("btn-watch"),
                h.OnClick(
                  CommandReceived({ command: PlayerCommandValue.WatchTrackDirect({ concertId, trackIdx: track.index }) }),
                ),
              ],
              ["Watch"],
            ),
          ]
        : []),
      deleteTrackButton(concertId, track.index, track.title),
      addToPlaylistButton(concertId, track.index, track.title),
    ],
  );
};

// Unavailable track: clicking triggers prepare via PlayTrack's missing-file path.
const unavailableTrackRowView = (track: SidebarTrack, concertId: number, tracksBusy: boolean): Html => {
  const h = html<Message>();
  return h.keyed("li")(
    `unavail-${track.index}`,
    [h.Class("concert-item track-unavailable")],
    [
      h.button(
        [
          h.Class("btn-track-listen track-title-unavailable"),
          h.Attribute("data-concert-id", String(concertId)),
          h.Attribute("data-track-idx", String(track.index)),
          h.Disabled(tracksBusy),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.PlayTrack({ concertId, trackIdx: track.index }) })),
        ],
        [track.title],
      ),
    ],
  );
};

function wholeAlbumList(trackList: SidebarTrackList, concertId: number, currentTrackIdx: number | null): Html {
  const h = html<Message>();
  const { tracksBusy, tracks } = trackList;

  return h.ol(
    [h.Class("track-list")],
    tracks.map((track) => {
      const isPlaying = track.index === currentTrackIdx;
      return track.available
        ? availableTrackRowView(track, concertId, isPlaying, tracksBusy)
        : unavailableTrackRowView(track, concertId, tracksBusy);
    }),
  );
}

// ── Queue section ─────────────────────────────────────────────────────────

function queueSection(model: Model): Html {
  const h = html<Message>();
  const liRow = h.keyed("li");
  const { rows } = buildQueueRows(model.queue);

  return h.section(
    [h.Id("sidebar-queue-section")],
    [
      h.h2([], ["Queue"]),
      h.ol(
        [h.Id("sidebar-queue-list"), h.Class("track-list")],
        rows.map((row) => {
          if (row.kind === "group-header") {
            return liRow(
              `group-${row.groupId}`,
              [h.Class("queue-group-header")],
              [
                h.span([h.Class("queue-group-name")], [row.name]),
                h.button(
                  [
                    h.Class("btn-remove-group"),
                    h.AriaLabel(`Remove ${row.name} from queue`),
                    h.OnClick(CommandReceived({ command: PlayerCommandValue.RemoveGroup({ groupId: row.groupId }) })),
                  ],
                  ["×"],
                ),
              ],
            );
          }
          return liRow(
            `song-${row.entry.groupId ?? "solo"}-${row.entry.concertId}-${row.entry.trackIdx}`,
            [h.Class(row.nested ? "queue-song queue-song-nested" : "queue-song")],
            [
              h.button(
                [
                  h.Class("btn-remove-queue"),
                  h.AriaLabel(`Remove ${row.entry.title} from queue`),
                  h.OnClick(CommandReceived({ command: PlayerCommandValue.Dequeue({ pos: row.pos }) })),
                ],
                ["×"],
              ),
              h.button(
                [
                  h.Class("btn-play-queue"),
                  h.OnClick(CommandReceived({ command: PlayerCommandValue.PlayQueueEntryNow({ pos: row.pos }) })),
                ],
                [row.entry.title],
              ),
            ],
          );
        }),
      ),
      h.p(
        [h.Id("sidebar-queue-empty"), h.Style({ display: rows.length === 0 ? "" : "none" })],
        ["Nothing queued"],
      ),
    ],
  );
}

function concertSection(model: Model): Html {
  const h = html<Message>();
  const concertId = model.playback.concertId;

  // No active concert: render empty but structurally stable section.
  if (concertId === null) {
    return h.section([h.Id("sidebar-concert-section")], []);
  }

  const inner = Option.match(model.playback.concert, {
    onSome: (concert) => reconstructionList(concert, concertId),
    onNone: () =>
      Option.match(model.sidebar.tracks, {
        onSome: (trackList) => wholeAlbumList(trackList, concertId, model.playback.trackIdx),
        onNone: () => h.p([h.Class("sidebar-loading")], ["Loading…"]),
      }),
  });

  return h.section(
    [h.Id("sidebar-concert-section")],
    [h.h2([h.Id("sidebar-concert-heading")], ["Now playing"]), inner],
  );
}

// ── Player bar ────────────────────────────────────────────────────────────

function playerBarView(model: Model): Html {
  const h = html<Message>();
  const p = model.playback;
  const hasMedia = p.concertId !== null;
  const hasTrack = hasMedia && p.trackIdx !== null;
  const ps = toCorePb(p);
  const queueCount = model.queue.length;
  const nextOn = hasMedia && nextEnabled(ps, queueCount);
  const prevOn = hasMedia && prevEnabled(ps);
  const errorText = model.status._tag === "Error" ? model.status.message : "";
  const busyText = model.status._tag === "Busy" ? model.status.message : "";

  // The `active` class drives visibility (#player-bar is display:none until
  // active); reactive on hasMedia so it can never desync from playback.
  return h.div(
    [h.Id("player-bar"), ...(hasMedia ? [h.Class("active")] : [])],
    [
      // ── Queue/sidebar toggle ────────────────────────────────────
      h.button(
        [
          h.Id("player-queue-toggle"),
          h.AriaLabel("Toggle queue and tracks sidebar"),
          h.AriaExpanded(model.sidebar.open),
          h.Title("Show queue and tracks"),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
        ],
        [
          "☰",
          h.span(
            [
              h.Id("player-queue-badge"),
              // CSS baseline is visibility:hidden (style.css) — deliberately
              // visibility, not display, so the badge always occupies its
              // slot and enqueuing a track never shifts the bar layout.
              h.Style({ visibility: queueCount > 0 ? "visible" : "hidden" }),
              h.Title(queueCount > 0 ? model.queue.map((q) => q.title).join("\n") : ""),
            ],
            [queueCount > 0 ? String(queueCount) : ""],
          ),
        ],
      ),

      // ── Info: title-line + artist + playlist ────────────────────
      h.div(
        [h.Id("player-info")],
        [
          h.span(
            [h.Class("player-title-line")],
            [
              h.button(
                [
                  h.Id("player-like"),
                  h.Class(likeButtonClass(p.liked)),
                  h.Title("Like"),
                  h.AriaLabel("Like"),
                  h.AriaPressed(p.liked ? "true" : "false"),
                  h.Style({ display: hasTrack ? "" : "none" }),
                  h.OnClick(CommandReceived({ command: PlayerCommandValue.ToggleLike() })),
                ],
                [p.liked ? "★" : "☆"],
              ),
              h.button(
                [
                  h.Id("player-add-pl"),
                  h.Title("Add to playlist"),
                  h.AriaLabel("Add to playlist"),
                  h.Style({ display: hasTrack ? "" : "none" }),
                  h.OnClick(CommandReceived({ command: PlayerCommandValue.AddToPlaylist() })),
                ],
                ["+"],
              ),
              h.span(
                [
                  h.Id("player-track"),
                  h.Role("button"),
                  h.Tabindex(0),
                  h.AriaLabel("Toggle queue and tracks sidebar"),
                  // #player-track has `display: none` as its CSS baseline
                  // (style.css); like the action buttons above, it needs
                  // an explicit shown value, not "".
                  h.Style({ display: hasTrack && p.trackIdx !== null ? "inline-block" : "none" }),
                  h.OnClick(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
                  h.OnKeyDownPreventDefault(
                    onEnterKey(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
                  ),
                ],
                [hasTrack && p.trackIdx !== null ? `#${p.trackIdx + 1}` : ""],
              ),
              h.span(
                [
                  h.Id("player-title"),
                  h.Role("button"),
                  h.Tabindex(0),
                  h.AriaLabel("Toggle queue and tracks sidebar"),
                  h.OnClick(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
                  h.OnKeyDownPreventDefault(
                    onEnterKey(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
                  ),
                ],
                [p.title],
              ),
            ],
          ),
          // Concert navigation: a plain click is intercepted by
          // Player.openConcert (host shim, ../index.ts), which
          // preventDefaults and calls htmx.ajax with this anchor as
          // `source` — htmx then reads hx-target/hx-select/hx-swap/
          // hx-push-url below to do a partial #content swap + history push
          // so playback continues. Modifier/aux clicks and non-primary
          // targets fall through to the native href (e.g. open in new
          // tab). hx-boost=false keeps boost from double-handling the
          // click.
          h.a(
            [
              h.Id("player-artist"),
              h.Attribute("hx-boost", "false"),
              h.Attribute("onclick", "Player.openConcert(event)"),
              h.Href(hasMedia ? `/concerts/${p.concertId}` : "#"),
              h.Attribute("hx-target", "#content"),
              h.Attribute("hx-select", "#content"),
              h.Attribute("hx-swap", "outerHTML show:window:top"),
              h.Attribute("hx-push-url", "true"),
              h.Title("View concert"),
            ],
            [p.artist],
          ),
          h.span(
            [h.Id("player-playlist"), h.Style({ display: p.playlistLabel !== null ? "" : "none" })],
            [p.playlistLabel ?? ""],
          ),
        ],
      ),

      // ── Status / error feedback ─────────────────────────────────
      // #player-error / #player-status have `display: none` CSS baselines
      // (style.css), so they need an explicit shown value — "" would leave
      // the baseline in effect (same rule as the action buttons below).
      h.span(
        [h.Id("player-error"), h.Role("alert"), h.Style({ display: errorText ? "inline" : "none" })],
        [errorText],
      ),
      h.span(
        [h.Id("player-status"), h.AriaLive("polite"), h.Style({ display: busyText ? "inline" : "none" })],
        [busyText],
      ),

      // ── Action buttons ──────────────────────────────────────────
      // Watch gates on isVideo alone: it only folds out the inline video
      // panel over the already-playing #player-audio element, so it needs
      // no URL — and concert-reconstruction playback always has
      // watchUrl: null (see watchUrlFor's ConcertItem case in update/helpers.ts)
      // even for video items. Open gates on watchUrl because OpenExternal
      // is a no-op without one.
      //
      // "inline-block" (not "") is the shown value everywhere in this
      // action-button group: #player-watch/#player-open/#player-delete
      // all have `display: none` as their CSS baseline (style.css), so an
      // empty inline style leaves that baseline in effect and the element
      // stays hidden — only a real value actually overrides it.
      h.button(
        [
          h.Id("player-watch"),
          h.Title("Watch video in player"),
          h.Style({ display: p.isVideo ? "inline-block" : "none" }),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.Watch() })),
        ],
        ["Watch"],
      ),
      h.button(
        [
          h.Id("player-open"),
          h.Title("Open in system player"),
          h.AriaLabel("Open in system player"),
          h.Style({ display: p.watchUrl !== null ? "inline-block" : "none" }),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.OpenExternal() })),
        ],
        ["⊞"],
      ),
      h.button(
        [
          h.Id("player-delete"),
          h.Title("Delete this track"),
          h.AriaLabel("Delete this track"),
          // Liked tracks hide Delete (mirrors the old player's
          // `trackIdx == null || liked` guard) — deleting a starred
          // track's files is a destructive action gated behind unstarring
          // first.
          h.Style({ display: hasTrack && !p.liked ? "inline-block" : "none" }),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.DeleteTrack() })),
        ],
        [h.span([h.Class("icon-trash")], [])],
      ),

      // ── Transport ───────────────────────────────────────────────
      h.button(
        [
          h.Id("player-prev"),
          h.Title("Previous track"),
          h.AriaLabel("Previous track"),
          h.Disabled(!prevOn),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.SkipToPrev() })),
        ],
        ["⏮"],
      ),
      h.button(
        [
          h.Id("player-play-pause"),
          h.AriaLabel(model.isPlaying ? "Pause" : "Play"),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.TogglePause() })),
        ],
        [model.isPlaying ? "⏸" : "▶"],
      ),
      h.button(
        [
          h.Id("player-next"),
          h.Title("Next track"),
          h.AriaLabel("Next track"),
          h.Disabled(!nextOn),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.SkipToNext() })),
        ],
        ["⏭"],
      ),
      // Seek + time: static until audio Subscription adds currentTime/duration,
      // so it's disabled rather than presented as a working control.
      h.input([
        h.Id("player-seek"),
        h.Type("range"),
        h.AriaLabel("Seek"),
        h.Min("0"),
        h.Max("100"),
        h.Value("0"),
        h.Step("1"),
        h.Disabled(true),
      ]),
      h.span([h.Id("player-time")], ["0:00 / 0:00"]),
    ],
  );
}

// ── Sidebar ───────────────────────────────────────────────────────────────

function sidebarView(model: Model): Html {
  const h = html<Message>();
  return h.aside(
    [h.Id("player-sidebar")],
    [
      h.button(
        [
          h.Id("sidebar-close"),
          h.Type("button"),
          h.Title("Close"),
          h.AriaLabel("Close sidebar"),
          h.OnClick(CommandReceived({ command: PlayerCommandValue.CloseSidebar() })),
        ],
        ["×"],
      ),
      h.div([h.Id("sidebar-resize"), h.AriaHidden(true)], []),
      h.div([h.Class("sidebar-top-spacer")], []),
      queueSection(model),
      concertSection(model),
    ],
  );
}

export const view = (model: Model): Html => {
  const h = html<Message>();
  return h.div([], [playerBarView(model), sidebarView(model)]);
};
