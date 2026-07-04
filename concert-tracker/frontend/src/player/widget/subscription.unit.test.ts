import { Option } from "effect";
import { afterEach, describe, expect, test } from "vitest";

import { parseLikeSwapEvent } from "./subscription";

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
