import { Effect } from "effect";
import { describe, expect, test } from "vitest";

import { PlayAudio } from "./command";
import { Acked, AudioPlayRejected } from "./message";

// Direct Command-effect tests: Story/Scene never run a Command's real Effect
// body (they intercept and resolve Commands abstractly), so a defect that
// only shows up inside the Effect itself — like a throw bypassing
// Effect.catch — needs to run for real against happy-dom. Complements
// update.story.test.ts, which covers the Message-level AudioPlayRejected
// handling once the Command has already resolved.
describe("PlayAudio", () => {
  test("resolves to AudioPlayRejected instead of throwing when #player-audio is absent", async () => {
    document.getElementById("player-audio")?.remove();

    const result = await Effect.runPromise(PlayAudio({ url: "https://example.com/a.mp3" }).effect);

    expect(result).toEqual(AudioPlayRejected());
  });

  test("plays and resolves to Acked when #player-audio is present", async () => {
    document.getElementById("player-audio")?.remove();
    const audio = document.createElement("audio");
    audio.id = "player-audio";
    audio.play = () => Promise.resolve();
    document.body.appendChild(audio);

    const result = await Effect.runPromise(PlayAudio({ url: "https://example.com/a.mp3" }).effect);

    expect(result).toEqual(Acked());
    expect(audio.src).toBe("https://example.com/a.mp3");

    audio.remove();
  });
});
