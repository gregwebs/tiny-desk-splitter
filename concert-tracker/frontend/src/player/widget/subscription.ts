import { Effect, Option, Queue, Schema as S, Stream } from "effect";
import { Port, Subscription } from "foldkit";

import {
  clickShouldDismiss,
  isEditableTarget,
  isKeyboardShortcutIgnoredTarget,
  isPlainEscapeKey,
  isPlainSpaceKey,
  SIDEBAR_MIN_WIDTH,
} from "../core";
import { byIdOfOrNull, byIdOrNull } from "../../shared/dom";
import {
  EndedAudio,
  ErroredAudio,
  PausedAudio,
  StartedAudio,
  ClickedOutsideVideo,
  CommandReceived,
  type Message,
  MovedSidebarDrag,
  PressedEscape,
  PressedSpace,
  SettledHtmxContent,
  ReleasedSidebarDrag,
  SwappedLikeButton,
} from "./message";
import type { Model } from "./model";
import { ports } from "./port";

// SUBSCRIPTION
//
// Six subscription entries mirror the player.ts event-listener setup:
//   audioEvents   — play/pause/ended/error on the <audio> element
//   keyboard      — keydown → PressedSpace / PressedEscape
//   outsideVideo  — click outside #player-video-panel (gated on video.open)
//   htmxSettle    — htmx:afterSettle + historyRestore → SettledHtmxContent
//   htmxSwap      — htmx:afterSwap on like buttons → SwappedLikeButton
//   commandPort   — inbound Port.subscription for window.Player calls
//
// Sidebar-resize and video-controls-idle subscriptions land in commit 8
// alongside the layout.html restructure that adds #sidebar-resize to the DOM.

// #player-audio, if it has a track loaded — mirrors the pre-Foldkit
// player.ts hasActiveMedia() exactly (currentSrc/src), not the model's
// playback.concertId, which is only subtly equivalent (e.g. it can lag a
// real load failure). Space with nothing loaded must fall through to
// native (page-scroll) behavior, per the old player.ts.
function activeMediaElement(): HTMLMediaElement | null {
  const audio = byIdOfOrNull("player-audio", HTMLMediaElement);
  return audio && (audio.currentSrc || audio.getAttribute("src")) ? audio : null;
}

// ── Subscriptions ─────────────────────────────────────────────────────────

