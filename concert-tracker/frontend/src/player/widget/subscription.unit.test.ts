import { Option } from "effect";
import { afterEach, describe, expect, test, vi } from "vitest";

import { audioTimeMessage, attachVideoControlsIdle, parseLikeSwapEvent } from "./subscription";
import { VIDEO_CONTROLS_IDLE_MS } from "../core";
import { UpdatedAudioTime } from "./message";

// A real <audio> element with `.duration` overridden: happy-dom's `duration`
// is a spec-readonly getter that silently ignores direct assignment, so
// defineProperty is needed to simulate a loaded/unloaded element.
// `loadGen` mirrors what PlayAudio's Effect stamps onto dataset.audioLoadGen.
function mediaElement(duration: number, currentTime: number, loadGen?: number): HTMLMediaElement {
  const audio = document.createElement("audio");
  Object.defineProperty(audio, "duration", { value: duration, configurable: true });
  audio.currentTime = currentTime;
  if (loadGen !== undefined) audio.dataset.audioLoadGen = String(loadGen);
  return audio;
}

// parseLikeSwapEvent must extract everything it needs synchronously from
// evt.detail.elt (see the htmxSwap subscription entry's comment): htmx
// reassigns detail.elt right after dispatch, so this can't defer to a Stream
// stage. These tests exercise the parse logic directly against a real DOM
// (happy-dom), independent of that timing constraint.
describe("parseLikeSwapEvent", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  function likeButton(concertId: number, trackIdx: number, liked: boolean): HTMLElement {
    const btn = document.createElement("button");
    btn.setAttribute("hx-post", `/concerts/${concertId}/tracks/${trackIdx}/like`);
    btn.className = liked ? "btn-like liked" : "btn-like";
    document.body.appendChild(btn);
    return btn;
  }

  test("extracts concertId/trackIdx/liked from a swapped-in like button", () => {
    const btn = likeButton(1, 0, true);
    const evt = new CustomEvent("htmx:afterSwap", { detail: { elt: btn } });

    expect(parseLikeSwapEvent(evt)).toEqual(Option.some({ concertId: 1, trackIdx: 0, liked: true }));
  });

  test("reflects the unliked state from the swapped-in button's class", () => {
    const btn = likeButton(2, 3, false);
    const evt = new CustomEvent("htmx:afterSwap", { detail: { elt: btn } });

    expect(parseLikeSwapEvent(evt)).toEqual(Option.some({ concertId: 2, trackIdx: 3, liked: false }));
  });

  test("returns none when detail.elt has no hx-post (e.g. reassigned to a parent)", () => {
    const parent = document.createElement("li");
    document.body.appendChild(parent);
    const evt = new CustomEvent("htmx:afterSwap", { detail: { elt: parent } });

    expect(parseLikeSwapEvent(evt)).toEqual(Option.none());
  });

  test("returns none when hx-post doesn't match the like-endpoint shape", () => {
    const btn = document.createElement("button");
    btn.setAttribute("hx-post", "/concerts/1/tracks/0/delete");
    document.body.appendChild(btn);
    const evt = new CustomEvent("htmx:afterSwap", { detail: { elt: btn } });

    expect(parseLikeSwapEvent(evt)).toEqual(Option.none());
  });

  test("returns none for a plain Event (no detail)", () => {
    expect(parseLikeSwapEvent(new Event("htmx:afterSwap"))).toEqual(Option.none());
  });
});

// Mirrors player.ts's onTimeUpdate early-return guard: no message until
// duration is known, matching the pre-Foldkit `<audio>` element's real
// behavior of reporting NaN until loadedmetadata fires.
describe("audioTimeMessage", () => {
  test("returns None while duration is NaN (not yet loaded)", () => {
    expect(audioTimeMessage(mediaElement(NaN, 0))).toEqual(Option.none());
  });

  test("returns None while duration is 0", () => {
    expect(audioTimeMessage(mediaElement(0, 0))).toEqual(Option.none());
  });

  test("returns Some(UpdatedAudioTime) once duration is finite and positive, tagged with the element's DOM-stamped loadGen", () => {
    expect(audioTimeMessage(mediaElement(180, 42.5, 3))).toEqual(
      Option.some(UpdatedAudioTime({ currentTime: 42.5, duration: 180, loadGen: 3 })),
    );
  });

  // -1 can never equal a real model.audioLoadGen (starts at 0, only
  // increments), so an element PlayAudio has never touched is correctly
  // rejected by the reducer's comparison rather than coincidentally
  // matching on undefined === undefined or similar.
  test("loadGen is -1 when PlayAudio has never stamped the element", () => {
    expect(audioTimeMessage(mediaElement(180, 0))).toEqual(
      Option.some(UpdatedAudioTime({ currentTime: 0, duration: 180, loadGen: -1 })),
    );
  });
});

