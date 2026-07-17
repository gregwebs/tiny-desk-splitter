import { Effect } from "effect";
import { afterEach, describe, expect, test } from "vitest";

import { ToggleAudio } from "./command";
import { Acked, RejectedAudioPlay } from "./message";

describe("ToggleAudio", () => {
  afterEach(() => document.getElementById("player-audio")?.remove());

  test("two rapid toggles pause then resume from the media element's live state", async () => {
    const audio = document.createElement("audio");
    audio.id = "player-audio";
    let paused = false;
    const transitions: string[] = [];
    Object.defineProperty(audio, "paused", { get: () => paused });
    audio.pause = () => {
      transitions.push("pause");
      paused = true;
    };
    audio.play = () => {
      transitions.push("play");
      paused = false;
      return Promise.resolve();
    };
    document.body.appendChild(audio);

    expect(await Effect.runPromise(ToggleAudio().effect)).toEqual(Acked());
    expect(await Effect.runPromise(ToggleAudio().effect)).toEqual(Acked());

    expect(transitions).toEqual(["pause", "play"]);
    expect(audio.paused).toBe(false);
  });

  test("acknowledges a toggle when the media element is absent", async () => {
    document.getElementById("player-audio")?.remove();

    expect(await Effect.runPromise(ToggleAudio().effect)).toEqual(Acked());
  });

  test("reports rejected playback when resuming fails", async () => {
    const audio = document.createElement("audio");
    audio.id = "player-audio";
    Object.defineProperty(audio, "paused", { get: () => true });
    audio.play = () => Promise.reject(new Error("blocked"));
    document.body.appendChild(audio);

    expect(await Effect.runPromise(ToggleAudio().effect)).toEqual(RejectedAudioPlay());
  });
});
