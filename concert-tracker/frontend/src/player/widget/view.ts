import { Option } from "effect";
import { type Html, html } from "foldkit/html";

import { buildQueueRows, nextEnabled, prevEnabled } from "../core";
import { CommandReceived, type Message } from "./message";
import type { ConcertPlaybackState, Model, SidebarTrackList } from "./model";
import { PlayerCommandValue } from "./port";

// VIEW — player bar + sidebar (queue + concert sections).
// Host embed wiring + layout.html restructure land in commit 7.

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

// ── Concert section helpers ───────────────────────────────────────────────

function reconstructionList(concert: ConcertPlaybackState, concertId: number): Html {
  const h = html<Message>();
  const liRow = h.keyed("li");
  return h.ol(
    [h.Class("track-list track-list-concert-playback")],
    concert.items.map((item, pos) => {
      const isPlaying = pos === concert.pos;
      const isInterlude = item.kind === "interlude";
      const trackIdx = item.track_index ?? null;

      if (!isInterlude && trackIdx !== null) {
        return liRow(
          String(pos),
          [h.Class(isPlaying ? "concert-item concert-item-playing" : "concert-item")],
          [
            h.button(
              [
                h.Class(item.liked ? "btn-like liked" : "btn-like"),
                h.Title("Like"),
                h.OnClick(
                  CommandReceived({
                    command: PlayerCommandValue.SidebarLikeTrack({ concertId, trackIdx }),
                  }),
                ),
              ],
              [item.liked ? "★" : "☆"],
            ),
            h.button(
              [
                h.Class(isPlaying ? "btn-track-listen playing" : "btn-track-listen"),
                h.Attribute("data-concert-id", String(concertId)),
                h.Attribute("data-track-idx", String(trackIdx)),
                h.OnClick(
                  CommandReceived({ command: PlayerCommandValue.PlayConcertFrom({ concertId, pos }) }),
                ),
              ],
              [item.title],
            ),
            h.button(
              [
                h.Class("btn-delete"),
                h.Title("Delete track files"),
                h.OnClick(
                  CommandReceived({
                    command: PlayerCommandValue.SidebarDeleteTrack({ concertId, trackIdx }),
                  }),
                ),
              ],
              [h.span([h.Class("icon-trash")], [])],
            ),
            h.button(
              [
                h.Class("btn-add-pl"),
                h.Title("Add to playlist"),
                h.OnClick(
                  CommandReceived({
                    command: PlayerCommandValue.SidebarAddToPlaylist({
                      concertId,
                      trackIdx,
                      label: item.title,
                    }),
                  }),
                ),
              ],
              ["+"],
            ),
          ],
        );
      }

      // Interlude row
      const interludeIdx = item.interlude_index ?? 0;
      return liRow(
        String(pos),
        [h.Class(isPlaying ? "concert-item concert-item-interlude concert-item-playing" : "concert-item concert-item-interlude")],
        [
          h.button(
            [
              h.Class(isPlaying ? "btn-track-listen btn-interlude playing" : "btn-track-listen btn-interlude"),
              h.Attribute("data-concert-id", String(concertId)),
              h.Attribute("data-interlude-idx", String(interludeIdx)),
              h.OnClick(
                CommandReceived({ command: PlayerCommandValue.PlayConcertFrom({ concertId, pos }) }),
              ),
            ],
            [item.title],
          ),
          h.button(
            [
              h.Class("btn-delete"),
              h.Title("Delete interlude file"),
              h.OnClick(
                CommandReceived({
                  command: PlayerCommandValue.SidebarDeleteInterlude({ concertId, interludeIdx }),
                }),
              ),
            ],
            [h.span([h.Class("icon-trash")], [])],
          ),
        ],
      );
    }),
  );
}

