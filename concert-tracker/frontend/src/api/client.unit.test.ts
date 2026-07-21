import { afterEach, describe, expect, test, vi } from "vitest";

import { ApiError, getJsonNullOn404 } from "./client";

// getJsonNullOn404 backs getNextTrackMediaInfoOrNull, which distinguishes the
// player's benign "no later playable track" 404 from a genuine server
// failure. This is the one seam that guards that mapping directly — the
// player's Story/Scene tests stub the client entirely, so they can't catch a
// future "simplify to getJsonOrNull" that would silently swallow a real 500
// into the same benign outcome as a 404 (see command.ts's FetchNextTrackInfo).
describe("getJsonNullOn404", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  test("returns null on a 404 response", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 404 })));

    await expect(getJsonNullOn404("/whatever")).resolves.toBeNull();
  });

  test("throws ApiError on a 500 response instead of returning null", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 500 })));

    await expect(getJsonNullOn404("/whatever")).rejects.toBeInstanceOf(ApiError);
  });

  test("returns the parsed JSON body on a 2xx response", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: true }), { status: 200 })),
    );

    await expect(getJsonNullOn404("/whatever")).resolves.toEqual({ ok: true });
  });
});
