import { Effect } from "effect";
import { describe, expect, test } from "vitest";

import { HideVideoPanel } from "./command";
import { Acked } from "./message";

// Direct Command-effect test for HideVideoPanel's DOM-dependent branching —
// Story/Scene never run a Command's real Effect body. Complements the
// videoControlsIdle unit tests (subscription.unit.test.ts), which cover
// controls-visible being added/removed via the idle timer; this file
// verifies HideVideoPanel's own removal path (parity with the pre-Foldkit
// hideVideoPanel(), which cleared controls-visible on every close).
describe("HideVideoPanel", () => {
  test("removes both open and controls-visible from #player-video-panel", async () => {
    document.getElementById("player-video-panel")?.remove();
    const panel = document.createElement("div");
    panel.id = "player-video-panel";
    panel.classList.add("open", "controls-visible");
    document.body.appendChild(panel);

    const result = await Effect.runPromise(HideVideoPanel().effect);

    expect(result).toEqual(Acked());
    expect(panel.classList.contains("open")).toBe(false);
    expect(panel.classList.contains("controls-visible")).toBe(false);

    panel.remove();
  });

  test("does not throw when #player-video-panel is absent", async () => {
    document.getElementById("player-video-panel")?.remove();

    const result = await Effect.runPromise(HideVideoPanel().effect);

    expect(result).toEqual(Acked());
  });
});
