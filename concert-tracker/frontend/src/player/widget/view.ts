import { Option } from "effect";
import { type Html, html } from "foldkit/html";

import { nextEnabled, prevEnabled } from "../core";
import { CommandReceived, type Message } from "./message";
import type { Model } from "./model";
import { PlayerCommandValue } from "./port";

// VIEW — player-bar section (commit 2).
// Sidebar sections (queue list, concert track list) render from this same view
// function once commits 5/6 fill them in.  Host embed wiring + layout.html
// restructure land in commit 7.

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
    [h.Id("player-bar")],
    [
      // ── Queue/sidebar toggle ──────────────────────────────────────────
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

      // ── Info: title-line + artist + playlist ─────────────────────────
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
              // TODO(keyboard): the original layout.html:53-54 had onkeydown handlers
              // for Enter on these spans so they behave like buttons.  core.ts
              // has isPlainSpaceKey/isPlainEscapeKey but no Enter predicate, and
              // message.ts lists keyboard shortcuts as out-of-scope for commit 2.
              // A later Subscription commit should add h.OnKeyDown for Enter here.
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
          // Concert navigation: htmx handles the routing via hx-* attributes.
          // No h.OnClick here — the host shim (commit 7) intercepts modifier
          // clicks on this element before emitting an OpenConcert command.
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

      // ── Status / error feedback ───────────────────────────────────────
      h.span([h.Id("player-error")], [errorText]),
      h.span([h.Id("player-status")], [busyText]),

      // ── Action buttons (visibility gated on current media type) ───────
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

      // ── Transport controls ────────────────────────────────────────────
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

      // Seek slider and time display: static at 0 until audio Subscription
      // adds currentTime/duration to the Model (later commit).
      // TODO(seek): original layout.html:70 had oninput="Player.seek(this.value)";
      // hook it up when currentTime/duration enter the Model.
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
  );
};