export const subscriptions = Subscription.make<Model, Message>()((entry) => ({
  audioEvents: entry(
    {},
    {
      modelToDependencies: () => ({}),
      dependenciesToStream: (): Stream.Stream<Message> => {
        const audio = byIdOfOrNull("player-audio", HTMLMediaElement);
        if (!audio) return Stream.empty;
        return Stream.merge(
          Stream.merge(
            Stream.fromEventListener(audio, "play").pipe(Stream.map(() => StartedAudio())),
            Stream.fromEventListener(audio, "pause").pipe(Stream.map(() => PausedAudio())),
          ),
          Stream.merge(
            Stream.fromEventListener(audio, "ended").pipe(Stream.map(() => EndedAudio())),
            Stream.fromEventListener(audio, "error").pipe(Stream.map(() => ErroredAudio())),
          ),
        );
      },
    },
  ),

  keyboard: entry(
    { videoOpen: S.Boolean },
    {
      modelToDependencies: (model) => ({ videoOpen: model.video.open }),
      dependenciesToStream: ({ videoOpen }) =>
        Stream.fromEventListener<KeyboardEvent>(document, "keydown").pipe(
          Stream.mapEffect((e) =>
            Effect.sync((): Option.Option<Message> => {
              // An earlier handler (e.g. a future OnKeyDownPreventDefault in
              // view.ts) already claimed this key; don't double-handle it.
              if (e.defaultPrevented) return Option.none();
              // Escape only folds the video panel, so only claim it (and
              // suppress native Escape) while the panel is open — otherwise
              // native Escape (e.g. clearing a native browser field) wins.
              if (isPlainEscapeKey(e) && !isEditableTarget(e.target) && videoOpen) {
                e.preventDefault();
                return Option.some(PressedEscape());
              }
              if (isPlainSpaceKey(e) && !isKeyboardShortcutIgnoredTarget(e.target)) {
                // Nothing loaded: let Space fall through to native page-scroll.
                const audio = activeMediaElement();
                if (!audio) return Option.none();
                // preventDefault before the repeat check: a held Space must
                // still suppress page-scroll on every repeat keydown, even
                // though only the first one toggles playback.
                e.preventDefault();
                if (e.repeat) return Option.none();
                // audio.paused is read live, not from model.isPlaying: that
                // field only updates from the audio element's async
                // play/pause events, so two Space presses in quick
                // succession could otherwise both see a stale isPlaying and
                // both dispatch PauseAudio (see PressedSpace's doc comment).
                return Option.some(PressedSpace({ audioPaused: audio.paused }));
              }
              return Option.none();
            }),
          ),
          Stream.filter(Option.isSome),
          Stream.map((opt) => opt.value),
        ),
    },
  ),

  outsideVideo: entry(
    { videoOpen: S.Boolean },
    {
      modelToDependencies: (model) => ({ videoOpen: model.video.open }),
      dependenciesToStream: ({ videoOpen }) =>
        Stream.when(
          Stream.fromEventListener<MouseEvent>(document, "click").pipe(
            Stream.mapEffect((e) =>
              Effect.sync((): Option.Option<Message> => {
                const panel = byIdOrNull("player-video-panel");
                return clickShouldDismiss(e.target, panel)
                  ? Option.some(ClickedOutsideVideo())
                  : Option.none();
              }),
            ),
            Stream.filter(Option.isSome),
            Stream.map((opt) => opt.value),
          ),
          Effect.sync(() => videoOpen),
        ),
    },
  ),

  htmxSettle: entry(
    {},
    {
      modelToDependencies: () => ({}),
      dependenciesToStream: () =>
        Stream.merge(
          Stream.fromEventListener(document.body, "htmx:afterSettle"),
          Stream.fromEventListener(document.body, "htmx:historyRestore"),
        ).pipe(Stream.map(() => SettledHtmxContent())),
    },
  ),

  htmxSwap: entry(
    {},
    {
      modelToDependencies: () => ({}),
      dependenciesToStream: () =>
        Stream.fromEventListener<Event>(document.body, "htmx:afterSwap").pipe(
          Stream.mapEffect((evt) =>
            Effect.sync((): Option.Option<Message> => {
              const detail: { elt?: Element } | undefined = evt instanceof CustomEvent ? evt.detail : undefined;
              const hxPost = detail?.elt?.getAttribute("hx-post");
              const m = hxPost?.match(/\/concerts\/(\d+)\/tracks\/(\d+)\/like/);
              if (!m) return Option.none();
              const concertId = parseInt(m[1]!, 10);
              const trackIdx = parseInt(m[2]!, 10);
              const lb = document.querySelector<HTMLElement>(
                `[hx-post="/concerts/${concertId}/tracks/${trackIdx}/like"]`,
              );
              if (!lb) return Option.none();
              return Option.some(
                SwappedLikeButton({ concertId, trackIdx, liked: lb.classList.contains("liked") }),
              );
            }),
          ),
          Stream.filter(Option.isSome),
          Stream.map((opt) => opt.value),
        ),
    },
  ),

  // Sidebar drag-to-resize. Wraps the original initSidebarResize() imperative
  // logic in Stream.callback: pointer capture and body-class mutations happen
  // inline (no model round-trip needed), while MovedSidebarDrag / ReleasedSidebarDrag
  // are emitted so update.ts can dispatch SetSidebarWidthVar / PersistSidebarWidth.
  sidebarResize: entry(
    {},
    {
      modelToDependencies: () => ({}),
      dependenciesToStream: () =>
        Stream.callback<Message>((queue) =>
          Effect.acquireRelease(
            Effect.sync(() => {
              const handle = byIdOrNull("sidebar-resize");
              if (!handle) return () => {};

              let dragging = false;
              let moved = false;

              const onDown = (e: PointerEvent) => {
                dragging = true;
                moved = false;
                // Seed from computed width so a bare click never persists 0.
                handle.setPointerCapture(e.pointerId);
                e.preventDefault();
                document.body.classList.add("sidebar-resizing");
              };

              const onMove = (e: PointerEvent) => {
                if (!dragging) return;
                moved = true;
                // Sidebar is position:fixed; left:0 so clientX = desired width.
                Queue.offerUnsafe(queue, MovedSidebarDrag({ clientX: Math.round(e.clientX) }));
              };

              const onEnd = (e: PointerEvent) => {
                if (!dragging) return;
                dragging = false;
                document.body.classList.remove("sidebar-resizing");
                const clientX = Math.round(
                  e.clientX ||
                    parseInt(
                      getComputedStyle(document.documentElement).getPropertyValue("--sidebar-width"),
                      10,
                    ) ||
                    SIDEBAR_MIN_WIDTH,
                );
                Queue.offerUnsafe(queue, ReleasedSidebarDrag({ clientX, moved }));
                moved = false;
              };

              handle.addEventListener("pointerdown", onDown);
              document.addEventListener("pointermove", onMove);
              document.addEventListener("pointerup", onEnd);
              document.addEventListener("pointercancel", onEnd);

              return () => {
                handle.removeEventListener("pointerdown", onDown);
                document.removeEventListener("pointermove", onMove);
                document.removeEventListener("pointerup", onEnd);
                document.removeEventListener("pointercancel", onEnd);
              };
            }),
            (cleanup) => Effect.sync(cleanup),
          ).pipe(Effect.flatMap(() => Effect.never)),
        ),
    },
  ),

  commandPort: Port.subscription(ports.inbound.command, (cmd) =>
    CommandReceived({ command: cmd }),
  ),
}));
