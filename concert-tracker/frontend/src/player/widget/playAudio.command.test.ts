import { Effect } from "effect";
import { describe, expect, test } from "vitest";

import { PlayAudio } from "./command";
import { Acked, RejectedAudioPlay } from "./message";

// Direct Command-effect tests: Story/Scene never run a Command's real Effect
// body (they intercept and resolve Commands abstractly), so a defect that
// only shows up inside the Effect itself — like a throw bypassing
// Effect.catch — needs to run for real against happy-dom. Complements
// update.story.test.ts, which covers the Message-level RejectedAudioPlay
// handling once the Command has already resolved.
describe("PlayAudio", () => {
  test("resolves to RejectedAudioPlay instead of throwing when #player-audio is absent", async () => {
    document.getElementById("player-audio")?.remove();

    const result = await Effect.runPromise(PlayAudio({ url: "https://example.com/a.mp3", loadGen: 1 }).effect);

    expect(result).toEqual(RejectedAudioPlay());
  });

  test("plays and resolves to Acked when #player-audio is present", async () => {
    document.getElementById("player-audio")?.remove();
    const audio = document.createElement("audio");
    audio.id = "player-audio";
    audio.play = () => Promise.resolve();
    document.body.appendChild(audio);

    const result = await Effect.runPromise(PlayAudio({ url: "https://example.com/a.mp3", loadGen: 1 }).effect);

    expect(result).toEqual(Acked());
    expect(audio.src).toBe("https://example.com/a.mp3");

    audio.remove();
  });

  test("stamps the given loadGen onto the element's dataset before play() is called", async () => {
    document.getElementById("player-audio")?.remove();
    const audio = document.createElement("audio");
    audio.id = "player-audio";
    // Assert at the moment play() fires, not just after the Command
    // resolves — proves src/dataset are already both set by then, not just
    // eventually set by the time the whole Effect completes.
    let srcAtPlayCall: string | undefined;
    let loadGenAtPlayCall: string | undefined;
    audio.play = () => {
      srcAtPlayCall = audio.src;
      loadGenAtPlayCall = audio.dataset.audioLoadGen;
      return Promise.resolve();
    };
    document.body.appendChild(audio);

    await Effect.runPromise(PlayAudio({ url: "https://example.com/a.mp3", loadGen: 7 }).effect);

    expect(srcAtPlayCall).toBe("https://example.com/a.mp3");
    expect(loadGenAtPlayCall).toBe("7");

    audio.remove();
  });

  test("a same-URL replay still overwrites the DOM-stamped loadGen to the new value", async () => {
    document.getElementById("player-audio")?.remove();
    const audio = document.createElement("audio");
    audio.id = "player-audio";
    audio.src = "https://example.com/a.mp3";
    audio.dataset.audioLoadGen = "3";
    let loadGenAtPlayCall: string | undefined;
    audio.play = () => {
      loadGenAtPlayCall = audio.dataset.audioLoadGen;
      return Promise.resolve();
    };
    document.body.appendChild(audio);

    await Effect.runPromise(PlayAudio({ url: "https://example.com/a.mp3", loadGen: 4 }).effect);

    expect(loadGenAtPlayCall).toBe("4");
    expect(audio.dataset.audioLoadGen).toBe("4");

    audio.remove();
  });
});
