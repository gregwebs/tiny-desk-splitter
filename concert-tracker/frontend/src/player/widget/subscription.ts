import { Effect, Option, Queue, Schema as S, Stream } from "effect";
import { Port, Subscription } from "foldkit";

import {
  clickShouldDismiss,
  isEditableTarget,
  isKeyboardShortcutIgnoredTarget,
  isPlainEscapeKey,
  isPlainSpaceKey,
  SIDEBAR_MIN_WIDTH,
  VIDEO_CONTROLS_IDLE_MS,
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
  UpdatedAudioTime,
} from "./message";
import type { Model } from "./model";
import { ports } from "./port";

// SUBSCRIPTION
//
// Eight subscription entries mirror the player.ts event-listener setup:
//   audioEvents      — play/pause/ended/error on the <audio> element
//   keyboard         — keydown → PressedSpace / PressedEscape
//   outsideVideo     — click outside #player-video-panel (gated on video.open)
//   htmxSettle       — htmx:afterSettle + historyRestore → SettledHtmxContent
//   htmxSwap         — htmx:afterSwap on like buttons → SwappedLikeButton
//   sidebarResize    — pointer drag on #sidebar-resize
//   videoControlsIdle — reveal/fade the minimize button (gated on video.open)
//   commandPort      — inbound Port.subscription for window.Player calls

// #player-audio, if it has a track loaded — mirrors the pre-Foldkit
// player.ts hasActiveMedia() exactly (currentSrc/src), not the model's
// playback.concertId, which is only subtly equivalent (e.g. it can lag a
// real load failure). Space with nothing loaded must fall through to
// native (page-scroll) behavior, per the old player.ts.
function activeMediaElement(): HTMLMediaElement | null {
  const audio = byIdOfOrNull("player-audio", HTMLMediaElement);
  return audio && (audio.currentSrc || audio.getAttribute("src")) ? audio : null;
}

/** Reveal the video minimize button (#player-video-close, via CSS keyed off
 *  controls-visible) on pointer activity over the panel, fading it back out
 *  after VIDEO_CONTROLS_IDLE_MS idle — ports the pre-Foldkit
 *  showVideoControls()/videoControlsTimer pair. Returns a cleanup that
 *  detaches both listeners, cancels any pending timer, and removes the
 *  class; the videoControlsIdle entry below only acquires this while
 *  video.open is true, so no runtime "is the panel open" guard is needed. */
export function attachVideoControlsIdle(panel: HTMLElement | null): () => void {
  if (!panel) return () => {};

  let timer: ReturnType<typeof setTimeout> | null = null;

  const onActivity = () => {
    panel.classList.add("controls-visible");
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => panel.classList.remove("controls-visible"), VIDEO_CONTROLS_IDLE_MS);
  };

  panel.addEventListener("mousemove", onActivity);
  panel.addEventListener("touchstart", onActivity, { passive: true });

  return () => {
    panel.removeEventListener("mousemove", onActivity);
    panel.removeEventListener("touchstart", onActivity);
    if (timer) clearTimeout(timer);
    panel.classList.remove("controls-visible");
  };
}

/** Read `audio`'s current position into an `UpdatedAudioTime` message tagged
 *  with the DOM-stamped `audioLoadGen`, or `None` while duration isn't known
 *  yet — mirrors player.ts's onTimeUpdate early-return guard (a freshly-
 *  loaded/reset element reports `duration: NaN` until metadata arrives).
 *  `loadGen` is read from `audio.dataset.audioLoadGen` — PlayAudio
 *  (command.ts) stamps it there in the same synchronous statement as
 *  `audio.src = url`, so it's ground truth for "which resource is actually
 *  loaded right now" at the moment this function runs, regardless of when
 *  the Subscription itself last (re)acquired relative to that Command's
 *  Effect — see model.ts's audioLoadGen doc comment for the race this closes
 *  that a same-track-identity or Subscription-timing check alone can't. */
export function audioTimeMessage(audio: HTMLMediaElement): Option.Option<Message> {
  if (!Number.isFinite(audio.duration) || audio.duration <= 0) return Option.none();
  const loadGen = Number(audio.dataset.audioLoadGen);
  return Option.some(
    UpdatedAudioTime({
      currentTime: audio.currentTime,
      duration: audio.duration,
      loadGen: Number.isFinite(loadGen) ? loadGen : -1,
    }),
  );
}

/** Extracted from a `htmx:afterSwap` event fired by a like button's
 *  `hx-post="/concerts/:id/tracks/:idx/like"` swap. Must be called
 *  synchronously from the DOM listener — see the `htmxSwap` subscription
 *  entry for why `evt.detail` can't be read later on the Effect fiber. */