// Ports the pre-Foldkit showVideoControls()/hideVideoPanel() idle-timer pair
// as a directly testable function (the videoControlsIdle subscription entry
// is thin acquire/release glue over this, left to e2e coverage — same as
// sidebarResize). Fake timers give real time control without mocking the
// function under test.
describe("attachVideoControlsIdle", () => {
  afterEach(() => {
    vi.useRealTimers();
    document.body.innerHTML = "";
  });

  function videoPanel(): HTMLElement {
    const panel = document.createElement("div");
    panel.id = "player-video-panel";
    document.body.appendChild(panel);
    return panel;
  }

  test("mousemove adds controls-visible", () => {
    const panel = videoPanel();
    attachVideoControlsIdle(panel);

    panel.dispatchEvent(new MouseEvent("mousemove"));

    expect(panel.classList.contains("controls-visible")).toBe(true);
  });

  test("touchstart also reveals controls", () => {
    const panel = videoPanel();
    attachVideoControlsIdle(panel);

    panel.dispatchEvent(new Event("touchstart"));

    expect(panel.classList.contains("controls-visible")).toBe(true);
  });

  test("removes controls-visible after the idle timeout", () => {
    vi.useFakeTimers();
    const panel = videoPanel();
    attachVideoControlsIdle(panel);

    panel.dispatchEvent(new MouseEvent("mousemove"));
    vi.advanceTimersByTime(VIDEO_CONTROLS_IDLE_MS);

    expect(panel.classList.contains("controls-visible")).toBe(false);
  });

  test("activity partway through restarts the idle window", () => {
    vi.useFakeTimers();
    const panel = videoPanel();
    attachVideoControlsIdle(panel);

    panel.dispatchEvent(new MouseEvent("mousemove"));
    vi.advanceTimersByTime(VIDEO_CONTROLS_IDLE_MS - 500);
    panel.dispatchEvent(new MouseEvent("mousemove")); // restarts the window
    vi.advanceTimersByTime(600);
    expect(panel.classList.contains("controls-visible")).toBe(true);

    vi.advanceTimersByTime(VIDEO_CONTROLS_IDLE_MS);
    expect(panel.classList.contains("controls-visible")).toBe(false);
  });

  test("cleanup removes listeners and the class", () => {
    vi.useFakeTimers();
    const panel = videoPanel();
    const cleanup = attachVideoControlsIdle(panel);

    panel.dispatchEvent(new MouseEvent("mousemove"));
    expect(panel.classList.contains("controls-visible")).toBe(true);

    cleanup();
    expect(panel.classList.contains("controls-visible")).toBe(false);

    // A mousemove after cleanup must not re-add the class (listeners are gone).
    panel.dispatchEvent(new MouseEvent("mousemove"));
    expect(panel.classList.contains("controls-visible")).toBe(false);
  });

  test("cleanup cancels the pending timer (a leaked timer would resurrect the class)", () => {
    vi.useFakeTimers();
    const panel = videoPanel();
    const cleanup = attachVideoControlsIdle(panel);

    panel.dispatchEvent(new MouseEvent("mousemove"));
    cleanup();
    // Simulate a fresh reveal (e.g. the panel reopening) right after cleanup —
    // a leaked timer from the cleaned-up session would fire and remove this
    // unrelated class, since classList.remove doesn't care which "session"
    // added it. Asserting the class survives past the old timer's deadline
    // proves the timer was actually cancelled, not just that cleanup() itself
    // removed the class once (which the old, weaker test couldn't rule out).
    panel.classList.add("controls-visible");

    vi.advanceTimersByTime(VIDEO_CONTROLS_IDLE_MS);

    expect(panel.classList.contains("controls-visible")).toBe(true);
  });

  test("is a no-op for a null panel", () => {
    expect(() => attachVideoControlsIdle(null)()).not.toThrow();
  });
});
