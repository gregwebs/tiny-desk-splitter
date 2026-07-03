import { describe, expect, test } from "vitest";

import {
  isEditableTarget,
  isKeyboardShortcutIgnoredTarget,
  isPlayerPlaybackShortcutTarget,
} from "./core";

// Builds the DOM shape the keyboard-target predicates walk with closest():
// #player-bar (a non-editable button/span, plus an editable #player-seek
// input), #player-video-panel (containing #player-audio), a contenteditable
// island (including a non-editable child), and interactive/plain elements
// outside the player entirely.
function buildFixture(): {
  barButton: HTMLElement;
  barSpan: HTMLElement;
  seekInput: HTMLElement;
  videoAudio: HTMLElement;
  editableDiv: HTMLElement;
  editableChildText: HTMLElement;
  editableFalseIsland: HTMLElement;
  outsideButton: HTMLElement;
  outsideDiv: HTMLElement;
} {
  document.body.innerHTML = `
    <div id="player-bar">
      <button id="bar-button">Toggle</button>
      <span id="bar-span" role="button" tabindex="0"></span>
      <input id="player-seek" type="range" />
    </div>
    <div id="player-video-panel">
      <video id="player-audio" onclick="Player.togglePause()"></video>
    </div>
    <div id="editable-div" contenteditable="true">
      <span id="editable-child-text">notes</span>
      <div id="editable-false-island" contenteditable="false"></div>
    </div>
    <button id="outside-button">Listen</button>
    <div id="outside-div"></div>
  `;
  return {
    barButton: document.getElementById("bar-button")!,
    barSpan: document.getElementById("bar-span")!,
    seekInput: document.getElementById("player-seek")!,
    videoAudio: document.getElementById("player-audio")!,
    editableDiv: document.getElementById("editable-div")!,
    editableChildText: document.getElementById("editable-child-text")!,
    editableFalseIsland: document.getElementById("editable-false-island")!,
    outsideButton: document.getElementById("outside-button")!,
    outsideDiv: document.getElementById("outside-div")!,
  };
}

describe("isEditableTarget", () => {
  test("is true for an input inside the player bar", () => {
    const { seekInput } = buildFixture();
    expect(isEditableTarget(seekInput)).toBe(true);
  });

  test("is true for a contenteditable element", () => {
    const { editableDiv } = buildFixture();
    expect(isEditableTarget(editableDiv)).toBe(true);
  });

  test("is true for a child of a contenteditable element", () => {
    const { editableChildText } = buildFixture();
    expect(isEditableTarget(editableChildText)).toBe(true);
  });

  test("is false for a contenteditable=false island, even nested in an editable ancestor", () => {
    const { editableFalseIsland } = buildFixture();
    expect(isEditableTarget(editableFalseIsland)).toBe(false);
  });

  test("is false for a plain button or div", () => {
    const { barButton, outsideDiv } = buildFixture();
    expect(isEditableTarget(barButton)).toBe(false);
    expect(isEditableTarget(outsideDiv)).toBe(false);
  });

  test("is false for non-Element targets", () => {
    expect(isEditableTarget(null)).toBe(false);
    expect(isEditableTarget(document)).toBe(false);
  });
});

describe("isPlayerPlaybackShortcutTarget", () => {
  test("is true for a non-editable element inside #player-bar", () => {
    const { barButton, barSpan } = buildFixture();
    expect(isPlayerPlaybackShortcutTarget(barButton)).toBe(true);
    expect(isPlayerPlaybackShortcutTarget(barSpan)).toBe(true);
  });

  test("is true for #player-audio inside #player-video-panel", () => {
    const { videoAudio } = buildFixture();
    expect(isPlayerPlaybackShortcutTarget(videoAudio)).toBe(true);
  });

  test("is false for an editable element inside #player-bar (e.g. #player-seek)", () => {
    const { seekInput } = buildFixture();
    expect(isPlayerPlaybackShortcutTarget(seekInput)).toBe(false);
  });

  test("is false outside the bar and video panel", () => {
    const { outsideButton, outsideDiv } = buildFixture();
    expect(isPlayerPlaybackShortcutTarget(outsideButton)).toBe(false);
    expect(isPlayerPlaybackShortcutTarget(outsideDiv)).toBe(false);
  });

  test("is false for non-Element targets", () => {
    expect(isPlayerPlaybackShortcutTarget(null)).toBe(false);
    expect(isPlayerPlaybackShortcutTarget(document)).toBe(false);
  });
});

describe("isKeyboardShortcutIgnoredTarget", () => {
  test("is false (not ignored) for non-editable targets inside #player-bar or #player-video-panel", () => {
    const { barButton, barSpan, videoAudio } = buildFixture();
    expect(isKeyboardShortcutIgnoredTarget(barButton)).toBe(false);
    expect(isKeyboardShortcutIgnoredTarget(barSpan)).toBe(false);
    expect(isKeyboardShortcutIgnoredTarget(videoAudio)).toBe(false);
  });

  test("is true for #player-seek (editable, inside the bar)", () => {
    const { seekInput } = buildFixture();
    expect(isKeyboardShortcutIgnoredTarget(seekInput)).toBe(true);
  });

  test("is true for a contenteditable target outside the bar", () => {
    const { editableDiv } = buildFixture();
    expect(isKeyboardShortcutIgnoredTarget(editableDiv)).toBe(true);
  });

  test("is true for an interactive control outside the bar (INTERACTIVE_SELECTOR)", () => {
    const { outsideButton } = buildFixture();
    expect(isKeyboardShortcutIgnoredTarget(outsideButton)).toBe(true);
  });

  test("is false for a plain, non-interactive element outside the bar", () => {
    const { outsideDiv } = buildFixture();
    expect(isKeyboardShortcutIgnoredTarget(outsideDiv)).toBe(false);
  });

  test("is false for non-Element targets", () => {
    expect(isKeyboardShortcutIgnoredTarget(null)).toBe(false);
    expect(isKeyboardShortcutIgnoredTarget(document)).toBe(false);
  });
});