export function parseLikeSwapEvent(
  evt: Event,
): Option.Option<{ concertId: number; trackIdx: number; liked: boolean }> {
  const detail: { elt?: Element } | undefined = evt instanceof CustomEvent ? evt.detail : undefined;
  const hxPost = detail?.elt?.getAttribute("hx-post");
  const m = hxPost?.match(/\/concerts\/(\d+)\/tracks\/(\d+)\/like/);
  if (!m) return Option.none();
  const concertId = parseInt(m[1]!, 10);
  const trackIdx = parseInt(m[2]!, 10);
  const lb = document.querySelector<HTMLElement>(`[hx-post="/concerts/${concertId}/tracks/${trackIdx}/like"]`);
  if (!lb) return Option.none();
  return Option.some({ concertId, trackIdx, liked: lb.classList.contains("liked") });
}

// ── Subscriptions ─────────────────────────────────────────────────────────

export const subscriptions = Subscription.make<Model, Message>()((entry) => ({
  // timeupdate/loadedmetadata staleness (a stray event from a just-replaced
  // track) is handled by audioTimeMessage reading the DOM-stamped
  // audioLoadGen (see its doc comment and command.ts's PlayAudio), not by
  // gating this Subscription's own acquisition — so unlike keyboard/
  // outsideVideo/videoControlsIdle below, it doesn't need dependencies keyed
  // on anything track-specific; `{}` is correct and this can stay one entry.
  audioEvents: entry(
    {},
    {
      modelToDependencies: () => ({}),
      dependenciesToStream: (): Stream.Stream<Message> => {
        const audio = byIdOfOrNull("player-audio", HTMLMediaElement);
        if (!audio) return Stream.empty;
        return Stream.mergeAll(
          [
            Stream.merge(
              Stream.fromEventListener(audio, "play").pipe(Stream.map(() => StartedAudio())),
              Stream.fromEventListener(audio, "pause").pipe(Stream.map(() => PausedAudio())),
            ),
            Stream.merge(
              Stream.fromEventListener(audio, "ended").pipe(Stream.map(() => EndedAudio())),
              Stream.fromEventListener(audio, "error").pipe(Stream.map(() => ErroredAudio())),
            ),
            Stream.merge(
              Stream.fromEventListener(audio, "timeupdate"),
              Stream.fromEventListener(audio, "loadedmetadata"),
            ).pipe(
              Stream.map(() => audioTimeMessage(audio)),
              Stream.filter(Option.isSome),
              Stream.map((opt) => opt.value),
            ),
          ],
          { concurrency: "unbounded" },
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

  // htmx reuses the event's detail object for its settle phase and reassigns
  // detail.elt right after dispatch (outerHTML swap: from the swapped-in
  // button to its parent), so parseLikeSwapEvent must run synchronously
  // inside the DOM listener below — a Stream.mapEffect stage runs later on
  // the Effect fiber and would see the already-mutated detail.
  htmxSwap: entry(
    {},
    {
      modelToDependencies: () => ({}),
      dependenciesToStream: () =>
        Stream.callback<Message>((queue) =>
          Effect.acquireRelease(
            Effect.sync(() => {
              const onAfterSwap = (evt: Event) =>
                Option.map(parseLikeSwapEvent(evt), (p) => Queue.offerUnsafe(queue, SwappedLikeButton(p)));
              document.body.addEventListener("htmx:afterSwap", onAfterSwap);
              return onAfterSwap;
            }),
            (onAfterSwap) => Effect.sync(() => document.body.removeEventListener("htmx:afterSwap", onAfterSwap)),
          ).pipe(Effect.flatMap(() => Effect.never)),
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

  // Gated on video.open, like outsideVideo/keyboard above: Foldkit tears
  // down and re-acquires the stream on dependency change, so listeners and
  // the idle timer only exist while the panel is open — no runtime
  // "is it open" guard needed the way the pre-Foldkit always-attached
  // listener required.
  videoControlsIdle: entry(
    { videoOpen: S.Boolean },
    {
      modelToDependencies: (model) => ({ videoOpen: model.video.open }),
      dependenciesToStream: ({ videoOpen }): Stream.Stream<Message> =>
        !videoOpen
          ? Stream.empty
          : Stream.callback<Message>(() =>
              Effect.acquireRelease(
                Effect.sync(() => attachVideoControlsIdle(byIdOrNull("player-video-panel"))),
                (cleanup) => Effect.sync(cleanup),
              ).pipe(Effect.flatMap(() => Effect.never)),
            ),
    },
  ),

  commandPort: Port.subscription(ports.inbound.command, (cmd) =>
    CommandReceived({ command: cmd }),
  ),
}));