function wholeAlbumList(
  trackList: SidebarTrackList,
  concertId: number,
  currentTrackIdx: number | null,
): Html {
  const h = html<Message>();
  const liRow = h.keyed("li");
  const { tracksBusy, tracks } = trackList;

  return h.ol(
    [h.Class("track-list")],
    tracks.map((track) => {
      const isPlaying = track.index === currentTrackIdx;

      if (track.available) {
        return liRow(
          String(track.index),
          [h.Class(isPlaying ? "concert-item concert-item-playing" : "concert-item")],
          [
            h.button(
              [
                h.Class(track.liked ? "btn-like liked" : "btn-like"),
                h.Title("Like"),
                h.OnClick(
                  CommandReceived({
                    command: PlayerCommandValue.SidebarLikeTrack({
                      concertId,
                      trackIdx: track.index,
                    }),
                  }),
                ),
              ],
              [track.liked ? "★" : "☆"],
            ),
            h.button(
              [
                h.Class(isPlaying ? "btn-track-listen playing" : "btn-track-listen"),
                h.Attribute("data-concert-id", String(concertId)),
                h.Attribute("data-track-idx", String(track.index)),
                h.Disabled(tracksBusy),
                h.OnClick(
                  CommandReceived({
                    command: PlayerCommandValue.PlayTrack({ concertId, trackIdx: track.index }),
                  }),
                ),
              ],
              [track.title],
            ),
            ...(track.is_video
              ? [
                  h.button(
                    [
                      h.Class("btn-watch"),
                      h.OnClick(
                        CommandReceived({
                          command: PlayerCommandValue.WatchTrackDirect({
                            concertId,
                            trackIdx: track.index,
                          }),
                        }),
                      ),
                    ],
                    ["Watch"],
                  ),
                ]
              : []),
            h.button(
              [
                h.Class("btn-delete"),
                h.Title("Delete track files"),
                h.OnClick(
                  CommandReceived({
                    command: PlayerCommandValue.SidebarDeleteTrack({
                      concertId,
                      trackIdx: track.index,
                    }),
                  }),
                ),
              ],
              [h.span([h.Class("icon-trash")], [])],
            ),
            h.button(
              [
                h.Class("btn-add-pl"),
                h.Title("Add to playlist"),
                h.OnClick(
                  CommandReceived({
                    command: PlayerCommandValue.SidebarAddToPlaylist({
                      concertId,
                      trackIdx: track.index,
                      label: track.title,
                    }),
                  }),
                ),
              ],
              ["+"],
            ),
          ],
        );
      }

      // Unavailable track: clicking triggers prepare via PlayTrack's missing-file path.
      return liRow(
        String(track.index),
        [h.Class("concert-item track-unavailable")],
        [
          h.button(
            [
              h.Class("btn-track-listen track-title-unavailable"),
              h.Attribute("data-concert-id", String(concertId)),
              h.Attribute("data-track-idx", String(track.index)),
              h.Disabled(tracksBusy),
              h.OnClick(
                CommandReceived({
                  command: PlayerCommandValue.PlayTrack({ concertId, trackIdx: track.index }),
                }),
              ),
            ],
            [track.title],
          ),
        ],
      );
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
                    h.OnClick(CommandReceived({ command: PlayerCommandValue.RemoveGroup({ groupId: row.groupId }) })),
                  ],
                  ["×"],
                ),
              ],
            );
          }
          return liRow(
            `song-${row.entry.concertId}-${row.entry.trackIdx}`,
            [h.Class(row.nested ? "queue-song queue-song-nested" : "queue-song")],
            [
              h.button(
                [
                  h.Class("btn-remove-queue"),
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

export const view = (model: Model): Html => {
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

  return h.div(
    [],
    [
      // ── Player bar ────────────────────────────────────────────────────
      h.div(
        [h.Id("player-bar")],
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
            ["☰", h.span([h.Id("player-queue-badge")], [queueCount > 0 ? String(queueCount) : ""])],
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
                      h.Class("btn-like"),
                      h.Title("Like"),
                      h.Style({ display: hasTrack ? "" : "none" }),
                      h.OnClick(CommandReceived({ command: PlayerCommandValue.ToggleLike() })),
                    ],
                    [p.liked ? "★" : "☆"],
                  ),
                  h.button(
                    [
                      h.Id("player-add-pl"),
                      h.Title("Add to playlist"),
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
                      h.OnClick(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
                    ],
                    [hasTrack && p.trackIdx !== null ? `${p.trackIdx + 1}.` : ""],
                  ),
                  h.span(
                    [
                      h.Id("player-title"),
                      h.Role("button"),
                      h.Tabindex(0),
                      h.OnClick(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
                    ],
                    [p.title],
                  ),
                ],
              ),
              // Concert navigation via hx-* — host shim intercepts modifier
              // clicks before emitting OpenConcert (commit 7).
              h.a(
                [
                  h.Id("player-artist"),
                  h.Attribute("hx-boost", "false"),
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
                [
                  h.Id("player-playlist"),
                  h.Style({ display: p.playlistLabel !== null ? "" : "none" }),
                ],
                [p.playlistLabel ?? ""],
              ),
            ],
          ),

          // ── Status / error feedback ─────────────────────────────────
          h.span([h.Id("player-error")], [errorText]),
          h.span([h.Id("player-status")], [busyText]),

          // ── Action buttons ──────────────────────────────────────────
          h.button(
            [
              h.Id("player-watch"),
              h.Title("Watch video in player"),
              h.Style({ display: p.isVideo && p.watchUrl !== null ? "" : "none" }),
              h.OnClick(CommandReceived({ command: PlayerCommandValue.Watch() })),
            ],
            ["Watch"],
          ),
          h.button(
            [
              h.Id("player-open"),
              h.Title("Open in system player"),
              h.Style({ display: p.watchUrl !== null ? "" : "none" }),
              h.OnClick(CommandReceived({ command: PlayerCommandValue.OpenExternal() })),
            ],
            ["⊞"],
          ),
          h.button(
            [
              h.Id("player-delete"),
              h.Title("Delete this track"),
              h.AriaLabel("Delete this track"),
              h.Style({ display: hasTrack ? "" : "none" }),
              h.OnClick(CommandReceived({ command: PlayerCommandValue.DeleteTrack() })),
            ],
            [h.span([h.Class("icon-trash")], [])],
          ),

          // ── Transport ───────────────────────────────────────────────
          h.button(
            [
              h.Id("player-prev"),
              h.Title("Previous track"),
              h.Disabled(!prevOn),
              h.OnClick(CommandReceived({ command: PlayerCommandValue.SkipToPrev() })),
            ],
            ["⏮"],
          ),
          h.button(
            [
              h.Id("player-play-pause"),
              h.OnClick(CommandReceived({ command: PlayerCommandValue.TogglePause() })),
            ],
            [model.isPlaying ? "⏸" : "▶"],
          ),
          h.button(
            [
              h.Id("player-next"),
              h.Title("Next track"),
              h.Disabled(!nextOn),
              h.OnClick(CommandReceived({ command: PlayerCommandValue.SkipToNext() })),
            ],
            ["⏭"],
          ),
          // Seek + time: static until audio Subscription adds currentTime/duration.
          h.input([
            h.Id("player-seek"),
            h.Type("range"),
            h.Min("0"),
            h.Max("100"),
            h.Value("0"),
            h.Step("1"),
          ]),
          h.span([h.Id("player-time")], ["0:00 / 0:00"]),
        ],
      ),

      // ── Sidebar ───────────────────────────────────────────────────────
      h.aside(
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
      ),
    ],
  );
};
